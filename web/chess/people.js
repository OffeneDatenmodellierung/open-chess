// Players and running the server: #@/<name> is a player's profile (their
// ratings and latest games; staff see more), #admin the admin page
// (accounts and their roles, partners). Both need this server's sign-in:
// nothing about players is shown to anyone signed out.
//
// An admin's or manager's rights need a passkey check, done here with the
// app's token (the server's Account page adds passkeys).

const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
const PERFS = [["bullet", "Bullet"], ["blitz", "Blitz"], ["rapid", "Rapid"], ["classical", "Classical"], ["correspondence", "Correspondence"]];
const ONLINE_MS = 10 * 60 * 1000;

let getApi = () => null;
let me = null; // /api/account, for the signed-in token
let meFor = null; // the token `me` is for
let partnerToken = null; // a partner's token, shown once

const $ = (id) => document.getElementById(id);
const profileView = () => $("profileView");
const adminView = () => $("adminView");
const profileLink = (name) => `<a href="#@/${encodeURIComponent(name)}">${esc(name)}</a>`;

function when(ms) {
  if (!ms) return "never";
  const d = new Date(ms);
  const days = Math.floor((Date.now() - ms) / 86_400_000);
  if (days < 1) return d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" }) + " today";
  if (days < 30) return `${days} day${days === 1 ? "" : "s"} ago`;
  return d.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}

/// Who's signed in here (cached per token). Null when signed out.
export async function whoAmI(force = false) {
  const api = getApi();
  if (!api) {
    me = meFor = null;
    return null;
  }
  if (!force && meFor === api.token) return me;
  // Asked once per token (and again when forced), even when it fails.
  meFor = api.token;
  try {
    me = await api.account();
  } catch {
    me = null;
  }
  return me;
}

const isAdmin = () => !!me?.roles?.includes("admin");
const isStaff = () => !!me?.roles?.length;
const checked = () => me?.staff === "checked";

/// The header's Profile and Admin buttons, for who's signed in.
export async function refreshPeopleNav() {
  const u = await whoAmI();
  const p = $("openProfile");
  if (p) {
    p.hidden = !u;
    if (u) p.href = `#@/${encodeURIComponent(u.username)}`;
  }
  const a = $("openAdmin");
  if (a) a.hidden = !(u && isAdmin());
}

// ---------- the passkey check

function staffBanner() {
  if (!isStaff() || checked()) return "";
  const what = isAdmin() ? "admin" : "manager";
  if (!me.passkeys) {
    return `<div class="card notice"><p>You're ${what === "admin" ? "an admin" : "a manager"} here. Your rights need a passkey check: <a href="${esc(me.accountUrl)}" target="_blank" rel="noopener">add a passkey on your account page</a>, then come back.</p></div>`;
  }
  const framed = window.top !== window;
  return `<div class="card notice"><p>Your ${what} rights need a passkey check.</p>
    ${framed
      ? `<p class="hint">Passkeys don't work inside another site's page: <a href="${esc(location.href)}" target="_blank" rel="noopener">open the Chess lab on its own</a>.</p>`
      : `<button type="button" class="primary" data-act="staff-check">Check with your passkey</button>`}
    <p class="error" id="staffErr" role="alert"></p></div>`;
}

async function staffCheck(again) {
  const err = $("staffErr");
  try {
    await getApi().staffCheck();
    await whoAmI(true);
    again();
  } catch (e) {
    if (err) err.textContent = e.name === "NotAllowedError" ? "The passkey request was cancelled or timed out." : e.message;
  }
}

// ---------- profiles

function signInFirst(view, what) {
  view.innerHTML = `<div class="people"><p><a class="back" href="#">← Back to the board</a></p>
    <h1>${esc(what)}</h1><p>Sign in to this server under <b>Online</b> to see players.</p></div>`;
}

function searchBox() {
  return `<form class="player-search" data-act="search" role="search">
    <input id="playerFind" list="playerNames" placeholder="Find a player" autocomplete="off" autocapitalize="off" spellcheck="false" aria-label="Find a player">
    <datalist id="playerNames"></datalist><button type="submit">Go</button></form>`;
}

function result(g, name) {
  const lower = name.toLowerCase();
  const white = g.players.white.user?.name || "?";
  const black = g.players.black.user?.name || "?";
  const mine = white.toLowerCase() === lower ? "white" : black.toLowerCase() === lower ? "black" : null;
  if (g.status === "started" || g.status === "created") return { text: "playing", cls: "" };
  if (!g.winner) return { text: g.status === "aborted" ? "aborted" : "draw", cls: "draw" };
  if (!mine) return { text: `${g.winner} won`, cls: "" };
  return g.winner === mine ? { text: "won", cls: "win" } : { text: "lost", cls: "loss" };
}

async function showProfile(name) {
  const view = profileView();
  const api = getApi();
  if (!api) return signInFirst(view, "Players");
  view.innerHTML = `<div class="people"><p><a class="back" href="#">← Back to the board</a></p>${searchBox()}<p class="hint">Loading ${esc(name)}…</p></div>`;
  wireSearch(view);
  await whoAmI();
  let u;
  try {
    u = await api.user(name);
  } catch (e) {
    view.querySelector(".hint").outerHTML = `<p class="error">${/404/.test(e.message) ? `No player here is called ${esc(name)}.` : esc(e.message)}</p>`;
    return;
  }
  const games = await api.gamesOf(u.username, 20).catch(() => []);
  const own = me && me.username.toLowerCase() === u.username.toLowerCase();
  const online = u.seenAt && Date.now() - u.seenAt < ONLINE_MS;
  const perfs = PERFS.filter(([k]) => u.perfs?.[k]?.games).map(
    ([k, label]) => `<li><span>${label}</span><b>${u.perfs[k].rating}${u.perfs[k].prov ? "?" : ""}</b><span class="hint">${u.perfs[k].games} game${u.perfs[k].games === 1 ? "" : "s"}</span></li>`,
  );
  const c = u.count || {};
  document.title = `${u.username} · Chess lab`;
  view.innerHTML = `<div class="people">
    <p><a class="back" href="#">← Back to the board</a></p>
    ${searchBox()}
    <header class="profile-head">
      <h1>${u.title === "BOT" ? '<span class="tag">BOT</span> ' : ""}${esc(u.username)}${online ? ' <span class="online" title="Online">●</span>' : ""}</h1>
      <p class="hint">Joined ${when(u.createdAt)} · last seen ${online ? "now" : when(u.seenAt)}${u.tosViolation ? ' · <span class="error">fair-play violation</span>' : ""}</p>
      <div class="row">
        ${own ? `<a class="button" href="${esc(me.accountUrl)}" target="_blank" rel="noopener">Account settings and passkeys</a>` : u.title === "BOT" ? "" : `<button type="button" data-act="challenge" data-name="${esc(u.username)}">Challenge</button>`}
      </div>
    </header>
    ${own ? staffBanner() : ""}
    <section class="card"><h2>Ratings</h2>${perfs.length ? `<ul class="perfs">${perfs.join("")}</ul>` : `<p class="hint">No rated games yet.</p>`}
      <p class="hint">${c.all || 0} games · ${c.win || 0} won · ${c.loss || 0} lost · ${c.draw || 0} drawn${c.playing ? ` · ${c.playing} playing now` : ""}</p></section>
    <section class="card"><h2>Latest games</h2>${games.length
      ? `<div class="table-wrap"><table class="games"><thead><tr><th>When</th><th>White</th><th>Black</th><th>Result</th><th>Kind</th><th></th></tr></thead><tbody>${games
          .map((g) => {
            const r = result(g, u.username);
            const w = g.players.white, b = g.players.black;
            const clock = g.clock ? `${g.clock.initial / 60}+${g.clock.increment}` : "untimed";
            return `<tr><td>${when(g.createdAt)}</td>
              <td>${profileLink(w.user?.name || "?")} <span class="hint">${w.rating ?? ""}</span></td>
              <td>${profileLink(b.user?.name || "?")} <span class="hint">${b.rating ?? ""}</span></td>
              <td class="${r.cls}">${r.text}</td><td>${esc(g.perf)} ${clock}${g.rated ? " rated" : ""}</td>
              <td><button type="button" class="link" data-act="pgn" data-id="${esc(g.id)}">PGN</button></td></tr>`;
          })
          .join("")}</tbody></table></div>`
      : `<p class="hint">No games yet.</p>`}</section>
    ${isStaff() && checked() && !own ? `<section class="card" id="staffBox"><h2>Staff</h2><p class="hint">Loading…</p></section>` : ""}
  </div>`;
  if (isStaff() && checked() && !own) staffSection(u.username);
}

async function staffSection(name) {
  const box = $("staffBox");
  const api = getApi();
  const parts = [];
  if (isAdmin()) {
    try {
      const a = await api.adminPlayer(name);
      parts.push(`<p>${a.email ? `${esc(a.email)}${a.emailConfirmed ? " (confirmed)" : " (not confirmed)"}` : "No email"} ·
        ${a.password ? "password" : "no password"} · ${a.passkeys} passkey${a.passkeys === 1 ? "" : "s"} · ${a.tokens} token${a.tokens === 1 ? "" : "s"}${a.linked ? " · linked to the host site" : ""}${a.disabledAt ? ` · <span class="error">disabled ${when(a.disabledAt)}</span>` : ""}</p>
        <div class="row">
          <label class="check"><input type="checkbox" data-role="admin" ${a.admin ? "checked" : ""}> Admin</label>
          <label class="check"><input type="checkbox" data-role="manager" ${a.manager ? "checked" : ""}> Manager</label>
          <label class="check"><input type="checkbox" data-role="disabled" ${a.disabledAt ? "checked" : ""}> Disabled</label>
        </div>
        ${a.tokenList?.length ? `<details><summary>Tokens (${a.tokenList.length})</summary><ul class="plain">${a.tokenList
          .map((t) => `<li><code>${esc(t.prefix)}…</code> ${esc(t.name)} <span class="hint">${esc(t.scopes.join(" "))} · used ${when(t.lastUsedAt)}</span> <button type="button" class="link" data-act="revoke-token" data-id="${t.id}">Revoke</button></li>`)
          .join("")}</ul><button type="button" data-act="revoke-tokens">Revoke all</button></details>` : ""}
        ${a.passkeys ? `<p><button type="button" data-act="reset-passkeys">Remove their passkeys</button></p>` : ""}`);
    } catch (e) {
      parts.push(`<p class="error">${esc(e.message)}</p>`);
    }
  }
  try {
    const f = await api.fairplayPlayer(name);
    parts.push(`<h3>Fair play</h3><p>${f.screened} game${f.screened === 1 ? "" : "s"} screened, ${f.flagged} flagged · mean score ${f.meanScore.toFixed(2)} (${f.meanScore30d.toFixed(2)} in 30 days)${f.marked ? ' · <span class="error">marked</span>' : ""}</p>
      ${f.reviews?.length ? `<ul class="plain">${f.reviews.slice(0, 5).map((r) => `<li>${esc(r.verdict)} ${r.gameId ? `(${esc(r.gameId)})` : ""} by ${esc(r.reviewer || "?")}, ${when(r.at)}${r.note ? `: ${esc(r.note)}` : ""}</li>`).join("")}</ul>` : ""}`);
  } catch (e) {
    parts.push(`<p class="hint">Fair play: ${esc(e.message)}</p>`);
  }
  box.innerHTML = `<h2>Staff</h2>${parts.join("")}<p class="error" id="staffErr" role="alert"></p>`;
  box.dataset.name = name;
}

function wireSearch(view) {
  const input = view.querySelector("#playerFind");
  if (!input) return;
  let t = null;
  input.addEventListener("input", () => {
    clearTimeout(t);
    const term = input.value.trim();
    if (term.length < 2) return;
    t = setTimeout(async () => {
      try {
        const r = await getApi().autocomplete(term);
        $("playerNames").innerHTML = (r.result || []).map((p) => `<option value="${esc(p.name)}">`).join("");
      } catch {}
    }, 200);
  });
}

// ---------- the admin page

async function showAdmin() {
  const view = adminView();
  const api = getApi();
  if (!api) return signInFirst(view, "Admin");
  await whoAmI(true);
  document.title = "Admin · Chess lab";
  if (!isAdmin()) {
    view.innerHTML = `<div class="people"><p><a class="back" href="#">← Back to the board</a></p><h1>Admin</h1><p>This page is for this server's admins.</p></div>`;
    return;
  }
  if (!checked()) {
    view.innerHTML = `<div class="people"><p><a class="back" href="#">← Back to the board</a></p><h1>Admin</h1>${staffBanner()}</div>`;
    return;
  }
  view.innerHTML = `<div class="people">
    <p><a class="back" href="#">← Back to the board</a></p>
    <h1>Admin</h1>
    <section class="card"><h2>Accounts</h2>
      <form class="player-search" data-act="admin-search" role="search">
        <input id="adminFind" placeholder="Name or email" autocomplete="off" aria-label="Name or email">
        <label class="check"><input type="checkbox" id="adminStaff"> Admins and managers only</label>
        <button type="submit">Search</button></form>
      <div id="adminPlayers"><p class="hint">Loading…</p></div>
      <p class="hint">Open a player to change their roles, disable them, or revoke their tokens. New admins and managers need a passkey to use their rights.</p>
    </section>
    <section class="card"><h2>Partners</h2>
      <p class="hint">Other services' access to this server's API: a token with what it may do. Give the token to the service once; revoke it here.</p>
      <div id="adminPartners"><p class="hint">Loading…</p></div>
    </section>
    <p class="error" id="adminErr" role="alert"></p>
  </div>`;
  listPlayers();
  listPartners();
}

async function listPlayers() {
  const box = $("adminPlayers");
  try {
    const r = await getApi().adminPlayers($("adminFind")?.value || "", $("adminStaff")?.checked);
    box.innerHTML = r.players.length
      ? `<div class="table-wrap"><table class="games"><thead><tr><th>Player</th><th>Email</th><th>Roles</th><th>Sign-in</th><th>Last seen</th></tr></thead><tbody>${r.players
          .map((p) => `<tr><td>${profileLink(p.username)}${p.bot ? ' <span class="tag">BOT</span>' : ""}${p.disabledAt ? ' <span class="error">disabled</span>' : ""}</td>
            <td>${esc(p.email || "")}</td>
            <td>${[p.admin && "admin", p.manager && "manager"].filter(Boolean).join(", ")}</td>
            <td class="hint">${[p.password && "password", p.passkeys && `${p.passkeys} passkey${p.passkeys === 1 ? "" : "s"}`, p.linked && "host site"].filter(Boolean).join(", ") || "tokens only"}</td>
            <td>${when(p.seenAt)}</td></tr>`)
          .join("")}</tbody></table></div>`
      : `<p class="hint">Nobody matches.</p>`;
  } catch (e) {
    box.innerHTML = `<p class="error">${esc(e.message)}</p>`;
  }
}

async function listPartners() {
  const box = $("adminPartners");
  try {
    const r = await getApi().partners();
    box.innerHTML = `${partnerToken ? `<div class="notice card"><p>The new token, shown once: copy it now and give it to the service.</p><p><code class="token">${esc(partnerToken)}</code> <button type="button" data-act="copy-partner">Copy</button></p></div>` : ""}
      ${r.partners.length
        ? `<ul class="plain partners">${r.partners
            .map((p) => `<li><b>${esc(p.name)}</b> <code>${esc(p.prefix)}…</code> <span class="hint">${esc(p.scopes.join(", "))} · made ${when(p.createdAt)} · used ${when(p.lastUsedAt)}</span> <button type="button" class="link" data-act="revoke-partner" data-id="${p.id}" data-name="${esc(p.name)}">Revoke</button></li>`)
            .join("")}</ul>`
        : `<p class="hint">No partners yet.</p>`}
      ${r.sharedSecret ? `<p class="hint">The host site can also call with its shared secret (<code>ESPORTS_SECRET</code>). Give it a partner token with <b>read</b> and <b>site</b> instead; the secret then only signs its players in.</p>` : ""}
      <form class="new-partner" data-act="new-partner">
        <label class="field"><span>New partner</span><input id="partnerName" placeholder="e.g. esports-lab" maxlength="60" required></label>
        ${r.scopes.map((s) => `<label class="check"><input type="checkbox" name="scope" value="${esc(s.scope)}" ${s.scope === "read" ? "checked" : ""}> <span><b>${esc(s.scope)}</b>: ${esc(s.about)}</span></label>`).join("")}
        <button type="submit" class="primary">Make the token</button>
      </form>`;
  } catch (e) {
    box.innerHTML = `<p class="error">${esc(e.message)}</p>`;
  }
}

// ---------- wiring

function current() {
  const m = /^#@\/([^/?#]+)/.exec(location.hash);
  if (m) return { page: "profile", name: decodeURIComponent(m[1]) };
  if (/^#admin\b/.test(location.hash)) return { page: "admin" };
  return null;
}

async function act(e) {
  const t = e.target.closest("[data-act]");
  if (!t) return;
  const api = getApi();
  const name = $("staffBox")?.dataset.name;
  const fail = (err) => {
    const box = $("staffErr") || $("adminErr");
    if (box) box.textContent = err.message;
  };
  try {
    switch (t.dataset.act) {
      case "staff-check":
        return staffCheck(() => route());
      case "challenge":
        location.hash = "";
        dispatchEvent(new CustomEvent("open-chess:challenge", { detail: t.dataset.name }));
        return;
      case "pgn": {
        const pgn = await api.pgn(t.dataset.id);
        const url = URL.createObjectURL(new Blob([pgn], { type: "application/x-chess-pgn" }));
        const a = Object.assign(document.createElement("a"), { href: url, download: `${t.dataset.id}.pgn` });
        a.click();
        setTimeout(() => URL.revokeObjectURL(url), 1000);
        return;
      }
      case "revoke-token":
        await api.adminRevokeTokens(name, Number(t.dataset.id));
        return staffSection(name);
      case "revoke-tokens":
        if (!confirm(`Revoke all of ${name}'s tokens? Their apps and boards will need to sign in again.`)) return;
        await api.adminRevokeTokens(name);
        return staffSection(name);
      case "reset-passkeys": {
        if (!confirm(`Remove ${name}'s passkeys? Without a password or the host site's sign-in, they won't be able to sign in.`)) return;
        await api.adminResetPasskeys(name);
        return staffSection(name);
      }
      case "revoke-partner":
        if (!confirm(`Revoke ${t.dataset.name}'s token? The service stops reaching this server at once.`)) return;
        await api.revokePartner(Number(t.dataset.id));
        partnerToken = null;
        return listPartners();
      case "copy-partner":
        await navigator.clipboard.writeText(partnerToken);
        t.textContent = "Copied";
        return;
    }
  } catch (err) {
    fail(err);
  }
}

async function submit(e) {
  const f = e.target.closest("form[data-act]");
  if (!f) return;
  e.preventDefault();
  try {
    if (f.dataset.act === "search") {
      const v = $("playerFind").value.trim();
      if (v) location.hash = `#@/${encodeURIComponent(v)}`;
    } else if (f.dataset.act === "admin-search") {
      listPlayers();
    } else if (f.dataset.act === "new-partner") {
      const scopes = [...f.querySelectorAll("input[name=scope]:checked")].map((i) => i.value);
      const r = await getApi().newPartner($("partnerName").value, scopes);
      partnerToken = r.token;
      listPartners();
    }
  } catch (err) {
    const box = $("adminErr");
    if (box) box.textContent = err.message;
  }
}

async function change(e) {
  const box = e.target.closest("#staffBox");
  const role = e.target.dataset.role;
  if (!box || !role) return;
  const name = box.dataset.name;
  try {
    await getApi().adminChange(name, { [role]: e.target.checked });
    staffSection(name);
  } catch (err) {
    e.target.checked = !e.target.checked;
    $("staffErr").textContent = err.message;
  }
}

function route() {
  const c = current();
  if (c?.page === "profile") showProfile(c.name);
  else if (c?.page === "admin") showAdmin();
}

/// Wire it up: `api` returns the signed-in server's API (or null).
export function startPeople({ api }) {
  getApi = api;
  for (const v of [profileView(), adminView()]) {
    v.addEventListener("click", act);
    v.addEventListener("submit", submit);
    v.addEventListener("change", change);
  }
  addEventListener("open-chess:page", (e) => {
    if (e.detail === "profile" || e.detail === "admin") route();
  });
  refreshPeopleNav();
}
