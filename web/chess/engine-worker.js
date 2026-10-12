// Runs the Rust engine (chess-wasm) off the main thread. Each request
// carries the whole game (start FEN + moves), so the worker holds no game
// state of its own and stale requests are harmless. The searcher (and the
// endgame tables it builds) persists between requests.

import init, { Session } from "./pkg/open_chess_wasm.js";

let session = null;
// An optional trained network next to the app (nets/default.nnue); without
// one the hand-written evaluation is used.
const ready = init().then(async () => {
  session = new Session(null);
  try {
    const r = await fetch(new URL("./nets/default.nnue", import.meta.url));
    if (r.ok) session.loadNetwork(new Uint8Array(await r.arrayBuffer()));
  } catch {}
});

self.onmessage = async (e) => {
  const { id, op, fen, moves, args } = e.data;
  try {
    await ready;
    session.loadMoves(fen || null, moves || "");
    let result;
    if (op === "analyse") {
      const { depth, nodes, lines, plies, timeMs } = args;
      result = JSON.parse(session.analyse(depth, nodes, lines, plies, timeMs || 0));
    } else if (op === "bot") {
      result = JSON.parse(session.botMove(args.level, args.seed >>> 0));
    } else if (op === "best") {
      result = JSON.parse(session.bestMove(args.timeMs));
    } else if (op === "tablebase") {
      result = JSON.parse(session.tablebase());
    } else if (op === "reset") {
      session.reset_engines();
      result = null;
    } else {
      throw new Error(`unknown op ${op}`);
    }
    self.postMessage({ id, result });
  } catch (err) {
    self.postMessage({ id, error: String(err?.message || err) });
  }
};
