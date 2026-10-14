// A board for correcting a position by hand: drag pieces to move them
// (off the board to remove them), drag one in from the palette, or click a
// square to change what's on it. Used by the Chess lab's board editor
// (after reading a photo) and by sites that review board photos (the
// esports site's Review page imports it from the chess server).
//
//   import { PositionEditor } from "<chess server>/app/position-editor.js";
//   const ed = new PositionEditor(el, { placement, unsure: ["e4"], onChange: (p) => … });
//   ed.placement   // "rnbqkbnr/pppppppp/8/…" (FEN's first field)
//
// No dependencies; it brings its own styles (themeable with the Chess lab's
// CSS variables: --sq-light, --sq-dark, --amber, --panel, --ink, --rule).

const FILES = "abcdefgh";
const GLYPH = { k: "♚", q: "♛", r: "♜", b: "♝", n: "♞", p: "♟" };
const NAME = { k: "king", q: "queen", r: "rook", b: "bishop", n: "knight", p: "pawn" };
const PIECES = ["K", "Q", "R", "B", "N", "P", "k", "q", "r", "b", "n", "p"];
const START = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR";
// A press that moves less than this is a click, not a drag.
const DRAG_PX = 5;

const CSS = `
.pe { --pe-light: var(--sq-light, #dde4ea); --pe-dark: var(--sq-dark, #7e93a6); --pe-mark: var(--amber, #e0a22e);
  --pe-panel: var(--panel, #f7f9fa); --pe-ink: var(--ink, #18212b); --pe-rule: var(--rule, #c9d1d9);
  position: relative; display: grid; gap: 0.5rem; user-select: none; -webkit-user-select: none; }
.pe-board { position: relative; display: grid; grid-template-columns: repeat(8, 1fr); grid-template-rows: repeat(8, 1fr); aspect-ratio: 1; width: 100%;
  border-radius: 4px; overflow: hidden; touch-action: none; }
.pe-sq { position: relative; display: grid; place-items: center; min-width: 0; min-height: 0; padding: 0; margin: 0;
  border: 0; border-radius: 0; cursor: pointer; font-size: var(--pe-size, clamp(1.2rem, 7vw, 3.2rem)); line-height: 1;
  background: var(--pe-light); color: inherit; }
.pe-sq.d { background: var(--pe-dark); }
.pe-sq:focus-visible { outline: 3px solid var(--pe-mark); outline-offset: -3px; z-index: 1; }
.pe-sq.mark { box-shadow: inset 0 0 0 4px var(--pe-mark); }
.pe-sq.over { box-shadow: inset 0 0 0 100vmax rgba(224, 162, 46, 0.55); }
.pe-sq.lifted .pe-piece { opacity: 0.25; }
.pe-sq[data-rank]::before, .pe-sq[data-file]::after { position: absolute; font: 700 0.62rem/1 system-ui, sans-serif; opacity: 0.8; }
.pe-sq[data-rank]::before { content: attr(data-rank); top: 2px; left: 3px; color: var(--pe-dark); }
.pe-sq[data-file]::after { content: attr(data-file); bottom: 2px; right: 3px; color: var(--pe-dark); }
.pe-sq.d[data-rank]::before, .pe-sq.d[data-file]::after { color: var(--pe-light); }
.pe-piece { pointer-events: none; font-family: "DejaVu Sans", "Segoe UI Symbol", "Apple Symbols", "Noto Sans Symbols 2", serif; }
.pe-piece.w { color: #fbfcfd; -webkit-text-stroke: 1.5px #18212b; paint-order: stroke fill; }
.pe-piece.b { color: #18212b; -webkit-text-stroke: 1px #f3f5f7; paint-order: stroke fill; }
.pe-palette { display: grid; grid-template-columns: repeat(13, 1fr); gap: 0.2rem; touch-action: none; }
.pe-palette button, .pe-pick button { padding: 0; min-height: 40px; display: grid; place-items: center; font-size: 1.5rem; line-height: 1;
  border: 1px solid var(--pe-rule); border-radius: 6px; background: var(--pe-light); color: #18212b; cursor: grab; }
.pe-palette button[aria-pressed="true"], .pe-pick button[aria-pressed="true"] { outline: 3px solid var(--pe-mark); outline-offset: -3px; }
.pe-palette .pe-none, .pe-pick .pe-none { font: 600 0.68rem/1 system-ui, sans-serif; overflow: hidden; }
.pe-tools { display: flex; flex-wrap: wrap; gap: 0.4rem; align-items: center; font: 0.85rem/1.3 system-ui, sans-serif; color: var(--pe-ink); }
.pe-tools button { min-height: 32px; padding: 0 0.7rem; border: 1px solid var(--pe-rule); border-radius: 6px; background: var(--pe-panel); color: var(--pe-ink); cursor: pointer; font: inherit; }
.pe-tools button:disabled { opacity: 0.45; cursor: default; }
.pe-hint { opacity: 0.75; flex: 1 1 100%; }
.pe-pick { position: absolute; z-index: 5; display: grid; grid-template-columns: repeat(6, 2.6rem); gap: 0.2rem; padding: 0.4rem;
  border: 1px solid var(--pe-rule); border-radius: 10px; background: var(--pe-panel); box-shadow: 0 6px 24px rgba(0, 0, 0, 0.3); }
.pe-pick button { cursor: pointer; }
.pe-pick .pe-none { grid-column: span 6; min-height: 32px; }
.pe-ghost { position: fixed; z-index: 1000; pointer-events: none; transform: translate(-50%, -50%); font-size: var(--pe-ghost, 3rem); line-height: 1; }
`;

function injectStyles() {
  if (document.getElementById("position-editor-css")) return;
  const s = document.createElement("style");
  s.id = "position-editor-css";
  s.textContent = CSS;
  document.head.append(s);
}

/// The squares of a FEN placement: { e4: "P", … }; null if it isn't one.
export function parsePlacement(placement) {
  const rows = String(placement || "").trim().split(" ")[0].split("/");
  if (rows.length !== 8) return null;
  const grid = {};
  for (let i = 0; i < 8; i++) {
    let f = 0;
    for (const c of rows[i]) {
      if (/[1-8]/.test(c)) f += Number(c);
      else if (/[KQRBNPkqrbnp]/.test(c) && f < 8) grid[FILES[f++] + (8 - i)] = c;
      else return null;
    }
    if (f !== 8) return null;
  }
  return grid;
}

export function placementOf(grid) {
  const rows = [];
  for (let r = 8; r >= 1; r--) {
    let row = "";
    let empty = 0;
    for (const f of FILES) {
      const c = grid[f + r];
      if (!c) empty++;
      else {
        if (empty) row += empty;
        empty = 0;
        row += c;
      }
    }
    rows.push(row + (empty || ""));
  }
  return rows.join("/");
}

const pieceName = (c) => (c ? `${c === c.toUpperCase() ? "White" : "Black"} ${NAME[c.toLowerCase()]}` : "Empty");
const glyphHtml = (c) => `<span class="pe-piece ${c === c.toUpperCase() ? "w" : "b"}">${GLYPH[c.toLowerCase()]}︎</span>`;

export class PositionEditor {
  /// options: placement, unsure (squares to check), white (White at the
  /// bottom), onChange(placement), tools (show Undo / Flip / Clear /
  /// Start, default true), hint (show the how-to line, default true).
  constructor(root, options = {}) {
    injectStyles();
    this.root = root;
    this.onChange = options.onChange || null;
    this.white = options.white !== false;
    this.grid = parsePlacement(options.placement) || {};
    this.unsure = new Set(options.unsure || []);
    this.armed = null; // a palette piece placed by clicking squares ("" erases)
    this.history = [];
    this.root.classList.add("pe");
    this.root.innerHTML = "";
    this.boardEl = document.createElement("div");
    this.boardEl.className = "pe-board";
    this.boardEl.setAttribute("role", "grid");
    this.boardEl.setAttribute("aria-label", "Position: drag pieces, or select a square and type a piece letter");
    this.paletteEl = document.createElement("div");
    this.paletteEl.className = "pe-palette";
    this.paletteEl.setAttribute("role", "toolbar");
    this.paletteEl.setAttribute("aria-label", "Pieces to place");
    this.toolsEl = document.createElement("div");
    this.toolsEl.className = "pe-tools";
    this.root.append(this.boardEl, this.paletteEl);
    if (options.tools !== false || options.hint !== false) this.root.append(this.toolsEl);
    this.showTools = options.tools !== false;
    this.showHint = options.hint !== false;
    this.build();
    this.boardEl.addEventListener("pointerdown", (e) => this.press(e, "board"));
    this.paletteEl.addEventListener("pointerdown", (e) => this.press(e, "palette"));
    this.boardEl.addEventListener("contextmenu", (e) => {
      const sq = e.target.closest("[data-sq]")?.dataset.sq;
      if (!sq) return;
      e.preventDefault();
      this.choose(sq);
    });
    this.boardEl.addEventListener("keydown", (e) => this.key(e));
    // Enter or Space on a focused button (pointer clicks are handled above).
    this.root.addEventListener("click", (e) => {
      if (e.detail !== 0) return;
      const sq = e.target.closest(".pe-board [data-sq]")?.dataset.sq;
      const piece = e.target.closest(".pe-palette [data-piece]")?.dataset.piece;
      if (sq) this.click("board", sq);
      else if (piece != null) this.click("palette", null, piece);
    });
    this.closeOnOutside = (e) => {
      if (this.pickEl && !this.pickEl.contains(e.target)) this.closePick();
    };
  }

  get placement() {
    return placementOf(this.grid);
  }

  /// Replace the position (not undoable; clears the history).
  set(placement, unsure) {
    const g = parsePlacement(placement);
    if (!g) return false;
    this.grid = g;
    if (unsure) this.unsure = new Set(unsure);
    this.history = [];
    this.render();
    return true;
  }

  flip(white = !this.white) {
    this.white = white;
    this.build();
  }

  build() {
    this.boardEl.innerHTML = "";
    this.squares = {};
    for (let r = 0; r < 8; r++) {
      for (let c = 0; c < 8; c++) {
        const file = this.white ? FILES[c] : FILES[7 - c];
        const rank = this.white ? 8 - r : r + 1;
        const sq = file + rank;
        const b = document.createElement("button");
        b.type = "button";
        b.className = "pe-sq" + ((FILES.indexOf(file) + rank) % 2 === 1 ? " d" : ""); // a1 is dark
        b.dataset.sq = sq;
        if (c === 0) b.dataset.rank = rank;
        if (r === 7) b.dataset.file = file;
        this.boardEl.append(b);
        this.squares[sq] = b;
      }
    }
    this.render();
  }

  render() {
    for (const [sq, el] of Object.entries(this.squares)) {
      const p = this.grid[sq];
      el.innerHTML = p ? glyphHtml(p) : "";
      el.classList.toggle("mark", this.unsure.has(sq));
      el.setAttribute("aria-label", `${sq}: ${pieceName(p)}${this.unsure.has(sq) ? ", check this square" : ""}`);
    }
    this.paletteEl.innerHTML =
      PIECES.map((c) => `<button type="button" data-piece="${c}" aria-pressed="${this.armed === c}" aria-label="${pieceName(c)}" title="${pieceName(c)}: drag onto a square, or select it and click squares">${glyphHtml(c)}</button>`).join("") +
      `<button type="button" data-piece="" class="pe-none" aria-pressed="${this.armed === ""}" aria-label="Clear squares" title="Clear squares: select it and click squares">✕</button>`;
    if (this.toolsEl.isConnected) {
      const hint = this.armed != null
        ? `${this.armed ? pieceName(this.armed) : "Clearing"}: click squares. Click it again to stop.`
        : `Drag pieces to move them (off the board removes one). Click a square to change it.${this.unsure.size ? " Ringed squares weren't clear in the photo." : ""}`;
      this.toolsEl.innerHTML =
        (this.showHint ? `<span class="pe-hint">${hint}</span>` : "") +
        (this.showTools
          ? `<button type="button" data-tool="undo" ${this.history.length ? "" : "disabled"}>Undo</button>` +
            `<button type="button" data-tool="flip">Flip</button>` +
            `<button type="button" data-tool="clear">Clear all</button>` +
            `<button type="button" data-tool="start">Start position</button>`
          : "");
      this.toolsEl.onclick = (e) => {
        const t = e.target.closest("[data-tool]")?.dataset.tool;
        if (t === "undo") this.undo();
        else if (t === "flip") this.flip();
        else if (t === "clear") this.change(() => (this.grid = {}));
        else if (t === "start") this.change(() => (this.grid = parsePlacement(START)));
      };
    }
  }

  /// Make a change (undoable) and tell the page.
  change(fn) {
    const before = { ...this.grid };
    fn();
    if (placementOf(before) === this.placement) return this.render();
    this.history.push(before);
    if (this.history.length > 200) this.history.shift();
    this.render();
    this.onChange?.(this.placement);
  }

  undo() {
    const before = this.history.pop();
    if (!before) return;
    this.grid = before;
    this.render();
    this.onChange?.(this.placement);
  }

  setSquare(sq, piece) {
    this.change(() => {
      if (piece) this.grid[sq] = piece;
      else delete this.grid[sq];
      this.unsure.delete(sq);
    });
  }

  // ---- pointer: click or drag

  press(e, from) {
    if (e.button !== undefined && e.button !== 0) return;
    const target = from === "board" ? e.target.closest("[data-sq]") : e.target.closest("[data-piece]");
    if (!target) return;
    const sq = target.dataset.sq;
    const piece = from === "board" ? this.grid[sq] : target.dataset.piece;
    const start = { x: e.clientX, y: e.clientY };
    let ghost = null;
    let over = null;
    const el = e.currentTarget;
    el.setPointerCapture?.(e.pointerId);
    const move = (ev) => {
      if (!piece) return; // nothing to drag (an empty square, or Clear)
      if (!ghost && Math.hypot(ev.clientX - start.x, ev.clientY - start.y) < DRAG_PX) return;
      if (!ghost) {
        this.closePick();
        ghost = document.createElement("div");
        ghost.className = "pe-ghost";
        ghost.innerHTML = glyphHtml(piece);
        const size = this.boardEl.getBoundingClientRect().width / 8;
        ghost.style.setProperty("--pe-ghost", `${Math.max(28, size * 0.9)}px`);
        document.body.append(ghost);
        if (sq) this.squares[sq].classList.add("lifted");
      }
      ghost.style.left = `${ev.clientX}px`;
      ghost.style.top = `${ev.clientY}px`;
      const hit = this.squareAt(ev.clientX, ev.clientY);
      if (hit !== over) {
        if (over) this.squares[over]?.classList.remove("over");
        if (hit) this.squares[hit].classList.add("over");
        over = hit;
      }
    };
    const up = (ev) => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      el.removeEventListener("pointercancel", up);
      if (over) this.squares[over]?.classList.remove("over");
      if (sq) this.squares[sq]?.classList.remove("lifted");
      if (!ghost) return this.click(from, sq, target.dataset.piece);
      ghost.remove();
      if (ev.type === "pointercancel") return;
      const to = this.squareAt(ev.clientX, ev.clientY);
      if (from === "board") {
        if (to === sq) return;
        this.change(() => {
          delete this.grid[sq];
          if (to) this.grid[to] = piece; // off the board: removed
          this.unsure.delete(sq);
          if (to) this.unsure.delete(to);
        });
      } else if (to) {
        this.setSquare(to, piece);
      }
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", up);
  }

  squareAt(x, y) {
    const hit = document.elementFromPoint(x, y)?.closest?.("[data-sq]");
    return hit && this.boardEl.contains(hit) ? hit.dataset.sq : null;
  }

  click(from, sq, piece) {
    if (from === "palette") {
      this.armed = this.armed === piece ? null : piece;
      this.closePick();
      return this.render();
    }
    if (this.armed != null) {
      // Clicking the armed piece onto itself removes it, as in the app.
      return this.setSquare(sq, this.grid[sq] === this.armed ? "" : this.armed);
    }
    this.choose(sq);
  }

  // ---- the piece chooser for one square

  choose(sq) {
    this.closePick();
    const cur = this.grid[sq] || "";
    const pick = document.createElement("div");
    pick.className = "pe-pick";
    pick.setAttribute("role", "dialog");
    pick.setAttribute("aria-label", `What's on ${sq}?`);
    pick.innerHTML =
      PIECES.map((c) => `<button type="button" data-set="${c}" aria-pressed="${cur === c}" aria-label="${pieceName(c)}">${glyphHtml(c)}</button>`).join("") +
      `<button type="button" data-set="" class="pe-none" aria-pressed="${cur === ""}">Empty square</button>`;
    pick.addEventListener("click", (e) => {
      const b = e.target.closest("[data-set]");
      if (!b) return;
      this.closePick();
      this.setSquare(sq, b.dataset.set);
      this.squares[sq]?.focus();
    });
    pick.addEventListener("keydown", (e) => {
      if (e.key === "Escape") {
        this.closePick();
        this.squares[sq]?.focus();
      }
    });
    this.root.append(pick);
    // Next to the square, inside the editor.
    const box = this.root.getBoundingClientRect();
    const s = this.squares[sq].getBoundingClientRect();
    const w = pick.offsetWidth;
    const h = pick.offsetHeight;
    let left = s.left - box.left + s.width / 2 - w / 2;
    let top = s.bottom - box.top + 4;
    if (top + h > this.boardEl.offsetHeight && s.top - box.top - h - 4 >= 0) top = s.top - box.top - h - 4;
    left = Math.max(0, Math.min(left, box.width - w));
    pick.style.left = `${left}px`;
    pick.style.top = `${top}px`;
    this.pickEl = pick;
    pick.querySelector('[aria-pressed="true"]')?.focus();
    setTimeout(() => document.addEventListener("pointerdown", this.closeOnOutside), 0);
  }

  closePick() {
    this.pickEl?.remove();
    this.pickEl = null;
    document.removeEventListener("pointerdown", this.closeOnOutside);
  }

  // ---- keyboard: a letter sets the focused square, arrows move

  key(e) {
    const sq = e.target.closest?.("[data-sq]")?.dataset.sq;
    if (!sq) return;
    if (/^[KQRBNPkqrbnp]$/.test(e.key)) {
      e.preventDefault();
      this.setSquare(sq, e.key);
    } else if (e.key === "Delete" || e.key === "Backspace" || e.key === ".") {
      e.preventDefault();
      this.setSquare(sq, "");
    } else if (e.key.startsWith("Arrow")) {
      e.preventDefault();
      const dir = { ArrowUp: [0, 1], ArrowDown: [0, -1], ArrowLeft: [-1, 0], ArrowRight: [1, 0] }[e.key];
      const sign = this.white ? 1 : -1;
      const f = FILES.indexOf(sq[0]) + dir[0] * sign;
      const r = Number(sq[1]) + dir[1] * sign;
      if (f >= 0 && f < 8 && r >= 1 && r <= 8) this.squares[FILES[f] + r].focus();
    } else if ((e.ctrlKey || e.metaKey) && e.key === "z") {
      e.preventDefault();
      this.undo();
    }
  }
}
