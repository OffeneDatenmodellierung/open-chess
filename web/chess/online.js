// A small client for the Lichess Board API, which both lichess.org and
// an open-chess server speak. Sign-in is OAuth with PKCE (no client
// secret; both servers accept any client id), with a pasted personal
// token as a fallback.

const CLIENT_ID = "open-chess-pwa";
const SCOPES = "board:play challenge:read challenge:write";

export const SERVERS = {
  lichess: { name: "Lichess", base: "https://lichess.org" },
  // The name is replaced by the one the server gives (`GET /`, `site`).
  site: { name: "Chess server", base: defaultSiteBase() },
};

function defaultSiteBase() {
  // Served by the chess server itself (…/app/): the API is the same origin.
  const at = location.pathname.indexOf("/app/");
  if (at >= 0 && location.protocol.startsWith("http")) return location.origin + location.pathname.slice(0, at);
  // Otherwise a server named in the page (`<meta name="chess-server">`),
  // or this origin; either can be changed in the app's settings.
  const meta = document.querySelector('meta[name="chess-server"]')?.content;
  return meta || location.origin;
}

/// Ask a server what its site is called, for the app's labels, and
/// whether it's an open-chess board device (`"device": true`), which needs
/// no sign-in and plays on the board. Quietly keeps the defaults when it
/// can't say (offline, or not an open-chess server).
export async function loadSiteName(base) {
  try {
    const r = await fetch(base.replace(/\/+$/, "") + "/", { headers: { Accept: "application/json" } });
    if (!r.ok) return;
    const j = await r.json();
    if (typeof j.site === "string" && j.site.trim()) SERVERS.site.name = j.site.trim();
    SERVERS.site.device = j.device === true;
    // `"login": false`: the server has no browser sign-in, only tokens.
    SERVERS.site.noLogin = j.login === false;
  } catch {
    // keep the default
  }
}

/// The token a board device is sent: it has no accounts.
export const DEVICE_TOKEN = "device";

function b64url(bytes) {
  return btoa(String.fromCharCode(...new Uint8Array(bytes)))
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}

function redirectUri() {
  return location.origin + location.pathname;
}

/// Start the OAuth flow: navigates away and comes back with ?code=.
export async function beginSignIn(serverKey, base) {
  const verifier = b64url(crypto.getRandomValues(new Uint8Array(32)));
  const challenge = b64url(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier)));
  const state = b64url(crypto.getRandomValues(new Uint8Array(12)));
  sessionStorage.setItem("esl-chess:oauth", JSON.stringify({ verifier, state, serverKey, base }));
  const q = new URLSearchParams({
    response_type: "code",
    client_id: CLIENT_ID,
    redirect_uri: redirectUri(),
    code_challenge_method: "S256",
    code_challenge: challenge,
    scope: SCOPES,
    state,
  });
  location.assign(`${base}/oauth?${q}`);
}

/// Finish the OAuth flow if this page load is the redirect back.
/// Returns {serverKey, base, token} or null.
export async function finishSignIn() {
  const params = new URLSearchParams(location.search);
  const code = params.get("code");
  if (!code) return null;
  const saved = JSON.parse(sessionStorage.getItem("esl-chess:oauth") || "null");
  sessionStorage.removeItem("esl-chess:oauth");
  history.replaceState(null, "", redirectUri());
  if (!saved || saved.state !== params.get("state")) throw new Error("Sign-in was interrupted. Try signing in again.");
  const res = await fetch(`${saved.base}/api/token`, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "authorization_code",
      code,
      code_verifier: saved.verifier,
      redirect_uri: redirectUri(),
      client_id: CLIENT_ID,
    }),
  });
  if (!res.ok) throw new Error(`The server refused the sign-in (${res.status}).`);
  const body = await res.json();
  return { serverKey: saved.serverKey, base: saved.base, token: body.access_token };
}

/// Lichess's tablebase server: exact results for positions with up to
/// seven pieces. Free and open; only used when online.
export async function lichessTablebase(fen) {
  const r = await fetch(`https://tablebase.lichess.ovh/standard?fen=${encodeURIComponent(fen)}`, {
    signal: AbortSignal.timeout(5000),
  });
  if (!r.ok) throw new Error(`tablebase ${r.status}`);
  return r.json();
}

export class Api {
  constructor(base, token) {
    this.base = base.replace(/\/+$/, "");
    this.token = token;
  }

  headers(extra = {}) {
    return { Authorization: `Bearer ${this.token}`, ...extra };
  }

  async json(path) {
    const r = await fetch(this.base + path, { headers: this.headers() });
    if (!r.ok) throw new Error(`${path}: ${r.status} ${await r.text().catch(() => "")}`.trim());
    return r.json();
  }

  async post(path, form) {
    const r = await fetch(this.base + path, {
      method: "POST",
      headers: this.headers(form ? { "Content-Type": "application/x-www-form-urlencoded" } : {}),
      body: form ? new URLSearchParams(form) : undefined,
    });
    if (!r.ok) {
      let msg = `${r.status}`;
      try {
        const b = await r.json();
        msg = b.error || JSON.stringify(b);
      } catch {}
      throw new Error(msg);
    }
    return r.headers.get("content-type")?.includes("json") ? r.json() : null;
  }

  /// Read an ndjson stream; calls onLine for each object until aborted.
  async stream(path, onLine, signal) {
    const r = await fetch(this.base + path, { headers: this.headers(), signal });
    if (!r.ok || !r.body) throw new Error(`${path}: ${r.status}`);
    const reader = r.body.getReader();
    const dec = new TextDecoder();
    let buf = "";
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      buf += dec.decode(value, { stream: true });
      let i;
      while ((i = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, i).trim();
        buf = buf.slice(i + 1);
        if (line) onLine(JSON.parse(line));
      }
    }
  }

  account() {
    return this.json("/api/account");
  }
  events(onLine, signal) {
    return this.stream("/api/stream/event", onLine, signal);
  }
  game(id, onLine, signal) {
    return this.stream(`/api/board/game/stream/${id}`, onLine, signal);
  }
  move(id, uci) {
    return this.post(`/api/board/game/${id}/move/${uci}`);
  }
  resign(id) {
    return this.post(`/api/board/game/${id}/resign`);
  }
  abort(id) {
    return this.post(`/api/board/game/${id}/abort`);
  }
  draw(id, yes) {
    return this.post(`/api/board/game/${id}/draw/${yes ? "yes" : "no"}`);
  }
  challenge(user, { minutes, increment, color, rated }) {
    return this.post(`/api/challenge/${encodeURIComponent(user)}`, {
      rated: String(!!rated),
      "clock.limit": String(minutes * 60),
      "clock.increment": String(increment),
      color,
      variant: "standard",
    });
  }
  challengeAi(level, { minutes, increment, color }) {
    return this.post("/api/challenge/ai", {
      level: String(level),
      "clock.limit": String(minutes * 60),
      "clock.increment": String(increment),
      color,
    });
  }
  accept(id) {
    return this.post(`/api/challenge/${id}/accept`);
  }
  // open-chess servers only:
  /// Declare the app's engine help on or off during a game here.
  assist(id, on) {
    return this.post(`/api/board/game/${id}/assist`, { on: String(!!on) });
  }
  /// Exact endgame result (up to seven pieces), cached by our server.
  tablebase(fen) {
    return this.json(`/api/esl/tablebase?fen=${encodeURIComponent(fen)}`);
  }
  /// The position dictionary: the stored evaluation of a position.
  position(fen) {
    return this.json(`/api/esl/position?fen=${encodeURIComponent(fen)}`);
  }
  /// Every legal move with the dictionary's value for it, where known.
  positionMoves(fen) {
    return this.json(`/api/esl/position/moves?fen=${encodeURIComponent(fen)}`);
  }
  /// Send in this app's analysis of a position (a hint for others).
  async positionHint(hint) {
    const r = await fetch(this.base + "/api/esl/position", {
      method: "POST",
      headers: this.headers({ "Content-Type": "application/json" }),
      body: JSON.stringify(hint),
    });
    if (!r.ok) throw new Error(`sending the analysis failed (${r.status})`);
  }
  /// Read a board from a photo (a data: URL).
  async recognise(image) {
    const r = await fetch(this.base + "/api/esl/recognise", {
      method: "POST",
      headers: this.headers({ "Content-Type": "application/json" }),
      body: JSON.stringify({ image }),
    });
    const body = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(body.error || `photo reading failed (${r.status})`);
    return body;
  }
  /// Say what a photo the server kept for review showed (FEN placement).
  async confirmPhoto(id, fen) {
    const placement = String(fen).split(" ")[0];
    const r = await fetch(this.base + `/api/esl/photos/${id}/confirm`, {
      method: "POST",
      headers: this.headers({ "Content-Type": "application/json" }),
      body: JSON.stringify({ placement, fen: String(fen).includes(" ") ? fen : undefined }),
    });
    if (!r.ok) throw new Error(`confirming the photo failed (${r.status})`);
  }
  decline(id) {
    return this.post(`/api/challenge/${id}/decline`);
  }
  // open-chess board devices only:
  /// The device's settings and what it's doing.
  device() {
    return this.json("/api/device");
  }
  /// Change the device's settings: any of {level, play, time} ("5+3", "none").
  setDevice(form) {
    return this.post("/api/device", form);
  }
}
