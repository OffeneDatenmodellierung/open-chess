# Electronic boards, PicoChess and DGT

open-chess is growing a board runtime: play on a real electronic board
against its engine, online on lichess.org or an open-chess server, or through
the tools people already use with these boards (PicoChess's web page,
Lichess's DGT connector). This page says what works today and what is
coming, and how to try it.

## What works today

**DGT e-boards over USB, serial or Bluetooth** (the wooden e-Boards, the
Smart Board, the Revelation II), with a DGT3000 or XL clock on the board's
cable. The `chess-dgt` crate speaks DGT's board protocol and follows the game
from the board: moves (castling, captures, promotions to the piece you put
down), takebacks, a new game when the starting position is set up (either way
round), and moves played elsewhere (an engine, an online opponent) waiting to
be made on the board.

Try it with any DGT USB board on Linux or a Mac:

```sh
cargo run -p open-chess-dgt --features serial --example watch -- /dev/ttyACM0
# macOS: /dev/cu.usbmodem…   Bluetooth (after `rfcomm bind`): /dev/rfcomm0
```

Set up the starting position and play: each move prints as it's made, and
shows on a DGT3000 if one is connected.

**Playing the engine on the board.** `open-chess-device` plays a game on the
board against open-chess's engine (levels 1–8) or any UCI engine
(Stockfish, Leela …), with a Polyglot opening book if you give it one:

```sh
cargo run --release -p open-chess-device -- --board /dev/ttyACM0 --level 5
cargo run --release -p open-chess-device -- --board /dev/ttyACM0 \
    --uci /usr/games/stockfish --move-time 2 --uci-option "Skill Level=5" \
    --book gm2001.bin --play black --time 5+3
```

The engine's move shows on the DGT3000 (and its squares light on a Revelation
II); make it on the board and play on. Set up the starting position again
for a new game; put pieces back to take moves back. The result shows when the
game ends.

With `--time 5+3` (minutes + increment) the game is played to a clock: the
DGT3000 shows both sides' time (`--white-clock right` if White sits on the
right), clocks start with the first move, the engine plays to the clock, and
a flag ends the game. The DGT3000's buttons, left to right: show the
engine's last move again, level down, switch sides (the engine takes over
the side to move), level up, pause or resume the clock.

**The Chess lab app as the board's screen.** With `--listen`, the device
serves the app and a Lichess-style API; open its address on a phone or
tablet on the same network:

```sh
scripts/build-chess-app.sh dist/chess
cargo run --release -p open-chess-device -- --board /dev/ttyACM0 \
    --listen 0.0.0.0:8080 --app dist/chess --name "Living room board"
# then open http://<the device's address>:8080/ on the phone
```

The app opens on the board's game: the moves as they're made, the engine's
move waiting to be made on the board, both clocks, and the app's own help
(win odds, lines, move explanations) if you want it. Its Play online tab
becomes the device's panel: the engine's level, which side you play, and
the time control. Moves are still made on the board. It's the same app as
on an open-chess server or lichess.org, reading the same streams; the
device says it's a device (`GET /` → `"device": true`), so the app skips
signing in.

The device's API, for other clients:

| Endpoint | What |
|---|---|
| `GET /` | `{"site": …, "device": true}` (browsers go to the app) |
| `GET /api/account`, `/api/account/playing` | The person at the board (`board`) and their game |
| `GET /api/stream/event` | `gameStart` when a game starts on the board, `gameFinish` when it ends (or a new one is set up) |
| `GET /api/board/game/stream/{id}` | `gameFull`, then `gameState` as moves are made |
| `GET /api/stream/game/{id}` | The live feed: the game, then the position and clocks after each move |
| `GET /api/device` | Level, side, time control, the engine, whether it's thinking or its move is waiting |
| `POST /api/device` | Change them: `level=1…8`, `play=white\|black`, `time=5+3\|none` |
| `GET /app/…` | The app (from `--app`) |

Board API moves, resignations and draw offers answer `400` with a message:
on a board they're made with the pieces. There is no sign-in: anyone who can
reach the address can watch and change the settings, so listen on a home
network (`--listen 0.0.0.0:8080` is every network the device is on; give
one address to narrow it), never the internet.

**The DGT Pi.** On a DGT Pi the clock is the Pi's own, wired to its I²C
pins rather than the board's cable; `--clock dgtpi` drives it (a Rust port of
DGT's `dgtpicom`, in `chess-dgtpi`). The board connects over Bluetooth or
USB as usual. The clock driver maps the Pi's I²C registers from `/dev/mem`,
so it runs as root, as PicoChess does:

```sh
# On the DGT Pi (64-bit Raspberry Pi OS), with the board paired:
sudo rfcomm bind 0 <the board's Bluetooth address>
sudo open-chess-device --board /dev/rfcomm0 --clock dgtpi --time 5+3 \
    --listen 0.0.0.0:8080 --app /opt/open-chess/app
```

It wakes the clock if it's off, takes control of it, shows the moves and the
time, and the five buttons work as on a DGT3000 on the cable.
[`deploy/dgtpi`](../deploy/dgtpi) has a systemd unit to start it at boot.
`--clock sim` runs the same code against a simulated DGT3000, to try
everything else without one.

**The DGT Centaur.** On a Centaur, `open-chess-device --centaur` runs on
its own Raspberry Pi and talks to the board's controller (`/dev/serial0`,
1,000,000 baud; `chess-centaur`):

```sh
open-chess-device --centaur --level 3 --listen 0.0.0.0:8080 --app /opt/open-chess/app
```

The Centaur senses only whether a square has a piece, not which, so moves
are read from pieces lifted and put down: a move counts when its piece is
put down on its square (for a capture, lift the captured piece and put
yours in its place, in either order), castling when the second of king and
rook is put down, en passant when the captured pawn is lifted, if that's
last. Lifting a piece and putting it back does nothing; setting an earlier
position back takes moves back; the starting position starts a new game.
Promotions are to a queen. The engine's move lights its two squares and
beeps. The buttons: back shows the engine's move again (on the clock, so
for now only in the app), down and up change the level, tick switches sides,
play pauses the clock. Its e-paper screen shows the game: the players and
clocks, the board (from your side) with the last move marked, what's
going on ("make e5 on the board") and the latest moves, with quick
partial refreshes and a full one now and then to clear ghosting
(`--no-screen` leaves it alone). The original Centaur has a Raspberry Pi Zero W, a
32-bit ARMv6: build for `arm-unknown-linux-gnueabihf`, or swap in a Zero 2 W
and use the 64-bit build. [`deploy/centaur`](../deploy/centaur) has a
systemd unit.

**Bluetooth LE boards: Chessnut and the DGT Pegasus.** `--ble` connects
to a board by its Bluetooth address, on Linux (a Raspberry Pi or any Linux
computer), through the kernel's own Bluetooth sockets: nothing to install
beyond BlueZ, which Raspberry Pi OS has.

```sh
bluetoothctl            # then: scan on   (note the board's address)   quit
open-chess-device --ble AA:BB:CC:DD:EE:FF --ble-board chessnut   # Chessnut Air (and Go)
open-chess-device --ble AA:BB:CC:DD:EE:FF --ble-board pegasus    # DGT Pegasus
```

Don't pair the board, or disconnect it in `bluetoothctl` first: it takes
one connection at a time. If `bluetoothctl info` shows a random address,
add `--random-address`.

* **Chessnut Air, Air+, Pro** (`chess-chessnut`): the board reports every
  piece, so moves, takebacks and new games read as on a DGT board, and the
  engine's move lights its two squares. The protocol is PicoChess's.
* **Chessnut Go**: expected to speak the same protocol (Chessnut's own
  drivers treat the Air, Air+, Go and Pro together); to be confirmed on a
  board.
* **DGT Pegasus**: the DGT protocol over the Nordic UART service, unlocked
  with the developer key community drivers publish, with its own lights.

None of these has been tried on a real board yet.

**Online play from the board.** `--online` plays games on lichess.org (or
an open-chess server: `--online https://chess.example.org`) instead of the
engine, through the Board API, with no browser. Create an API token with
the `board:play` scope (and `challenge:write` to challenge from the device)
at lichess.org/account/oauth/token:

```sh
export OPEN_CHESS_TOKEN=lip_…
open-chess-device --board /dev/ttyACM0 --online
open-chess-device --board /dev/ttyACM0 --online --challenge a_friend --time 10+5 --play black
```

Start a game on the site or in the app (a seek, or accept a challenge) and
the board plays it: your moves on the board are sent, your opponent's show
on the clock (and light up on boards with lights) and wait to be made on
the board, and the clock shows the game's times. The clock's first button
shows the opponent's last move again. A takeback on the board can't take a
move back online: the board says "set back" until the game's position is
set up again. It works with every board above (`--centaur`, `--ble` …),
and `--listen` shows the game in the app too. The Board API allows rapid,
classical and correspondence games (and blitz on lichess.org for some
accounts), not bullet.

**PicoChess's web API.** With `--listen`, the device also answers the
API PicoChess v4's web server has, so tools written for PicoChess work
with it: the `/event` websocket (`Game`, `Fen` with `user`, `computer` or
`reload`, `Light` and `Clear` for the engine's move waiting on the board,
`Clock` with PicoChess's HTML clock every second while it runs, `Header`,
`GameEnd`), `POST /channel` (`clockbutton` and `new_time`; moves are made
on the board, so the rest answer that), `GET /dgt?action=get_last_move` and
`GET /info?action=get_headers|get_clock_text|get_system_info`.

**Lichess's DGT page** ([lichess.org/dgt](https://lichess.org/dgt)).
`--livechess` makes the device the board for Lichess's own DGT page, in
place of DGT's LiveChess app, for any board it drives:

```sh
open-chess-device --board /dev/ttyACM0 --livechess      # or --centaur, --ble …
# then open lichess.org/dgt in a browser on the same computer, and play
```

It answers LiveChess's websocket on `localhost:1982`: the page finds the
board, follows it, and sends the moves made on it to Lichess; when the
opponent moves, the page sends the new position, which the device plays
on the board to be made there (lit, on boards with lights). Browsers let
lichess.org reach a plain websocket only on `localhost`, so the browser
must run on the computer the device runs on: the device's own screen, or a
laptop with the board on USB or Bluetooth. (`--online` does the same
without a browser.)

**The terminal screen.** Run `open-chess-device` in a terminal without
naming a board (or with `--tui`) and it lists the boards it finds, Bluetooth
LE boards nearby, USB and serial ports, Chessnut boards on USB, to choose
from with the arrow keys and Enter. Once connected it shows the board, the
players, both clocks, the moves and what's waiting to be made on the
board, above a log, newest first: the board, the engine, the online game,
and every request from the app, PicoChess's tools and Lichess's DGT page
(handy when the page doesn't connect). `b` goes back to the list, `q`
quits, PgUp/PgDn scroll the log. All the other options work with it
(`--online`, `--livechess`, `--listen` …).

**Chessnut boards on USB.** `--usb /dev/hidraw0` (or choose it on the
screen) plays a Chessnut Air, Air+, Pro or Go on its USB cable: the same
messages as over Bluetooth, as HID reports, found by Chessnut's USB ids
(those in Chessnut's EasyLink SDK). On Linux, give your user access to the
board's `hidraw` node with a udev rule (or run as root):

```
SUBSYSTEM=="hidraw", ATTRS{idVendor}=="2d80", MODE="0660", GROUP="plugdev"
```

**macOS and Windows.** The device runs there too: DGT boards on USB
(`/dev/cu.usbmodem…`, or `COM3`), and Bluetooth LE boards through the
system's own Bluetooth (on a Mac a board is known by the id macOS gives it,
which the screen shows). Chessnut boards on USB work there too (through hidapi); the DGT Pi
clock and the Centaur are Linux by nature.

**PicoChess's settings.** `--picochess-ini /opt/picochess/picochess.ini`
takes what a PicoChess setup already says: the board's port
(`dgt-port`), the DGT Pi's clock (`dgtpi`), the engine, its level
(`Elo@2200` limits a UCI engine's strength, `Level@5` sets Skill Level)
and book, the time control (`time = 5 3`), the web server's port and which
side of the clock is White's. Options on the command line win; keys the
device doesn't use are listed in the log.

## Coming next

In order; each lands in open-chess as it's ready.

1. **Play the engine on the board.** Done (above): levels, UCI engines,
   Polyglot books, time controls on the clock and its buttons. PicoChess's
   full clock menu comes with PicoChess compatibility (4).
2. **The Chess lab app as the board's screen.** Done (above).
3. **Online play from the board.** Done (above).
4. **PicoChess compatibility.** The web API and `picochess.ini` are done
   (above). Still to come: PicoChess's full clock menu.
5. **Lichess's DGT connector.** Done (above).
6. **A terminal screen and Mac/Windows.** Done (above).
7. **Chessnut boards over USB.** Done (above).
8. **The Centaur's e-paper screen.** Done (above).

## Devices

| Device | Inside | Status |
|---|---|---|
| DGT USB / Bluetooth e-Board, Smart Board, Revelation II | — (a computer drives it) | Works: `open-chess-device` (above) |
| DGT Pi | Raspberry Pi 3 in a DGT3000 clock; the board on USB or Bluetooth | Works: the board via `chess-dgt`, the built-in clock via `chess-dgtpi` (`--clock dgtpi`). Install: a 64-bit Raspberry Pi OS card with the runtime, like PicoChess v4 |
| DGT Centaur | Raspberry Pi Zero W (or a Zero 2 W swapped in), board controller on serial, e-paper | Works: board, lights, buttons, sounds, e-paper screen (`--centaur`, `chess-centaur`) |
| DGT Pegasus | Bluetooth LE | `--ble … --ble-board pegasus` (`chess-ble`, `chess-dgt`) |
| Chessnut Air, Air+, Pro | Bluetooth LE; recognises pieces | `--ble … --ble-board chessnut` (`chess-chessnut`) |
| Chessnut Go | Bluetooth LE (and USB); recognises pieces | Expected to work as the Air; to be confirmed. USB later |
| Raspberry Pi 4 / 5 with any USB or Bluetooth board | — | With the runtime; a DGT3000 on a Pi 5 needs a different clock backend |

Nothing here touches a device's own firmware: a DGT Pi or Centaur boots from
an SD card, so trying open-chess means a second card, and putting the
original back restores it.

## Licences

The board protocol and the following of games (`chess-dgt`) are MIT or
Apache-2.0, like the rest of open-chess. The DGT Pi clock driver
(`chess-dgtpi`) is a port of DGT's `dgtpicom` (GPL-3.0), and the device
runtime (`chess-device`) uses it and will port parts of PicoChess (GPL-3.0),
so both are GPL-3.0-or-later. Nothing MIT/Apache depends on them, and
`deny.toml` allows GPL for those crates only. The Centaur's controller
(`chess-centaur`) follows the protocol as the GPL DGTCentaurMods project
worked it out, and the Chessnut driver (`chess-chessnut`) PicoChess's, so
both are GPL-3.0-or-later too. The Bluetooth layer (`chess-ble`) is ours,
MIT or Apache-2.0.

Not yet tried on real hardware: the DGT Pi driver follows `dgtpicom`
message for message (its tests check the bytes against DGT's), the Centaur
driver follows DGTCentaurMods' packets, the Chessnut driver PicoChess's,
and all are tested against simulated hardware.
