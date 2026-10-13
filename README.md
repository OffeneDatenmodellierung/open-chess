<p align="center"><img src="web/chess/icon.svg" alt="open-chess logo: a knight in an open ring" width="112"></p>

# open-chess

A chess server, engine, training app and electronic-board runtime in Rust.

* **A Lichess-compatible server** (`open-chess`): the Board, Bot, Challenge
  and OAuth (PKCE) APIs, the event, game and move streams and game export,
  with Lichess's JSON, on Postgres. Lichess clients, Board API apps and
  [lichess-bot](https://github.com/lichess-bot-devs/lichess-bot) work with
  it by changing the base URL. Replicas share state through Postgres
  (LISTEN/NOTIFY), so it scales out.
* **Post-game fair-play screening** for human review: accuracy, engine-match
  rate, move-time patterns and a fingerprint for players copying the
  training app's engine.
* **Our own engine and training toolkit**: search, win/draw/loss odds, move
  explanations, tactical motifs, openings, small endgame tables and NNUE
  evaluation, pure Rust and wasm-safe, with distributed SPRT/SPSA tuning.
* **The Chess lab app** (`web/chess`): an installable, offline-first web app
  that plays against the engine, explains moves, reads a position from a
  photo, and plays online on Lichess or an open-chess server.
* **Electronic boards** (`open-chess-device`): DGT boards (USB, Bluetooth,
  serial, the Smart Board, the Revelation II), the DGT Pi, the DGT Centaur,
  the DGT Pegasus and Chessnut's boards (Air, Go …, on Bluetooth or USB).
  Play the engine or any UCI engine on the board, play online on Lichess,
  or be the board for Lichess's DGT page. It speaks PicoChess's web API,
  reads `picochess.ini` and has PicoChess's clock menu, and runs on Linux
  (including the Raspberry Pi inside a DGT Pi or Centaur), macOS and
  Windows.

## Install

Each [release](https://github.com/OffeneDatenmodellierung/open-chess/releases)
has the binaries, the image and the crates
([docs/releases.md](docs/releases.md) has the details, and how to cut one):

| | |
|---|---|
| The board runtime | `open-chess-device-vX.Y.Z-<target>.tar.gz` (`.zip` on Windows): Linux x86-64 and arm64 (64-bit Raspberry Pi OS), ARMv6 (the Centaur's Pi Zero), macOS (Apple silicon and Intel) and Windows |
| The server | `open-chess-server-vX.Y.Z-<target>.tar.gz`: Linux x86-64 and arm64; or the image below |
| The Chess lab app | `chess-app-vX.Y.Z.tar.gz`: static files to serve |
| The image | `docker pull ghcr.io/offenedatenmodellierung/open-chess` (with Stockfish, for both roles) or `:slim` (the API and the app only) |
| The crates | `open-chess-core`, `open-chess-client`, `open-chess-analysis` … on [crates.io](https://crates.io/search?q=open-chess) |

Or from source: `cargo install --git https://github.com/OffeneDatenmodellierung/open-chess open-chess-device`.

## Quick start

**A board.** Run `open-chess-device` in a terminal: it lists the boards it
finds (serial ports, Chessnut boards on USB, Bluetooth LE boards) and shows
the game and a log once you pick one. Or name the board:

```sh
open-chess-device --board /dev/ttyACM0 --level 5 --time 5+3   # a DGT board, the engine at level 5
open-chess-device --ble AA:BB:CC:DD:EE:FF --ble-board chessnut  # a Chessnut Air or Go
open-chess-device --centaur                                     # on a DGT Centaur
OPEN_CHESS_TOKEN=lip_… open-chess-device --board /dev/ttyACM0 --online   # your Lichess games
open-chess-device --board /dev/ttyACM0 --livechess              # for lichess.org/dgt
```

Everything about boards, the clock and its menu, PicoChess and the APIs
the device serves is in [docs/boards.md](docs/boards.md).

**The server.**

```sh
docker run -p 8090:8090 -e DATABASE_URL=postgres://… \
  -e CHESS_PUBLIC_URL=https://chess.example.com ghcr.io/offenedatenmodellierung/open-chess
```

It keeps its own accounts (`/signup`, `/login`, tokens on `/account`); a
host site's players can sign in with the site (`ESPORTS_LOGIN`,
`ESPORTS_SECRET`), and sites follow its games through a live feed
(`GET /api/feed`). The settings, scaling and fair play are in the
[server README](crates/chess-server/README.md), the endpoints in
[docs/api.md](docs/api.md).

## Crates

Packages are named `open-chess-*` (`cargo test -p open-chess-core`).

| Crate | What |
|---|---|
| `chess-core` | Rules (on the MIT cozy-chess move generator), SAN, draw rules, Fischer clocks, PGN, Glicko-2 |
| `chess-proto` | Lichess API wire types |
| `chess-client` | Async client for the Lichess Board, Bot and Challenge APIs, against lichess.org or open-chess |
| `chess-engine` | Async UCI engine driver (Stockfish) |
| `chess-analysis` | The engine and training analysis, and Polyglot opening books ([docs/engine.md](docs/engine.md)) |
| `chess-wasm` | `chess-analysis` for the browser (not published) |
| `chess-fairplay` | Post-game fair-play screening |
| `chess-server` | The server, `open-chess` ([README](crates/chess-server/README.md)) |
| `chess-train` | Distributed engine training |
| `chess-vision` | The board reader ([README](crates/chess-vision/README.md)) |
| `chess-vision-train` | Training the board reader on licence-checked datasets ([README](crates/chess-vision-train/README.md); not published) |
| `chess-conformance` | Checks a server against Lichess's API schemas (not published) |
| `chess-dgt` | The DGT e-board protocol (USB, serial and Bluetooth boards, the Smart Board, the Revelation II, a DGT3000 on the board's cable, the Pegasus), and turning board readings into moves; serial I/O behind the `serial` feature |
| `chess-ble` | A small Bluetooth LE GATT client over the Linux kernel's sockets (no D-Bus, no system libraries) |
| `chess-chessnut` | Chessnut's boards (Air, Air+, Pro, Go) over Bluetooth LE or USB (GPL-3.0-or-later) |
| `chess-centaur` | The DGT Centaur's board controller and e-paper screen, and following a game from occupancy alone (GPL-3.0-or-later) |
| `chess-dgtpi` | The DGT Pi's built-in clock over I²C, a port of DGT's `dgtpicom` (GPL-3.0-or-later) |
| `chess-device` | `open-chess-device`, the board runtime (GPL-3.0-or-later) |

## Docs

* [docs/boards.md](docs/boards.md): electronic boards, the clock and its menu, PicoChess, Lichess, and the device's APIs
* [docs/api.md](docs/api.md): the server's API, live game streams, OAuth and scopes
* [docs/lichess.md](docs/lichess.md): using the server with Lichess clients and bots, and Lichess's DGT connector
* [docs/engine.md](docs/engine.md): the engine and its training
* [docs/releases.md](docs/releases.md): downloads, the image, the crates, signing, and cutting a release
* [The server README](crates/chess-server/README.md): running and configuring the server, fair play, scaling
* [The board reader](crates/chess-vision/README.md) and [its training](crates/chess-vision-train/README.md)
* Deployment examples: [Kubernetes](deploy/k8s/chess.yaml), [Cloudflare Containers](deploy/cloudflare/README.md), and systemd units for the [DGT Pi](deploy/dgtpi) and [Centaur](deploy/centaur)

## Build and test

Rust 1.97 or newer.

```sh
cargo test --workspace
# The server's end-to-end tests need a Postgres:
CHESS_TEST_DATABASE_URL=postgres://dev:dev@127.0.0.1/chessdev?sslmode=disable \
  cargo test -p open-chess-server --test e2e
# The app, with the engine compiled to WebAssembly:
scripts/build-chess-app.sh dist/chess
# The image from your own build instead of a release:
cargo build --release -p open-chess-server --bin open-chess
install -D target/release/open-chess dist/linux-amd64/open-chess
scripts/build-chess-app.sh dist/app
docker build --build-arg BINARIES=local -t open-chess .
```

## Status

Extracted from esports-lab, a private application that hosts the server's
accounts (players, API tokens and sign-ins come from a host site's
database schema) and the training coordinator. Next: a standalone account
mode, so the server needs no host site. Board support is complete for the
boards above; none of it has been tried on every board yet, so reports
are welcome.

## Licence

MIT or Apache-2.0, at your option, except the device runtime
(`chess-device`), the DGT Pi clock driver (`chess-dgtpi`), the Centaur
driver (`chess-centaur`) and the Chessnut driver (`chess-chessnut`), which
build on GPL code and are GPL-3.0-or-later; nothing else depends on them.
The app's font (Atkinson Hyperlegible) is under the SIL Open Font License
(`web/chess/fonts/OFL.txt`). Stockfish, used by the fair-play worker and
installed in the image, is a separate GPL-3.0 program run as a child
process.
