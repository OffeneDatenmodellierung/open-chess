// Chess lab: the app shell. Game rules and move explanations run in the
// Rust engine (wasm) on the main thread; search (bot moves, training
// analysis, endgame tables) runs in two workers so the board never freezes.

import init, { Session, checkFen, completeFen, glossary, motifs, openingBook } from "./pkg/open_chess_wasm.js";
import { Board, GLYPH } from "./board.js";
import { PositionEditor } from "./position-editor.js";
import { askPhotoTerms, startReview } from "./review.js";
import { Api, DEVICE_TOKEN, SERVERS, beginSignIn, finishSignIn, lichessTablebase, loadSiteName } from "./online.js";

const $ = (id) => document.getElementById(id);
const store = {
  get(k, d) {
    try {
      const v = localStorage.getItem("esl-chess:" + k);
      return v === null ? d : JSON.parse(v);
    } catch {
      return d;
    }
  },
  set(k, v) {
    try {
      localStorage.setItem("esl-chess:" + k, JSON.stringify(v));
    } catch {}
  },
  del(k) {
    try {
      localStorage.removeItem("esl-chess:" + k);
    } catch {}
  },
};

const START_PLACEMENT = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR";

// "normal" must match the server's CHESS_FAIRPLAY_APP_DEPTH / _NODES, so
// the fair-play fingerprint compares against what players actually saw.
const SEARCH = {
  quick: { depth: 4, nodes: 80_000 },
  normal: { depth: 6, nodes: 400_000 },
  deep: { depth: 9, nodes: 3_000_000 },
  max: { depth: 64, nodes: 2_000_000_000 }, // limited by thinking time
};

// ---------- engine workers

class Engine {
  constructor() {
    this.worker = new Worker(new URL("./engine-worker.js", import.meta.url), { type: "module" });
    this.next = 1;
    this.waiting = new Map();
    this.worker.onmessage = ({ data }) => {
      const p = this.waiting.get(data.id);
      this.waiting.delete(data.id);
      if (!p) return;
      data.error ? p.reject(new Error(data.error)) : p.resolve(data.result);
    };
  }
  run(op, game, args = {}) {
    const id = this.next++;
    return new Promise((resolve, reject) => {
      this.waiting.set(id, { resolve, reject });
      this.worker.postMessage({ id, op, fen: game.fen, moves: game.moves, args });
    });
  }
}

/// The on-device board reader (vision-worker.js), started on the first
/// photo so its weights aren't loaded until someone needs them.
class Vision {
  constructor() {
    this.worker = new Worker(new URL("./vision-worker.js", import.meta.url), { type: "module" });
    this.next = 1;
    this.waiting = new Map();
    this.worker.onmessage = ({ data }) => {
      const p = this.waiting.get(data.id);
      this.waiting.delete(data.id);
      if (!p) return;
      data.error ? p.reject(new Error(data.error)) : p.resolve(data.result);
    };
    // If the worker can't even start (its wasm missing), fail what's
    // waiting, and later calls, rather than leave them hanging.
    this.broken = null;
    this.worker.onerror = (e) => {
      this.broken = new Error(e.message || "the board reader couldn't start");
      for (const p of this.waiting.values()) p.reject(this.broken);
      this.waiting.clear();
    };
  }
  run(op, extra = {}, transfer = []) {
    if (this.broken) return Promise.reject(this.broken);
    const id = this.next++;
    return new Promise((resolve, reject) => {
      this.waiting.set(id, { resolve, reject });
      this.worker.postMessage({ id, op, ...extra }, transfer);
    });
  }
}
let vision = null;

// ---------- state

const settings = {
  level: store.get("level", 3),
  thinkSecs: store.get("thinkSecs", 5),
  side: store.get("side", "white"),
  depth: store.get("depth", "normal"),
  plies: store.get("plies", 10),
  train: store.get("train", true),
  siteBase: store.get("siteBase", SERVERS.site.base),
};
const siteLoaded = loadSiteName(settings.siteBase);

const app = {
  mode: store.get("mode", "bot"),
  session: null, // the whole game
  view: null, // the position being looked at, when stepping back
  viewPly: null, // null: the latest position
  st: null, // Session.state() of the whole game
  vst: null, // Session.state() of the viewed position
  legal: [], // legal moves in the viewed position
  hist: [], // per ply: {san, text, patterns}
  userWhite: true, // bot mode: which side the user plays
  analysis: null,
  analysisFor: "",
  tablebase: null, // {for, text, wdl, best}
  dictionary: null, // {for, entry}
  thinking: false,
  selected: null,
  editing: null, // {grid: {sq: char}, white, unsure: Set, pick}
  online: {
    serverKey: store.get("server", "lichess"),
    api: null,
    me: null,
    challenges: new Map(),
    game: null, // {id, white, black, myWhite, wtime, btime, at, status, winner, drawOffer}
    eventsAbort: null,
    gameAbort: null,
    error: "",
    assistSent: null,
    device: null, // a board device's settings (GET /api/device)
  },
};

let board;
let analysisEngine;
let botEngine;

// ---------- helpers

const pct = (x) => `${Math.round(x * 100)}%`;
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
const FILES = "abcdefgh";

function scoreText(score) {
  if (!score) return "";
  if (score.kind === "mate") return score.moves > 0 ? `White mates in ${score.moves}` : `Black mates in ${-score.moves}`;
  const v = score.value / 100;
  return `${v > 0 ? "+" : ""}${v.toFixed(2)}`;
}

function cur() {
  return app.viewPly === null ? app.session : app.view;
}

function viewKey() {
  return { fen: app.vst.initial_fen, moves: app.vst.uci.join(" ") };
}

function atLatest() {
  return app.viewPly === null;
}

function onlineLive() {
  return app.mode === "online" && app.online.game && app.online.game.status === "started";
}

/// Engine help (odds, lines, move explanations' patterns, tablebases).
/// Off during live Lichess games (their rules); switchable on our own
/// server, where switching it on is declared to the server.
function helpAllowed() {
  if (app.mode !== "online") return true;
  if (!app.online.game) return false;
  if (!onlineLive()) return true;
  return app.online.serverKey === "site";
}

function helpOn() {
  return settings.train && helpAllowed();
}

/// Playing on a board device: moves are made on the board, the app shows them.
function onDevice() {
  return app.mode === "online" && !!SERVERS[app.online.serverKey].device;
}

function canMove() {
  if (app.editing || !app.vst || app.vst.status !== "started") return false;
  if (app.mode === "local" || app.mode === "analyse") return true;
  if (!atLatest()) return false;
  if (onDevice()) return false;
  if (app.mode === "bot") return !app.thinking && (app.st.turn === "white") === app.userWhite;
  const g = app.online.game;
  return !!g && g.status === "started" && (app.st.turn === "white") === g.myWhite;
}

function patternChips(patterns) {
  if (!patterns?.length) return "";
  return `<div class="chips">${patterns
    .map((p) => `<details class="chip ${p.motif}"><summary>${esc(p.name)}</summary><p>${esc(p.detail)}.</p><p class="def">${esc(p.definition)}</p></details>`)
    .join("")}</div>`;
}

function pieceCount(fen) {
  return (fen.split(" ")[0].match(/[a-zA-Z]/g) || []).length;
}

// ---------- game lifecycle

function sessionFor(fen) {
  return new Session(fen && fen.split(" ")[0] !== START_PLACEMENT ? fen : null);
}

function replayHistory(fen, uci) {
  const s = sessionFor(fen);
  const hist = [];
  for (const m of uci) {
    try {
      const info = JSON.parse(s.play(m));
      hist.push({ san: info.san, text: info.text, patterns: info.patterns });
    } catch {
      break;
    }
  }
  s.free();
  return hist;
}

function loadGame(fen, moves) {
  app.session.loadMoves(fen, moves.join(" "));
  app.hist = replayHistory(fen, moves);
  app.viewPly = null;
  refresh();
}

function resetEngines() {
  app.analysis = null;
  app.analysisFor = "";
  app.tablebase = null;
  analysisEngine.run("reset", { fen: null, moves: "" });
  botEngine.run("reset", { fen: null, moves: "" });
}

function newGame(fen = null) {
  if (app.mode === "online") return;
  app.session?.free?.();
  app.session = sessionFor(fen);
  app.hist = [];
  app.viewPly = null;
  resetEngines();
  if (app.mode === "bot") {
    app.userWhite = settings.side === "random" ? Math.random() < 0.5 : settings.side === "white";
    board.flip(app.userWhite);
  }
  refresh();
}

function save() {
  if (app.mode === "online" || !app.st) return;
  store.set(`game:${app.mode}`, { fen: app.st.initial_fen, moves: app.st.uci, userWhite: app.userWhite });
}

function play(uci) {
  if (onlineLive()) return playOnline(uci);
  // Moving from an earlier position replaces what came after it.
  if (!atLatest()) {
    const drop = app.st.ply - app.viewPly;
    app.session.undo(drop);
    app.hist.splice(app.viewPly, drop);
    app.viewPly = null;
  }
  try {
    const info = JSON.parse(app.session.play(uci));
    app.hist.push({ san: info.san, text: info.text, patterns: info.patterns });
  } catch (e) {
    showStatus(String(e.message || e));
    return;
  }
  app.selected = null;
  refresh();
}

function takeBack() {
  if (app.mode === "online" || !app.st?.ply) return;
  app.viewPly = null;
  // Against the bot, take back the bot's reply too, so it's your turn again.
  let n = 1;
  if (app.mode === "bot" && (app.st.turn === "white") === app.userWhite) n = 2;
  n = Math.min(n, app.st.ply);
  app.session.undo(n);
  app.hist.splice(-n, n);
  refresh();
}

function step(to) {
  const last = app.st.ply;
  let ply = to === "first" ? 0 : to === "last" ? last : (app.viewPly ?? last) + to;
  ply = Math.max(0, Math.min(last, ply));
  app.viewPly = ply === last ? null : ply;
  app.selected = null;
  refresh();
}

// ---------- rendering

function refresh() {
  // Checking board photos needs this server's sign-in.
  $("openReview").hidden = !siteApi();
  app.st = JSON.parse(app.session.state());
  if (app.viewPly !== null && app.viewPly >= app.st.ply) app.viewPly = null;
  if (app.viewPly !== null) {
    app.view ??= new Session(null);
    app.view.loadMoves(app.st.initial_fen.split(" ")[0] === START_PLACEMENT ? null : app.st.initial_fen, app.st.uci.slice(0, app.viewPly).join(" "));
  }
  app.vst = JSON.parse(cur().state());
  app.legal = app.vst.status === "started" ? JSON.parse(cur().legalMoves()) : [];
  if (app.editing) return renderEditor();
  const kingSq = app.vst.check ? findKing(app.vst.fen, app.vst.turn === "white") : null;
  board.marks = new Set();
  board.set({ fen: app.vst.fen, legal: canMove() ? app.legal : [], last: app.vst.last, check: kingSq, enabled: canMove() });
  if (!helpOn()) board.setArrows([]);
  renderPlayers();
  renderStatus();
  renderSteps();
  renderHistory();
  renderOptions();
  renderLines();
  renderOnline();
  renderControls();
  save();
  requestAnalysis();
  requestTablebase();
  requestDictionary();
  maybeBotMove();
}

function renderControls() {
  const online = app.mode === "online";
  $("newGame").hidden = online;
  $("undo").hidden = online || app.mode === "analyse";
  $("undo").disabled = !app.st.ply || app.thinking;
  $("onlineControls").hidden = !onlineLive();
  $("analyseTools").hidden = $("photoNote").hidden = app.mode !== "analyse";
  $("controls").hidden = !!app.editing;
  $("editor").hidden = !app.editing;
  const train = $("train");
  train.checked = settings.train;
  const lichessLive = onlineLive() && app.online.serverKey === "lichess";
  train.disabled = lichessLive;
  $("trainNote").hidden = !(onlineLive() && settings.train);
  $("trainNote").textContent = lichessLive
    ? "Engine help is off in live Lichess games: Lichess counts it as cheating. It comes back when the game ends."
    : "Engine help is on in a live game: the app tells the server, and the game is screened and labelled as assisted.";
}

function findKing(fen, white) {
  const rows = fen.split(" ")[0].split("/");
  for (let i = 0; i < 8; i++) {
    let f = 0;
    for (const c of rows[i]) {
      if (/\d/.test(c)) f += Number(c);
      else {
        if (c === (white ? "K" : "k")) return FILES[f] + (8 - i);
        f++;
      }
    }
  }
  return null;
}

function renderPlayers() {
  const top = $("playerTop");
  const bottom = $("playerBottom");
  let white = "White";
  let black = "Black";
  if (app.mode === "bot") {
    const bot = settings.level >= 9 ? `Bot, max strength (${settings.thinkSecs}s a move)` : `Bot, strength ${settings.level}`;
    white = app.userWhite ? "You" : bot;
    black = app.userWhite ? bot : "You";
  } else if (app.mode === "online" && app.online.game) {
    white = app.online.game.white;
    black = app.online.game.black;
  }
  const [t, b] = board.white ? [["black", black], ["white", white]] : [["white", white], ["black", black]];
  for (const [el, [side, name]] of [[top, t], [bottom, b]]) {
    el.querySelector(".who").textContent = name;
    el.dataset.side = side;
    el.classList.toggle("to-move", app.vst.status === "started" && app.vst.turn === side);
  }
  renderClocks();
}

function renderClocks() {
  const g = app.online.game;
  for (const el of [$("playerTop"), $("playerBottom")]) {
    const c = el.querySelector(".clock");
    if (app.mode !== "online" || !g || g.wtime == null) {
      c.hidden = true;
      continue;
    }
    const white = el.dataset.side === "white";
    let ms = white ? g.wtime : g.btime;
    if (g.status === "started" && (app.st.turn === "white") === white) ms -= Date.now() - g.at;
    ms = Math.max(0, ms);
    const s = Math.floor(ms / 1000);
    c.hidden = false;
    c.textContent = s >= 3600 ? `${Math.floor(s / 3600)}:${String(Math.floor(s / 60) % 60).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}` : `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
    c.classList.toggle("low", ms < 20_000);
  }
}

function showStatus(msg) {
  $("status").textContent = msg;
}

function renderStatus() {
  const st = app.st;
  if (!atLatest()) {
    const can = app.mode === "local" || app.mode === "analyse";
    showStatus(`Looking at move ${app.viewPly} of ${st.ply}.${can ? " A move here replaces the moves after it." : ""}`);
    return;
  }
  if (st.status !== "started") {
    const how = { mate: "checkmate", stalemate: "stalemate", draw: "a draw", resign: "resignation", outoftime: "time", aborted: "abort" }[st.status] || st.status;
    if (st.winner) showStatus(`${st.winner === "white" ? "White" : "Black"} wins by ${how}.`);
    else showStatus(st.status === "aborted" ? "Game aborted." : `The game is drawn (${how}).`);
    return;
  }
  const side = st.turn === "white" ? "White" : "Black";
  let msg = `${side} to move${st.check ? ", in check" : ""}.`;
  if (app.mode === "bot" && (st.turn === "white") !== app.userWhite) msg = settings.level >= 9 ? `The bot is thinking (up to ${settings.thinkSecs} seconds)…` : "The bot is thinking…";
  if (app.mode === "online" && app.online.game && !canMove() && onlineLive()) msg = "Waiting for your opponent…";
  if (onDevice() && onlineLive()) {
    const d = app.online.device;
    const mine = (st.turn === "white") === app.online.game.myWhite;
    msg = d?.awaitingBoard ? "Make the engine's move on the board." : mine ? "Your move: make it on the board." : "The engine is thinking…";
  }
  if (app.mode === "online" && !app.online.game) msg = onDevice() ? "Set up the starting position on the board to play." : "Start or accept a game in Play online.";
  showStatus(msg);
}

function renderSteps() {
  const last = app.st.ply;
  const at = app.viewPly ?? last;
  $("stepFirst").disabled = $("stepBack").disabled = at === 0;
  $("stepFwd").disabled = $("stepLast").disabled = at === last;
  $("stepLabel").textContent = last === 0 ? "No moves yet" : at === last ? `Move ${last} of ${last}` : `Move ${at} of ${last}`;
}

function renderHistory() {
  const ol = $("history");
  if (!app.hist.length) {
    ol.innerHTML = `<li class="empty">No moves yet. Every move is written out here in plain English. Tap a move to step back to it.</li>`;
    return;
  }
  const firstWhite = app.st.initial_fen.split(" ")[1] !== "b";
  const firstNo = Number(app.st.initial_fen.split(" ")[5] || 1);
  const at = app.viewPly ?? app.st.ply;
  ol.innerHTML = app.hist
    .map((h, i) => {
      const white = (i % 2 === 0) === firstWhite;
      const no = firstNo + Math.floor((i + (firstWhite ? 0 : 1)) / 2);
      const show = helpOn() ? patternChips(h.patterns) : "";
      return `<li class="${i + 1 === at ? "current" : ""}"><button type="button" class="step-to" data-ply="${i + 1}"><span class="no">${no}${white ? "." : "…"}</span> <b class="san">${esc(h.san)}</b></button> <span class="txt">${esc(h.text)}</span>${show}</li>`;
    })
    .join("");
  ol.querySelector("li.current")?.scrollIntoView({ block: "nearest" });
}

function renderOptions() {
  const box = $("options");
  if (!helpOn() && onlineLive()) {
    box.innerHTML = `<p class="hint">Move help is off during this online game. It comes back when the game ends.</p>`;
    return;
  }
  if (!app.legal.length) {
    box.innerHTML = `<p class="hint">No moves: the game is over at this point.</p>`;
    return;
  }
  const list = app.selected ? app.legal.filter((m) => m.from === app.selected) : app.legal;
  $("optionsHint").textContent = app.selected
    ? `Moves for the ${list[0]?.piece ?? "piece"} on ${app.selected}. Tap it again to see every move.`
    : `${app.legal.length} legal moves. Tap one of the pieces to see only its moves.`;
  const order = ["king", "queen", "rook", "bishop", "knight", "pawn"];
  const vals = helpOn() ? moveValues() : null;
  const byValue = (a, b) => (vals.get(b.uci)?.mover ?? -1e9) - (vals.get(a.uci)?.mover ?? -1e9);
  const sorted = [...list].sort((a, b) => (vals?.size ? byValue(a, b) : 0) || order.indexOf(a.piece) - order.indexOf(b.piece) || a.from.localeCompare(b.from) || a.to.localeCompare(b.to));
  box.innerHTML = `<ul class="moves">${sorted
    .map((m) => `<li><button type="button" class="san-btn" data-uci="${m.uci}" ${canMove() ? "" : "disabled"}>${esc(m.san)}</button>${valueChip(vals?.get(m.uci))}<span class="txt">${esc(m.text)}</span>${patternChips(m.patterns)}</li>`)
    .join("")}</ul>`;
}

/// Each legal move's value here: the server's dictionary where it has a
/// real evaluation (deeper), else this device's quick scoring of every
/// move. uci → {mover (centipawns for the side moving), wdl, rank?, source}.
function moveValues() {
  const key = viewKey().moves;
  const out = new Map();
  const local = app.moveScores?.for === key ? app.moveScores.list : [];
  for (const m of local) out.set(m.uci, { mover: m.cp, wdl: m.wdl, rank: m.rank, source: "this device" });
  const server = app.serverMoves?.for === key ? app.serverMoves.list : [];
  for (const m of server) {
    if (m.scoreCp == null || m.source === "hint") continue;
    out.set(m.uci, { mover: m.scoreCp, wdl: m.wdl, rank: out.get(m.uci)?.rank, source: m.source === "engine" ? `server, depth ${m.depth}` : `server (the position after), depth ${m.depth}` });
  }
  return out;
}

function valueChip(v) {
  if (!v) return "";
  const cp = Math.abs(v.mover) >= 9000 ? (v.mover > 0 ? "mates" : "is mated") : `${v.mover > 0 ? "+" : ""}${(v.mover / 100).toFixed(2)}`;
  const mine = app.vst.turn === "white" ? v.wdl.white : v.wdl.black;
  return `<span class="chip value" title="Value for the side moving (${esc(v.source)}); their chance of winning after it">${cp} · ${pct(mine)}</span>`;
}

function renderSanResult() {
  const v = $("sanInput").value.trim();
  const out = $("sanResult");
  if (!v) {
    out.innerHTML = "";
    return;
  }
  const e = JSON.parse(cur().explain(v));
  if (!e) {
    out.innerHTML = `<p class="hint">${esc(v)} isn't a legal move here. Check the piece letter and the square, or tap a piece on the board to see its moves.</p>`;
    return;
  }
  out.innerHTML = `<div class="decode"><p><b class="san">${esc(e.san)}</b> ${esc(e.text)}</p><table><tbody>${e.tokens
    .map((t) => `<tr><th scope="row">${t.text ? esc(t.text) : "<i>none</i>"}</th><td>${esc(t.meaning)}</td></tr>`)
    .join("")}</tbody></table></div>`;
}

/// The odds to show: an exact tablebase result beats the search estimate.
function currentOdds() {
  const key = viewKey().moves;
  if (app.tablebase?.for === key && app.tablebase.wdl) return { wdl: app.tablebase.wdl, note: app.tablebase.text, exact: true };
  const a = app.analysis;
  if (!a || app.analysisFor !== key) return null;
  if (a.result) return { wdl: a.wdl, note: "The game is over.", exact: true };
  const note =
    a.score?.kind === "mate"
      ? `${scoreText(a.score)} with best play.`
      : `Estimate from a ${a.depth}-ply search of ${(a.nodes / 1000).toFixed(0)}k positions; evaluation ${scoreText(a.score)}.`;
  return { wdl: a.wdl, note, exact: false };
}

function renderOdds() {
  const odds = $("odds");
  odds.hidden = !helpOn();
  if (odds.hidden) return;
  const o = currentOdds();
  if (!o) {
    $("oddsNote").textContent = "Working out the odds…";
    odds.classList.add("pending");
    return;
  }
  odds.classList.remove("pending");
  const w = o.wdl;
  $("segWhite").style.flexGrow = w.white;
  $("segDraw").style.flexGrow = w.draw;
  $("segBlack").style.flexGrow = w.black;
  $("pWhite").textContent = pct(w.white);
  $("pDraw").textContent = pct(w.draw);
  $("pBlack").textContent = pct(w.black);
  $("oddsBar").setAttribute("aria-label", `White ${pct(w.white)}, draw ${pct(w.draw)}, Black ${pct(w.black)}`);
  $("oddsNote").textContent = o.note;
}

function renderLines() {
  renderOdds();
  const box = $("lines");
  const a = app.analysis;
  $("opening").textContent = app.vst.opening ? `${app.vst.opening.name} (${app.vst.opening.eco})` : "";
  const key = viewKey().moves;
  const tb = app.tablebase?.for === key ? app.tablebase : null;
  const entry = app.dictionary?.for === key ? app.dictionary.entry : null;
  const dict = entry && (entry.known || entry.queued || entry.hint) ? entry : null;
  $("tbBox").hidden = !(helpOn() && tb);
  if (tb) $("tbBox").innerHTML = `<b>Tablebase</b> ${esc(tb.text)}${tb.best ? ` Best move: <b>${esc(tb.best)}</b>.` : ""} <span class="hint">${esc(tb.source)}</span>`;
  $("dictBox").hidden = !(helpOn() && dict);
  if (dict) {
    const v = dict.scoreCp / 100;
    const signed = (cp) => `${cp > 0 ? "+" : ""}${(cp / 100).toFixed(2)}`;
    const parts = [];
    if (dict.known) {
      const seen = [dict.seen ? `in ${dict.seen} screened game${dict.seen === 1 ? "" : "s"}` : "", dict.photos ? `in ${dict.photos} confirmed photo${dict.photos === 1 ? "" : "s"}` : ""].filter(Boolean).join(" and ");
      parts.push(`<b>Seen before</b>${seen ? ` ${seen}` : ""}: ${esc(dict.engine || "the engine")} at depth ${dict.depth} prefers <b>${esc(dict.best || "—")}</b> (${signed(dict.scoreCp)} for the side to move).`);
    } else if (dict.queued) {
      parts.push(`<b>Seen before</b> in ${dict.photos} confirmed photo${dict.photos === 1 ? "" : "s"}: waiting for the server's engine to evaluate it.`);
    }
    if (dict.hint) parts.push(`<span class="hint">A player's app (${esc(dict.hint.engine)}, depth ${dict.hint.depth}) suggests ${esc(dict.hint.best || "—")} (${signed(dict.hint.scoreCp)}): a hint, not checked.</span>`);
    $("dictBox").innerHTML = parts.join(" ");
  }
  if (!helpOn()) {
    box.innerHTML = onlineLive()
      ? `<p class="hint">Engine lines are off during this game. They come back when it ends, so you can review it.</p>`
      : `<p class="hint">Turn on Training to see the five best continuations and each side's chances.</p>`;
    return;
  }
  if (app.vst.status !== "started") {
    box.innerHTML = `<p class="hint">The game is over at this point. Step back, take back moves or start a new game to keep training.</p>`;
    return;
  }
  if (!a || app.analysisFor !== key) {
    box.innerHTML = `<p class="hint">${settings.depth === "max" ? `Searching for up to ${settings.thinkSecs} seconds…` : "Searching for the best lines…"}</p>`;
    return;
  }
  box.innerHTML = a.lines
    .map(
      (l, i) => `<article class="line" data-i="${i}">
        <header><span class="rank">${l.rank}</span><span class="eval">${scoreText(l.score)}</span><span class="mini">White ${pct(l.wdl.white)} · draw ${pct(l.wdl.draw)} · Black ${pct(l.wdl.black)}</span></header>
        <ol class="seq">${l.moves
          .map((m, j) => `<li><button type="button" class="mv ${m.projected ? "projected" : ""}" data-i="${i}" data-j="${j}" ${m.projected ? 'title="Beyond the main search: a likely continuation, less certain"' : ""}>${j === 0 || m.side === "white" ? `<span class="no">${esc(m.number)}</span>` : ""}${esc(m.san)}</button></li>`)
          .join("")}</ol>
        <div class="explain" id="explain-${i}"></div>
        ${canMove() ? `<button type="button" class="play-first" data-uci="${l.moves[0].uci}">Play ${esc(l.moves[0].san)}</button>` : ""}
      </article>`,
    )
    .join("");
  showLineArrows(0);
}

function showLineArrows(i) {
  const l = app.analysis?.lines?.[i];
  if (!l || !helpOn()) return board.setArrows([]);
  board.setArrows(l.moves.slice(0, 2).map((m, j) => ({ uci: m.uci, kind: j === 0 ? "best" : "reply" })));
}

function explainLineMove(i, j) {
  const m = app.analysis.lines[i].moves[j];
  const box = $(`explain-${i}`);
  box.innerHTML = `<p><b class="san">${esc(m.number)} ${esc(m.san)}</b> ${esc(m.text)}${m.projected ? " <em>(projected beyond the main search)</em>" : ""}</p>${patternChips(m.patterns)}`;
  board.setArrows([{ uci: m.uci, kind: "best" }]);
}

// ---------- engines, tablebases and the position dictionary

async function requestAnalysis() {
  if (!helpOn()) return;
  const key = viewKey();
  if (app.vst.status !== "started") {
    const w = app.vst.winner;
    app.analysis = { wdl: w ? { white: w === "white" ? 1 : 0, draw: 0, black: w === "black" ? 1 : 0 } : { white: 0, draw: 1, black: 0 }, result: app.vst.status, lines: [] };
    app.analysisFor = key.moves;
    renderOdds();
    return;
  }
  const target = SEARCH[settings.depth] || SEARCH.normal;
  if (app.analysisFor === key.moves && app.analysis?.final) return;
  const passes = settings.depth === "quick" ? [{ ...SEARCH.quick, final: true }] : [SEARCH.quick, { ...target, final: true }];
  for (const p of passes) {
    let res;
    const timeMs = settings.depth === "max" && p.final ? settings.thinkSecs * 1000 : 0;
    try {
      res = await analysisEngine.run("analyse", key, { depth: p.depth, nodes: p.nodes, lines: 5, plies: settings.plies, timeMs });
    } catch (e) {
      showStatus(`Analysis failed: ${e.message}`);
      return;
    }
    if (viewKey().moves !== key.moves || viewKey().fen !== key.fen || !helpOn()) return; // moved on
    res.final = !!p.final;
    app.analysis = res;
    app.analysisFor = key.moves;
    renderLines();
  }
  await requestMoveScores(key);
}

/// Score every legal move (a shallower search with a line per move), then
/// offer this device's analysis to the server's dictionary as a hint.
async function requestMoveScores(key) {
  if (app.moveScores?.for === key.moves) return;
  let list;
  try {
    list = await analysisEngine.run("moves", key, { depth: 5, nodes: 400_000 });
  } catch {
    return;
  }
  if (viewKey().moves !== key.moves || !helpOn()) return;
  app.moveScores = { for: key.moves, list };
  renderOptions();
  sendHint(key, list);
}

const hintsSent = new Set();
function sendHint(key, list) {
  const api = siteApi();
  const a = app.analysis;
  if (!api || onlineLive() || !navigator.onLine || !list.length || !a?.final || app.analysisFor !== key.moves) return;
  const fen = app.vst.fen;
  if (hintsSent.has(fen)) return;
  hintsSent.add(fen);
  const clamp = (cp) => Math.max(-1000, Math.min(1000, cp));
  const best = a.lines?.[0];
  const white = app.vst.turn === "white";
  const top = best ? clamp(best.score.kind === "mate" ? (best.score.moves > 0) === white ? 1000 : -1000 : white ? best.score.value : -best.score.value) : clamp(list[0].cp);
  api
    .positionHint({ fen, best: best?.moves?.[0]?.uci ?? list[0].uci, scoreCp: top, depth: a.depth || 1, engine: "open-chess app", moves: list.map((m) => ({ uci: m.uci, scoreCp: clamp(m.cp) })) })
    .catch(() => {});
}

/// Exact results for small endings: Lichess's server (up to 7 pieces)
/// when online, the built-in 3-piece tables otherwise.
async function requestTablebase() {
  if (!helpOn() || app.vst.status !== "started") return;
  const fen = app.vst.fen;
  const n = pieceCount(fen);
  if (n > 7) return;
  const key = viewKey().moves;
  if (app.tablebase?.for === key) return;
  const white = app.vst.turn === "white";
  const side = white ? "White" : "Black";
  const other = white ? "Black" : "White";
  const asWdl = (r) => (r === 0 ? { white: 0, draw: 1, black: 0 } : (r > 0) === white ? { white: 1, draw: 0, black: 0 } : { white: 0, draw: 0, black: 1 });
  let result = null;
  // Three pieces: the built-in tables answer offline and instantly (after a
  // one-off build). More: ask Lichess, when online.
  if (n === 3) {
    try {
      const p = await analysisEngine.run("tablebase", viewKey());
      if (p) {
        const r = p.result === "win" ? 1 : p.result === "loss" ? -1 : 0;
        const mate = p.plies != null && r !== 0 ? ` Mate in ${Math.ceil(p.plies / 2)}.` : "";
        result = { for: key, wdl: asWdl(r), text: r > 0 ? `${side} wins.${mate}` : r < 0 ? `${other} wins.${mate}` : "A draw with best play.", best: null, source: "(built in, 3 pieces)" };
      }
    } catch {}
  }
  if (!result && navigator.onLine) {
    try {
      // Our server keeps every answer it fetches; Lichess directly otherwise.
      const site = onlineLive() ? null : siteApi();
      const t = site ? await site.tablebase(fen).catch(() => lichessTablebase(fen)) : await lichessTablebase(fen);
      const cat = t.category;
      const r = cat === "win" ? 1 : cat === "loss" ? -1 : cat === "draw" || cat === "cursed-win" || cat === "blessed-loss" ? 0 : null;
      if (r !== null) {
        const mate = t.dtm != null ? ` Mate in ${Math.ceil(Math.abs(t.dtm) / 2)}.` : "";
        const text = r > 0 ? `${side} wins.${mate}` : r < 0 ? `${other} wins.${mate}` : cat === "draw" ? "A draw with best play." : "A draw under the 50-move rule (the win takes too long).";
        result = { for: key, wdl: asWdl(r), text, best: t.moves?.[0]?.san, source: "(Lichess, up to 7 pieces)" };
      }
    } catch {}
  }
  if (result && viewKey().moves === key) {
    app.tablebase = result;
    renderLines();
  }
}

function siteApi() {
  const token = store.get("token:site", null);
  return token ? new Api(settings.siteBase, token) : null;
}

/// The position dictionary on our chess server, when signed in.
async function requestDictionary() {
  const api = siteApi();
  if (!api || !helpOn() || onlineLive() || !navigator.onLine) return;
  const key = viewKey().moves;
  if (app.dictionary?.for === key) return;
  try {
    const [entry, moves] = await Promise.all([api.position(app.vst.fen), api.positionMoves(app.vst.fen).catch(() => null)]);
    if (viewKey().moves === key) {
      app.dictionary = { for: key, entry };
      if (moves) app.serverMoves = { for: key, list: moves.moves || [] };
      renderLines();
      renderOptions();
    }
  } catch {
    app.dictionary = { for: key, entry: null };
  }
}

async function maybeBotMove() {
  if (app.mode !== "bot" || app.thinking || app.st.status !== "started" || !atLatest()) return;
  if ((app.st.turn === "white") === app.userWhite) return;
  app.thinking = true;
  const key = { fen: app.st.initial_fen, moves: app.st.uci.join(" ") };
  const started = performance.now();
  try {
    const mv =
      settings.level >= 9
        ? await botEngine.run("best", key, { timeMs: settings.thinkSecs * 1000 })
        : await botEngine.run("bot", key, { level: settings.level, seed: (Math.random() * 2 ** 32) >>> 0 });
    const wait = 350 - (performance.now() - started);
    if (wait > 0) await new Promise((r) => setTimeout(r, wait));
    app.thinking = false;
    if (mv && app.st.uci.join(" ") === key.moves && app.mode === "bot") play(mv.uci);
    else refresh();
  } catch (e) {
    app.thinking = false;
    showStatus(`The bot failed to move: ${e.message}`);
  }
}

// ---------- board editor and photos

/// The board editor: a position to correct (after a photo) or set up.
/// Pieces drag to move (off the board to remove), drag in from the
/// palette, or change by clicking their square (position-editor.js).
function startEditor(placement, white = true, unsure = []) {
  board.setArrows([]);
  const editor = new PositionEditor($("editBoard"), {
    placement,
    unsure,
    white: board.white,
    hint: false, // the status line says how
    onChange: () => ($("editError").textContent = ""),
  });
  app.editing = { editor, white };
  $("board").hidden = true;
  $("editBoard").hidden = false;
  renderControls();
  renderEditor();
}

function closeEditor() {
  app.editing = null;
  $("editBoard").hidden = true;
  $("editBoard").innerHTML = "";
  $("board").hidden = false;
}

function renderEditor() {
  const e = app.editing;
  // Nothing about the old position while a new one is being set up.
  $("odds").hidden = true;
  $("tbBox").hidden = $("dictBox").hidden = true;
  $("opening").textContent = "";
  $("lines").innerHTML = `<p class="hint">Finish setting up the position to see each side's chances and the best moves.</p>`;
  board.setArrows([]);
  $("editWhite").setAttribute("aria-pressed", String(e.white));
  $("editBlack").setAttribute("aria-pressed", String(!e.white));
  const unsure = e.editor.unsure.size ? "Check the ringed squares: the photo wasn't clear there. " : "";
  showStatus(`${unsure}Drag a piece to move it, or off the board to remove it. Click a square to change its piece.`);
  $("editError").textContent = "";
}

function finishEditor() {
  const e = app.editing;
  const fen = completeFen(e.editor.placement, e.white);
  const problem = fen ? checkFen(fen) : "the board couldn't be read";
  if (problem) {
    $("editError").textContent = `This isn't a legal position yet: ${explainFenProblem(problem)}`;
    return;
  }
  closeEditor();
  confirmPhoto(fen);
  setMode("analyse", { keepGame: true });
  newGame(fen);
  showStatus("Position set up. Training shows each side's chances and the best moves from here.");
}

function explainFenProblem(p) {
  const s = String(p).toLowerCase();
  if (s.includes("king")) return "each side needs exactly one king.";
  if (s.includes("check")) return "the side not to move can't be in check.";
  if (s.includes("pawn")) return "pawns can't stand on the first or last rank.";
  return "check the kings, pawns and whose move it is.";
}

function cancelEditor() {
  closeEditor();
  app.photo = null;
  refresh();
}

async function photoToDataUrl(file) {
  const bmp = await createImageBitmap(file);
  const scale = Math.min(1, 1280 / Math.max(bmp.width, bmp.height));
  const c = document.createElement("canvas");
  c.width = Math.round(bmp.width * scale);
  c.height = Math.round(bmp.height * scale);
  c.getContext("2d").drawImage(bmp, 0, 0, c.width, c.height);
  return c.toDataURL("image/jpeg", 0.85);
}

/// Read a photo. "next": the photo shows the game after the next move, so
/// match it against the legal moves; "new": start from what it shows.
///
/// The board reader on this device goes first (it works offline). The
/// chess server is asked when there's no reader here, or when the
/// reader can't settle it: it may ask a vision model too, and keeps the
/// photo so a person can confirm it and the reader learns from it.
async function fromPhoto(file, how) {
  showStatus("Reading the board from the photo…");
  app.photo = null;
  let reading = null;
  try {
    vision ??= new Vision();
    if ((await vision.run("info")).available) {
      const bitmap = await createImageBitmap(file);
      reading = await vision.run("read", { bitmap }, [bitmap]);
    }
  } catch (e) {
    console.warn("on-device board reading:", e);
  }
  const settled = reading && (how === "next" ? sureMatch(photoCandidates(reading)) : readingIsSure(reading));
  if (!settled) {
    const api = siteApi();
    if (api && navigator.onLine) {
      showStatus(`Asking  about the photo…`);
      try {
        reading = await api.recognise(await photoToDataUrl(file));
      } catch (e) {
        if (!reading) {
          showStatus(`Couldn't read the photo: ${e.message}`);
          return;
        }
      }
    } else if (!reading) {
      showStatus(
        !api
          ? `Photo reading runs on the chess server: sign in under Online ›  first. You can set up the position by hand meanwhile.`
          : "Photo reading needs a connection. Set up the position by hand instead, or try again when you're online.",
      );
      return;
    }
  }
  app.photo = reading.photoId ? { id: reading.photoId } : null;
  // The first photo the server could have kept: ask about the photo licence.
  if (reading.termsNeeded) askPhotoTerms(siteApi());
  if (how === "next") {
    const cands = photoCandidates(reading);
    const a = sureMatch(cands);
    if (a) {
      if (!a.uci) showStatus("The photo shows the same position: no move yet.");
      else {
        play(a.uci);
        confirmPhoto(a.fen);
        showStatus(`Read from the photo: ${a.san}${a.distance ? ` (${a.distance} square${a.distance === 1 ? "" : "s"} didn't match, which is normal for a photo)` : ""}.`);
      }
      return;
    }
    return chooseCandidate(cands, reading);
  }
  const white = reading.sideToMove ? reading.sideToMove === "w" : true;
  if (reading.whiteAtBottom === false) board.flip(false);
  startEditor(reading.placement, white, reading.unsureSquares || []);
}

/// Every position one move on, scored against the reading: by the
/// reader's per-square probabilities when it gave them, else by squares
/// that differ.
function photoCandidates(reading) {
  const c = reading.squares
    ? cur().matchPhotoProbs(JSON.stringify(reading.squares))
    : cur().matchPhoto(reading.placement);
  return JSON.parse(c) || [];
}

/// The candidate to play without asking, if one clearly stands out.
function sureMatch(cands) {
  const [a, b] = cands;
  if (!a) return null;
  if (a.probability != null) return a.probability >= 0.9 && a.distance <= 6 ? a : null;
  return a.distance <= 6 && (!b || b.distance - a.distance >= 2) ? a : null;
}

function readingIsSure(r) {
  return r.plausible !== false && !(r.unsureSquares || []).length && (r.confidence ?? 0) >= 0.5;
}

/// The player confirmed what a photo kept for review showed (they played
/// the move it matched, or finished setting the position up): a label the
/// board reader learns from.
function confirmPhoto(fen) {
  const p = app.photo;
  app.photo = null;
  const api = siteApi();
  if (!p || !api) return;
  api.confirmPhoto(p.id, String(fen)).catch((e) => console.warn("confirming the photo:", e));
}

function chooseCandidate(cands, reading) {
  const dlg = $("pickMove");
  $("pickList").innerHTML = cands
    .filter((c) => c.distance <= 12)
    .map((c) => `<li><button type="button" data-uci="${c.uci}">${c.uci ? esc(c.san) : "No move yet"}</button> <span class="hint">${c.probability != null ? `${Math.round(c.probability * 100)}% likely · ` : ""}${c.distance} square${c.distance === 1 ? "" : "s"} differ</span></li>`)
    .join("");
  $("pickSetup").onclick = () => {
    dlg.close();
    startEditor(reading.placement, reading.sideToMove ? reading.sideToMove === "w" : app.vst.turn === "white", reading.unsureSquares || []);
  };
  $("pickList").onclick = (e) => {
    const b = e.target.closest("button");
    if (!b) return;
    dlg.close();
    const c = cands.find((x) => x.uci === b.dataset.uci);
    if (c) confirmPhoto(c.fen);
    if (b.dataset.uci) play(b.dataset.uci);
  };
  dlg.showModal();
}

// ---------- online

function api() {
  return app.online.api;
}

function serverBase(key) {
  return key === "site" ? settings.siteBase : SERVERS.lichess.base;
}

async function connectOnline() {
  const o = app.online;
  const token = SERVERS[o.serverKey].device ? DEVICE_TOKEN : store.get(`token:${o.serverKey}`, null);
  o.api = token ? new Api(serverBase(o.serverKey), token) : null;
  o.me = null;
  o.device = null;
  o.error = "";
  o.eventsAbort?.abort();
  if (!o.api) return renderOnline();
  try {
    o.me = await o.api.account();
    if (SERVERS[o.serverKey].device) o.device = await o.api.device();
  } catch (e) {
    o.error = `Couldn't reach ${SERVERS[o.serverKey].name}: ${e.message}. Check you're online, or sign in again.`;
    return renderOnline();
  }
  renderOnline();
  listenEvents();
}

async function listenEvents() {
  const o = app.online;
  const ctl = new AbortController();
  o.eventsAbort = ctl;
  for (let delay = 1000; !ctl.signal.aborted; delay = Math.min(delay * 2, 30_000)) {
    try {
      await o.api.events(onEvent, ctl.signal);
      delay = 1000;
    } catch (e) {
      if (ctl.signal.aborted) return;
    }
    await new Promise((r) => setTimeout(r, delay));
  }
}

function onEvent(ev) {
  const o = app.online;
  if (ev.type === "challenge") {
    o.challenges.set(ev.challenge.id, ev.challenge);
  } else if (ev.type === "challengeCanceled" || ev.type === "challengeDeclined") {
    o.challenges.delete(ev.challenge.id);
  } else if (ev.type === "gameStart") {
    const id = ev.game.gameId || ev.game.id;
    o.challenges.clear();
    // A board device starts a new game whenever the pieces are set up again.
    const replaced = SERVERS[o.serverKey].device && o.game && o.game.id !== id;
    if (!o.game || o.game.status !== "started" || replaced) openOnlineGame(id);
  }
  renderOnline();
}

async function openOnlineGame(id) {
  const o = app.online;
  o.gameAbort?.abort();
  const ctl = new AbortController();
  o.gameAbort = ctl;
  o.game = { id, status: "started", white: "White", black: "Black", myWhite: true };
  o.assistSent = null;
  setMode("online", { keepGame: true });
  try {
    await o.api.game(id, (ev) => onGameEvent(ev), ctl.signal);
  } catch (e) {
    if (!ctl.signal.aborted) {
      o.error = `Lost the game stream: ${e.message}. Reopen the game from Play online.`;
      renderOnline();
    }
  }
}

function onGameEvent(ev) {
  const o = app.online;
  const g = o.game;
  if (ev.type === "gameFull") {
    const name = (p) => p?.name || p?.id || (p?.aiLevel ? `Computer level ${p.aiLevel}` : "Anonymous");
    g.white = name(ev.white);
    g.black = name(ev.black);
    g.myWhite = ev.white?.id?.toLowerCase() === o.me.id.toLowerCase();
    g.fen = ev.initialFen && ev.initialFen !== "startpos" ? ev.initialFen : null;
    board.flip(g.myWhite);
    onGameState(ev.state);
  } else if (ev.type === "gameState") {
    onGameState(ev);
  } else if (ev.type === "opponentGone") {
    o.error = ev.gone ? "Your opponent has left the game." : "";
    renderOnline();
  }
}

function onGameState(s) {
  const g = app.online.game;
  g.status = s.status;
  g.winner = s.winner;
  // Untimed games (correspondence, a board device without a clock) send
  // the largest 32-bit number of milliseconds: show no clock.
  const untimed = s.wtime >= 2147483647;
  g.wtime = untimed ? null : s.wtime;
  g.btime = untimed ? null : s.btime;
  g.at = Date.now();
  g.drawOffer = (g.myWhite ? s.bdraw : s.wdraw) || false;
  const moves = s.moves ? s.moves.split(" ") : [];
  g.moves = moves;
  if (moves.join(" ") !== app.st.uci.join(" ")) loadGame(g.fen, moves);
  else refresh();
  declareAssist();
  refreshDevice();
  if (g.status !== "started") {
    app.online.gameAbort?.abort();
    showStatus(`Game over: ${g.winner ? `${g.winner === "white" ? "White" : "Black"} wins` : "draw"} (${g.status}). Training is available again for review.`);
  }
}

/// On our own server, tell it whenever engine help is on or off during a
/// live game (a label for the fair-play model; screening doesn't rely on it).
function declareAssist() {
  const o = app.online;
  if (!onlineLive() || o.serverKey !== "site" || onDevice()) return;
  const on = settings.train;
  if (o.assistSent === on) return;
  o.assistSent = on;
  o.api.assist(o.game.id, on).catch(() => {
    o.assistSent = null;
  });
}

/// A board device's settings can change on the device too (the clock's
/// buttons): read them again as the game goes on.
async function refreshDevice() {
  const o = app.online;
  if (!onDevice() || !o.api) return;
  try {
    const d = await o.api.device();
    if (JSON.stringify(d) === JSON.stringify(o.device)) return;
    o.device = d;
    // Sides switched on the device: turn the board and the names round.
    const g = o.game;
    if (g && g.id === d.gameId && (d.play === "white") !== g.myWhite) {
      g.myWhite = d.play === "white";
      [g.white, g.black] = [g.black, g.white];
      board.flip(g.myWhite);
      refresh();
    }
    renderOnline();
    renderStatus();
  } catch {
    // keep what we have
  }
}

function deviceCard() {
  const o = app.online;
  const d = o.device || {};
  const times = ["none", "3+2", "5+0", "5+3", "10+0", "10+5", "15+10", "30+0"];
  const time = d.time || "none";
  if (!times.includes(time)) times.push(time);
  return `<section class="card"><h3>${esc(SERVERS[o.serverKey].name)}</h3>
    <p>Play on the board: this app shows the game, the engine's moves and the clock. Set up the starting position on the board for a new game; put pieces back to take moves back.</p>
    <p class="hint">Opponent: ${esc(d.engine || "the engine")}${d.paused ? " · clock paused" : ""}</p>
    ${d.builtin ? `<label class="field"><span>Engine level</span><select id="devLevel">${[1, 2, 3, 4, 5, 6, 7, 8].map((n) => `<option ${n === d.level ? "selected" : ""}>${n}</option>`).join("")}</select></label>` : ""}
    <div class="row">
      <label class="field"><span>You play</span><select id="devPlay"><option value="white" ${d.play === "white" ? "selected" : ""}>White</option><option value="black" ${d.play === "black" ? "selected" : ""}>Black</option></select></label>
      <label class="field"><span>Time control</span><select id="devTime">${times.map((t) => `<option value="${t}" ${t === time ? "selected" : ""}>${t === "none" ? "Untimed" : t}</option>`).join("")}</select></label>
    </div>
    <div class="row"><button type="button" class="primary" id="saveDevice">Apply</button></div>
    <p class="hint">A new time control starts with the next game once this one has begun. The clock's buttons change the level and sides too.</p>
  </section>`;
}

async function playOnline(uci) {
  const g = app.online.game;
  try {
    const info = JSON.parse(app.session.play(uci));
    app.hist.push({ san: info.san, text: info.text, patterns: info.patterns });
    refresh();
    await api().move(g.id, uci);
  } catch (e) {
    showStatus(`Move not accepted: ${e.message}`);
    app.session.undo(1);
    app.hist.pop();
    refresh();
  }
}

function renderOnline() {
  $("tab-online").hidden = app.mode !== "online";
  if (app.mode !== "online") return;
  const o = app.online;
  const box = $("online");
  const server = SERVERS[o.serverKey];
  const serverPick = `<div class="field"><span>Play on</span><div class="seg-pick">${Object.entries(SERVERS)
    .map(([k, s]) => `<button type="button" data-server="${k}" aria-pressed="${k === o.serverKey}">${esc(s.name)}</button>`)
    .join("")}</div></div>`;
  if (!o.api) {
    box.innerHTML = `${serverPick}
      <p>${server.noLogin ? "Connect" : "Sign in"} to ${esc(server.name)} to play rated or casual games. Moves are sent through ${esc(server.name)}'s Board API.</p>
      ${o.serverKey === "site" ? `<p class="hint">Connecting here also turns on photo reading and the position dictionary.</p>` : ""}
      ${server.noLogin ? `<p class="hint">${esc(server.name)} has no browser sign-in: use an access token.</p>` : `<div class="row"><button type="button" class="primary" id="signIn">Sign in to ${esc(server.name)}</button></div>`}
      <details class="token"${server.noLogin ? " open" : ""}><summary>Use an access token${server.noLogin ? "" : " instead"}</summary>
        <p>Create a personal token with the <b>board:play</b> and <b>challenge:write</b> scopes, then paste it here.</p>
        <label class="field"><span>Token</span><input id="tokenInput" spellcheck="false" autocomplete="off"></label>
        <button type="button" id="saveToken">Save token</button>
      </details>
      ${o.error ? `<p class="error">${esc(o.error)}</p>` : ""}`;
    return;
  }
  if (!o.me) {
    box.innerHTML = `${serverPick}<p>${o.error ? `<span class="error">${esc(o.error)}</span>` : "Connecting…"}</p><button type="button" id="signOut">Sign out</button>`;
    return;
  }
  if (SERVERS[o.serverKey].device) {
    box.innerHTML = `${serverPick}${o.error ? `<p class="error">${esc(o.error)}</p>` : ""}${deviceCard()}`;
    return;
  }
  const g = o.game;
  let gamePart = "";
  if (g && g.status === "started") {
    gamePart = `<section class="card"><h3>Playing ${esc(g.myWhite ? g.black : g.white)}</h3>
      <p>You have the ${g.myWhite ? "white" : "black"} pieces.</p>
      ${g.drawOffer ? `<p>Your opponent offers a draw.</p><div class="row"><button type="button" id="acceptDraw" class="primary">Accept draw</button><button type="button" id="declineDraw">Decline</button></div>` : ""}
      ${app.st.ply < 2 ? `<button type="button" id="abortGame">Abort game</button>` : ""}</section>`;
  }
  const incoming = [...o.challenges.values()].filter((c) => c.destUser?.id?.toLowerCase() === o.me.id.toLowerCase());
  const outgoing = [...o.challenges.values()].filter((c) => c.challenger?.id?.toLowerCase() === o.me.id.toLowerCase());
  box.innerHTML = `${serverPick}
    <p>Signed in as <b>${esc(o.me.username || o.me.id)}</b>. <button type="button" class="link" id="signOut">Sign out</button></p>
    ${o.error ? `<p class="error">${esc(o.error)}</p>` : ""}
    ${gamePart}
    ${incoming.length ? `<section class="card"><h3>Challenges for you</h3><ul class="challenges">${incoming
      .map((c) => `<li><span>${esc(c.challenger?.name || c.challenger?.id)} · ${esc(c.timeControl?.show || "unlimited")} · ${c.rated ? "rated" : "casual"}</span><span class="row"><button type="button" class="primary" data-accept="${c.id}">Accept</button><button type="button" data-decline="${c.id}">Decline</button></span></li>`)
      .join("")}</ul></section>` : ""}
    ${outgoing.length ? `<p class="hint">Waiting for ${outgoing.map((c) => esc(c.destUser?.name || c.destUser?.id)).join(", ")} to accept your challenge.</p>` : ""}
    ${g?.status === "started" ? "" : `<section class="card"><h3>Start a game</h3>
      <label class="field"><span>Opponent's username</span><input id="opponent" autocomplete="off" autocapitalize="off" spellcheck="false"></label>
      <div class="row">
        <label class="field"><span>Minutes</span><select id="tcMin"><option>10</option><option selected>15</option><option>30</option></select></label>
        <label class="field"><span>Increment</span><select id="tcInc"><option>0</option><option>5</option><option selected>10</option><option>20</option></select></label>
        <label class="field"><span>Colour</span><select id="tcColor"><option value="random">Random</option><option value="white">White</option><option value="black">Black</option></select></label>
      </div>
      <label class="check"><input type="checkbox" id="tcRated"> Rated</label>
      <div class="row"><button type="button" class="primary" id="sendChallenge">Send challenge</button></div>
      <div class="row"><label class="field"><span>${o.serverKey === "lichess" ? "Lichess AI level" : "Computer level"}</span><select id="aiLevel">${[1, 2, 3, 4, 5, 6, 7, 8].map((n) => `<option ${n === 3 ? "selected" : ""}>${n}</option>`).join("")}</select></label><button type="button" id="playAi">${o.serverKey === "lichess" ? "Play the Lichess AI" : "Play the computer"}</button></div>
      <p class="hint">Online games use the Board API, which allows rapid and slower time controls.</p>
    </section>`}`;
}

async function onlineAction(fn, ok) {
  try {
    await fn();
    app.online.error = "";
    if (ok) showStatus(ok);
  } catch (e) {
    app.online.error = e.message;
  }
  renderOnline();
}

function wireOnline() {
  $("online").addEventListener("click", async (e) => {
    const t = e.target.closest("button");
    if (!t) return;
    const o = app.online;
    if (t.dataset.server) {
      o.serverKey = t.dataset.server;
      store.set("server", o.serverKey);
      o.challenges.clear();
      return connectOnline();
    }
    if (t.id === "signIn") return beginSignIn(o.serverKey, serverBase(o.serverKey));
    if (t.id === "saveToken") {
      const v = $("tokenInput").value.trim();
      if (v) store.set(`token:${o.serverKey}`, v);
      return connectOnline();
    }
    if (t.id === "signOut") {
      const token = store.get(`token:${o.serverKey}`, null);
      store.del(`token:${o.serverKey}`);
      if (token) fetch(`${serverBase(o.serverKey)}/api/token`, { method: "DELETE", headers: { Authorization: `Bearer ${token}` } }).catch(() => {});
      o.eventsAbort?.abort();
      return connectOnline();
    }
    if (t.dataset.accept) return onlineAction(() => api().accept(t.dataset.accept));
    if (t.dataset.decline) return onlineAction(() => api().decline(t.dataset.decline).then(() => o.challenges.delete(t.dataset.decline)));
    const tc = () => ({ minutes: Number($("tcMin").value), increment: Number($("tcInc").value), color: $("tcColor").value, rated: $("tcRated").checked });
    if (t.id === "sendChallenge") {
      const who = $("opponent").value.trim();
      if (!who) return ($("opponent").focus(), (o.error = "Enter your opponent's username."), renderOnline());
      return onlineAction(() => api().challenge(who, tc()), `Challenge sent to ${who}.`);
    }
    if (t.id === "playAi") return onlineAction(() => api().challengeAi(Number($("aiLevel").value), tc()));
    if (t.id === "saveDevice") {
      const form = { play: $("devPlay").value, time: $("devTime").value };
      if ($("devLevel")) form.level = $("devLevel").value;
      return onlineAction(async () => {
        const d = await api().setDevice(form);
        await refreshDevice();
        if (d?.said?.length) showStatus(d.said.join(" "));
      });
    }
    const g = o.game;
    if (t.id === "acceptDraw") return onlineAction(() => api().draw(g.id, true));
    if (t.id === "declineDraw") return onlineAction(() => api().draw(g.id, false));
    if (t.id === "abortGame") return onlineAction(() => api().abort(g.id));
  });
}

// ---------- modes, tabs, settings

function setMode(mode, { keepGame = false } = {}) {
  app.mode = mode;
  store.set("mode", mode);
  app.viewPly = null;
  for (const b of document.querySelectorAll(".modes button")) b.setAttribute("aria-checked", String(b.dataset.mode === mode));
  if (mode === "online") {
    if (!keepGame) {
      const g = app.online.game;
      app.session = new Session(null);
      app.hist = [];
      if (g) return loadGame(g.fen, g.moves ?? []);
    }
    selectTab("online");
    if (!app.online.api && !keepGame) connectOnline();
  } else {
    if (app.mode !== "online" && $("tab-online").getAttribute("aria-selected") === "true") selectTab("lines");
    if (!keepGame) {
      const saved = store.get(`game:${mode}`, null);
      if (saved) {
        app.userWhite = saved.userWhite ?? true;
        board.flip(mode === "bot" ? app.userWhite : true);
        app.session = new Session(null);
        resetEngines();
        return loadGame(saved.fen, saved.moves);
      }
      return newGame();
    }
  }
  refresh();
}

function selectTab(name) {
  for (const t of document.querySelectorAll('[role="tab"]')) {
    const on = t.id === `tab-${name}`;
    t.setAttribute("aria-selected", String(on));
    t.tabIndex = on ? 0 : -1;
    $(t.getAttribute("aria-controls")).hidden = !on;
  }
}

function wire() {
  for (const b of document.querySelectorAll(".modes button")) b.addEventListener("click", () => b.dataset.mode !== app.mode && setMode(b.dataset.mode));
  for (const t of document.querySelectorAll('[role="tab"]')) t.addEventListener("click", () => selectTab(t.id.slice(4)));
  document.querySelector(".tabs").addEventListener("keydown", (e) => {
    if (!["ArrowLeft", "ArrowRight"].includes(e.key)) return;
    const tabs = [...document.querySelectorAll('[role="tab"]:not([hidden])')];
    const i = tabs.indexOf(document.activeElement);
    const next = tabs[(i + (e.key === "ArrowRight" ? 1 : tabs.length - 1)) % tabs.length];
    next.focus();
    next.click();
  });
  document.addEventListener("keydown", (e) => {
    if (e.target.closest("input, select, textarea, dialog") || app.editing) return;
    if (e.key === "ArrowLeft" && !e.target.closest('[role="tab"]')) step(-1);
    if (e.key === "ArrowRight" && !e.target.closest('[role="tab"]')) step(1);
  });
  $("stepFirst").addEventListener("click", () => step("first"));
  $("stepBack").addEventListener("click", () => step(-1));
  $("stepFwd").addEventListener("click", () => step(1));
  $("stepLast").addEventListener("click", () => step("last"));
  $("history").addEventListener("click", (e) => {
    const b = e.target.closest(".step-to");
    if (b) step(Number(b.dataset.ply) - (app.viewPly ?? app.st.ply));
  });
  $("newGame").addEventListener("click", () => newGame());
  $("undo").addEventListener("click", takeBack);
  $("flip").addEventListener("click", () => {
    board.flip();
    if (!app.editing) renderPlayers();
  });
  $("train").addEventListener("change", (e) => {
    settings.train = e.target.checked;
    store.set("train", settings.train);
    declareAssist();
    refresh();
  });
  $("resign").addEventListener("click", () => {
    if (confirm("Resign this game?")) onlineAction(() => api().resign(app.online.game.id));
  });
  $("offerDraw").addEventListener("click", () => onlineAction(() => api().draw(app.online.game.id, true), "Draw offered."));
  $("copyPgn").addEventListener("click", async () => {
    const names = { white: "White", black: "Black" };
    if (app.mode === "bot") [names.white, names.black] = app.userWhite ? ["You", `Bot ${settings.level}`] : [`Bot ${settings.level}`, "You"];
    if (app.mode === "online" && app.online.game) [names.white, names.black] = [app.online.game.white, app.online.game.black];
    const pgn = app.session.pgn(names.white, names.black, "Chess lab");
    try {
      await navigator.clipboard.writeText(pgn);
      showStatus("PGN copied.");
    } catch {
      showStatus("Couldn't copy: your browser blocked the clipboard.");
    }
  });
  $("sanInput").addEventListener("input", renderSanResult);
  $("options").addEventListener("click", (e) => {
    const b = e.target.closest(".san-btn");
    if (b && canMove()) play(b.dataset.uci);
  });
  $("lines").addEventListener("click", (e) => {
    const mv = e.target.closest(".mv");
    if (mv) return explainLineMove(Number(mv.dataset.i), Number(mv.dataset.j));
    const pf = e.target.closest(".play-first");
    if (pf) return play(pf.dataset.uci);
    const line = e.target.closest(".line");
    if (line) showLineArrows(Number(line.dataset.i));
  });
  $("lines").addEventListener("focusin", (e) => {
    const line = e.target.closest(".line");
    if (line && !e.target.closest(".mv")) showLineArrows(Number(line.dataset.i));
  });

  // Analyse tools: set up a position, read one from a photo.
  $("setUp").addEventListener("click", () => startEditor(app.vst.fen.split(" ")[0], app.vst.turn === "white"));
  $("photoNew").addEventListener("click", () => {
    $("photoInput").dataset.how = "new";
    $("photoInput").click();
  });
  $("photoNext").addEventListener("click", () => {
    $("photoInput").dataset.how = "next";
    $("photoInput").click();
  });
  $("photoInput").addEventListener("change", (e) => {
    const f = e.target.files?.[0];
    e.target.value = "";
    if (f) fromPhoto(f, e.target.dataset.how);
  });
  $("editWhite").addEventListener("click", () => ((app.editing.white = true), renderEditor()));
  $("editBlack").addEventListener("click", () => ((app.editing.white = false), renderEditor()));
  $("editCancel").addEventListener("click", cancelEditor);
  $("editDone").addEventListener("click", finishEditor);

  // Settings dialog.
  const dlg = $("settings");
  const levelText = (v) => (Number(v) >= 9 ? "max" : v);
  $("openSettings").addEventListener("click", () => {
    $("level").value = settings.level;
    $("levelOut").textContent = levelText(settings.level);
    $("think").value = settings.thinkSecs;
    $("thinkOut").textContent = settings.thinkSecs;
    $("side").value = settings.side;
    $("depth").value = settings.depth;
    $("plies").value = String(settings.plies);
    $("siteBase").value = settings.siteBase;
    dlg.showModal();
  });
  $("level").addEventListener("input", (e) => ($("levelOut").textContent = levelText(e.target.value)));
  $("think").addEventListener("input", (e) => ($("thinkOut").textContent = e.target.value));
  dlg.addEventListener("close", () => {
    settings.level = Number($("level").value);
    settings.thinkSecs = Number($("think").value);
    settings.side = $("side").value;
    settings.depth = $("depth").value;
    settings.plies = Number($("plies").value);
    const base = $("siteBase").value.trim().replace(/\/+$/, "");
    const baseChanged = base && base !== settings.siteBase;
    if (base) settings.siteBase = base;
    if (baseChanged) loadSiteName(base);
    for (const k of ["level", "thinkSecs", "side", "depth", "plies", "siteBase"]) store.set(k, settings[k]);
    app.analysisFor = "";
    if (baseChanged && app.online.serverKey === "site") connectOnline();
    if (!app.editing) refresh();
  });

  wireOnline();
  setInterval(() => app.mode === "online" && onlineLive() && renderClocks(), 250);
}

function renderDictionary() {
  $("glossary").innerHTML = JSON.parse(glossary())
    .map(([k, v]) => `<div><dt>${esc(k)}</dt><dd>${esc(v)}</dd></div>`)
    .join("");
  $("motifs").innerHTML = JSON.parse(motifs())
    .map((m) => `<div><dt>${esc(m.name)}</dt><dd>${esc(m.definition)}</dd></div>`)
    .join("");
  $("book").innerHTML = JSON.parse(openingBook())
    .map((o) => `<li><span class="eco">${esc(o.eco)}</span> ${esc(o.name)}</li>`)
    .join("");
}

// ---------- start

async function main() {
  await init();
  analysisEngine = new Engine();
  botEngine = new Engine();
  board = new Board($("board"), {
    onMove: play,
    onSelect: (sq) => {
      app.selected = sq;
      renderOptions();
    },
  });
  wire();
  startReview({ api: siteApi });
  renderDictionary();
  app.session = new Session(null);

  if ("serviceWorker" in navigator) navigator.serviceWorker.register("sw.js").catch(() => {});

  let signedIn = null;
  try {
    signedIn = await finishSignIn();
  } catch (e) {
    app.online.error = e.message;
  }
  if (signedIn) {
    store.set(`token:${signedIn.serverKey}`, signedIn.token);
    store.set("server", signedIn.serverKey);
    app.online.serverKey = signedIn.serverKey;
    app.mode = "online";
  }
  // Opened from a board device: show the board's game.
  await siteLoaded;
  if (!signedIn && SERVERS.site.device && settings.siteBase === SERVERS.site.base) {
    app.online.serverKey = "site";
    app.mode = "online";
  }
  setMode(["bot", "local", "analyse", "online"].includes(app.mode) ? app.mode : "bot");
}

main().catch((e) => {
  document.body.insertAdjacentHTML("afterbegin", `<p class="error fatal">Chess lab couldn't start: ${esc(e.message || e)}. Reload the page; if it keeps happening, your browser may not support WebAssembly.</p>`);
});
