# open-chess

A chess server, engine and training app in Rust.

* **A Lichess-compatible server** (`crates/chess-server`, binary
  `open-chess`): the Board, Bot, Challenge and OAuth (PKCE) APIs, the event,
  game and move streams and game export, with Lichess's JSON, on Postgres.
  Lichess clients, Board API apps and
  [lichess-bot](https://github.com/lichess-bot-devs/lichess-bot) work with it
  by changing the base URL. Replicas share state through Postgres
  (LISTEN/NOTIFY), so it scales out.
* **Post-game fair-play screening** for human review: accuracy, engine-match
  rate, move-time patterns and a fingerprint for players copying the
  training app's engine. The screening model's numbers are data a
  deployment keeps private (see the server README).
* **Our own engine and training toolkit** (`crates/chess-analysis`): search,
  win/draw/loss odds, move explanations, tactical motifs, openings, small
  endgame tables and NNUE evaluation, pure Rust and wasm-safe, with
  distributed SPRT/SPSA tuning (`crates/chess-train`).
* **The Chess lab app** (`web/chess`): an installable, offline-first web app
  that plays against the engine, explains moves, and plays online on
  Lichess or an open-chess server.
* **A board reader** (`crates/chess-vision`): reads a position from a photo
  of a real board, on the server or in the browser. Ships no weights.

## Crates

| Crate | What |
|---|---|
| `chess-core` | Rules (on the MIT cozy-chess move generator), SAN, draw rules, Fischer clocks, PGN, Glicko-2 |
| `chess-proto` | Lichess API wire types |
| `chess-client` | Async client for the Lichess Board, Bot and Challenge APIs |
| `chess-engine` | Async UCI engine driver (Stockfish) |
| `chess-analysis` | The engine and training analysis, and Polyglot opening books (see [docs/engine.md](docs/engine.md)) |
| `chess-wasm` | `chess-analysis` for the browser |
| `chess-fairplay` | Post-game fair-play screening |
| `chess-server` | The server ([README](crates/chess-server/README.md)) |
| `chess-train` | Distributed engine training |
| `chess-vision`, `chess-vision-train` | The board reader and its trainer |
| `chess-dgt` | The DGT e-board protocol (USB, serial and Bluetooth boards, the Smart Board, the Revelation II, a DGT3000 on the board's cable) without I/O, and turning board readings into moves |
| `chess-device` | `open-chess-device`: play open-chess's engine or any UCI engine on a DGT board, with Polyglot opening books and the clock, and serve the app as the board's screen |

Packages are named `open-chess-*` (`cargo test -p open-chess-core`).

## Docs

* [docs/api.md](docs/api.md): the server's API, live game streams, OAuth and scopes
* [docs/lichess.md](docs/lichess.md): using it with Lichess clients, bots and the Lichess DGT connector
* [docs/boards.md](docs/boards.md): electronic boards, PicoChess and DGT devices: what works and what's next
* [docs/engine.md](docs/engine.md): the engine and its training
* [The server README](crates/chess-server/README.md): running and configuring it, fair play, scaling

## Build and test

Rust 1.97 or newer.

```sh
cargo test --workspace
# The server's end-to-end tests need a Postgres:
CHESS_TEST_DATABASE_URL=postgres://dev:dev@127.0.0.1/chessdev?sslmode=disable \
  cargo test -p open-chess-server --test e2e
# The app, with the engine compiled to WebAssembly:
scripts/build-chess-app.sh dist/chess
# The server image (API, fair-play worker with Stockfish, and the app):
docker build -t open-chess .
```

Deployment examples are in `deploy/` (Kubernetes and Cloudflare
Containers).

## Status

Extracted from esports-lab, a private application that still hosts the
server's accounts (players, API tokens and sign-ins come from a host site's
database schema) and the training coordinator. Next: a standalone account
mode, and running the app on electronic boards and chess computers
(DGT boards and the DGT Pi, the DGT Centaur), compatible with PicoChess and
Lichess's DGT connector.

## Licence

MIT or Apache-2.0, at your option. The app's font (Atkinson Hyperlegible)
is under the SIL Open Font License (`web/chess/fonts/OFL.txt`). Stockfish,
used by the fair-play worker and installed in the image, is a separate
GPL-3.0 program run as a child process.
