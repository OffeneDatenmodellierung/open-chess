# Electronic boards, PicoChess and DGT

`open-chess-device` plays chess on a real electronic board: against
open-chess's engine or any UCI engine, online on lichess.org or an
open-chess server, or as the board for the tools people already use with
these boards (PicoChess's web page and tools, Lichess's DGT page). It runs
on a computer with the board on USB or Bluetooth (Linux, macOS, Windows),
or inside the board's own Raspberry Pi (the DGT Pi, the DGT Centaur).

Get it from a [release](releases.md) (Linux x86-64, arm64 and ARMv6, macOS,
Windows), or build it: `cargo build --release -p open-chess-device`. It's the board
part of the `open-chess` program: `open-chess device …` takes the same
options, and the release archives' `open-chess-device` is a link to
`open-chess`.

## Getting started

Run `open-chess-device` in a terminal without naming a board (or with
`--tui`): it lists the boards it finds (Bluetooth LE boards nearby, USB and
serial ports, Chessnut boards on USB) to choose from with the arrow keys and
Enter. While it connects, it asks what to play (↑/↓ move between the
settings, ←/→ change one, type to fill in a name, token or address, Enter
starts, Esc goes back to the boards):

* **Play the computer**: open-chess's engine at a level (1–8), your colour,
  the clock and your name. This is PicoChess's kind of game: with
  `--picochess-ini` or `--uci` the computer is the engine they name, with
  their book and options.
* **Two players on this board**: both names and the clock. The board
  follows both sides' moves; nobody's moves are made for them.
* **Lichess** or **an open-chess server**: the account's API token (and the
  server's address). The game can be a new one (wait for one started on
  the site or the app, challenge a player, or play the site's computer at a
  level), or, once the token is in, one the site has open for you: a game
  still in progress (to go back to after a disconnect) or a challenge
  waiting for you to accept. ←/→ on **Game** goes through them. Only that
  game is played on the board; others on the account are left alone.
  **Two players here, both accounts** puts two people's accounts on one
  board: give the other player's token too, and the game is started
  between the two accounts (accepted at once with their token). Each move
  made on the board is sent as the account whose side it is, so both
  accounts' ratings count it. A game in progress between them can be
  resumed the same way, with the other token filled in.
* **The board for Lichess's DGT page**: see below.

The names, the level, the clock, the server and the tokens are remembered
for next time in `device.json` in the device's folder (`$OPEN_CHESS_HOME`,
or `~/.config/open-chess`; `%APPDATA%\open-chess` on Windows), readable by
you only.

Then it shows the board, the players and their clocks, the opening's name
and every move in words beside its notation, newest first ("2… Qh4#  Queen
from d8 to h4, checkmate"), above the log, newest first: the board, the
engine, the online game, and every request from the app, PicoChess's tools
and Lichess's DGT page (handy when the page doesn't connect). When the game
ends it says who won and how, and each player's rating before it and what
the game did to it. Set the pieces up again for another game of the same
kind, or press `n` to choose the next game on the same board.

The screen is in the site's colours (royal navy and orange);
`--theme terminal` (or `OPEN_CHESS_THEME=terminal`) keeps the terminal's
own.

`b` goes back to the list of boards, `r` scans again, `q` quits. The mouse
wheel scrolls the moves (over the top half) or the log (below); Tab picks
which one ↑/↓ scroll; PgUp/PgDn scroll the log and Home goes back to the
newest of both. To select text with the mouse, hold Option (macOS) or
Shift. Every other option works with it.

**Ratings.** Games against open-chess's engine and between two people on
the board are rated as FIDE rates: Elo, the expected score from the rating
difference (capped at 400), K = 40 for a player's first 30 games, 10 once
they've reached 2400, 20 otherwise, and a list each for blitz (up to 10
minutes, counting 60 moves' increments), rapid (under 60) and classical
(the rest, and games without a clock). Everyone starts at 1500; the
engine's levels are fixed at 800 for level 1 up to 2200 for level 8 (as on
the open-chess server). They're kept in `ratings.json` in the device's
folder. Games against a UCI engine aren't rated (its strength isn't known),
nor games of fewer than two moves. Online, the site's own ratings are
shown: each player's rating and the game's change once the site has rated
it.

When a board doesn't behave, run with `--debug` (or `OPEN_CHESS_DEBUG=1`):
the log adds every Bluetooth message to and from the board and what the
board offers, and is written to `open-chess-device.log` in the folder you
ran it from (the log's title says where). `--log-file PATH` writes the log
to a file of your choosing, with or without `--debug`.

Or name the board and go:

```sh
open-chess-device --board /dev/ttyACM0 --level 5               # a DGT board on USB
open-chess-device --board /dev/ttyACM0 --uci /usr/games/stockfish --move-time 2 \
    --uci-option "Skill Level=5" --book gm2001.bin --play black --time 5+3
```

The engine's move shows on the clock (and its squares light, on boards with
lights); make it on the board and play on. Set up the starting position
again for a new game; put pieces back to take moves back. The result shows
when the game ends.

## Boards

**DGT e-boards over USB, serial or Bluetooth** (the wooden e-Boards, the
Smart Board, the Revelation II), with a DGT3000 or XL clock on the board's
cable. `chess-dgt` speaks DGT's board protocol and follows the game from
the board: moves (castling, captures, promotions to the piece you put
down), takebacks, a new game when the starting position is set up (either
way round), and moves played elsewhere (an engine, an online opponent)
waiting to be made on the board. `--board` takes the port: `/dev/ttyACM0`
(Linux), `/dev/cu.usbmodem…` (macOS), `COM3` (Windows), `/dev/rfcomm0`
(Bluetooth, after `rfcomm bind`). To watch a board's moves without playing:

```sh
cargo run -p open-chess-dgt --features serial --example watch -- /dev/ttyACM0
```

**The DGT Pi.** The clock is the Pi's own, wired to its I²C pins rather than
the board's cable; `--clock dgtpi` drives it (a Rust port of DGT's
`dgtpicom`, in `chess-dgtpi`). The board connects over Bluetooth or USB as
usual. The clock driver maps the Pi's I²C registers from `/dev/mem`, so it
runs as root, as PicoChess does:

```sh
# On the DGT Pi (64-bit Raspberry Pi OS), with the board paired:
sudo rfcomm bind 0 <the board's Bluetooth address>
sudo open-chess-device --board /dev/rfcomm0 --clock dgtpi --time 5+3 \
    --listen 0.0.0.0:8080 --app /opt/open-chess/app
```

It wakes the clock if it's off, takes control of it, shows the moves and
the time, and the five buttons work as on a DGT3000 on the cable.
[`deploy/dgtpi`](../deploy/dgtpi) has a systemd unit to start it at boot.
`--clock sim` runs the same code against a simulated DGT3000, to try
everything else without one.

**The DGT Centaur.** `open-chess-device --centaur` runs on the Centaur's own
Raspberry Pi and talks to the board's controller (`/dev/serial0`, 1,000,000
baud; `chess-centaur`):

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
beeps. The buttons are the DGT3000's: back shows the engine's move again
(in the app), down and up change the level, tick is play (pause, or switch
sides), and play opens the clock's menu. Its e-paper screen shows the game:
the players and clocks, the board (from your side) with the last move
marked, what's going on ("make e5 on the board", or the menu) and the
latest moves, with quick partial refreshes and a full one now and then to
clear ghosting (`--no-screen` leaves it alone). The original Centaur has a
Raspberry Pi Zero W, a 32-bit ARMv6: use the `arm-unknown-linux-gnueabihf`
release, or swap in a Zero 2 W and use the 64-bit one.
[`deploy/centaur`](../deploy/centaur) has a systemd unit.

**Bluetooth LE boards: Chessnut and the DGT Pegasus.** `--ble` connects to
a board by its address (or pick it on the terminal screen). On Linux that's
through the kernel's own Bluetooth sockets (nothing to install beyond
BlueZ, which Raspberry Pi OS has); on macOS and Windows, through the
system's Bluetooth.

```sh
bluetoothctl            # then: scan on   (note the board's address)   quit
open-chess-device --ble AA:BB:CC:DD:EE:FF --ble-board chessnut   # Chessnut Air, Go …
open-chess-device --ble AA:BB:CC:DD:EE:FF --ble-board pegasus    # DGT Pegasus
```

Don't pair the board, or disconnect it in `bluetoothctl` first: it takes one
connection at a time. If `bluetoothctl info` shows a random address, add
`--random-address` (Linux). On a Mac a board is known by the id macOS gives
it, which the terminal screen shows.

* **Chessnut Air, Air+, Pro and Go** (`chess-chessnut`): the board reports
  every piece, so moves, takebacks and new games read as on a DGT board, and
  the engine's move lights its two squares. The protocol is PicoChess's;
  Chessnut's own drivers treat the Air, Air+, Go and Pro together.
* **DGT Pegasus**: the DGT protocol over the Nordic UART service, unlocked
  with the developer key community drivers publish, with its own lights.

**Chessnut boards on USB.** `--usb /dev/hidraw0` (or pick it on the screen)
plays a Chessnut Air, Air+, Pro or Go on its USB cable: the same messages
as over Bluetooth, as HID reports, found by Chessnut's USB ids (those in
Chessnut's EasyLink SDK). On macOS and Windows the screen finds them
through hidapi. On Linux, give your user access to the board's `hidraw`
node with a udev rule (or run as root):

```
SUBSYSTEM=="hidraw", ATTRS{idVendor}=="2d80", MODE="0660", GROUP="plugdev"
```

On macOS the board is opened shared, which needs no special rights. If
macOS still refuses it ("privilege violation"), allow your terminal app
under System Settings › Privacy & Security › Input Monitoring, then open
a new terminal window.

**macOS and Windows.** DGT boards on USB, Bluetooth LE boards and Chessnut
boards on USB all work there; the DGT Pi's clock and the Centaur are Linux
by nature. Unsigned release binaries need allowing once ([releases.md](releases.md#downloads)).

## Playing

**The engine.** open-chess's engine at levels 1–8 (`--level`, default 4),
or any UCI engine (`--uci`, with `--uci-option NAME=VALUE` and
`--move-time` seconds per move when the game has no clock), opening with a
Polyglot book (`--book`) when there is one.

**The clock.** With `--time 5+3` (minutes + increment) the game is played
to a clock: the DGT3000 shows both sides' time (`--white-clock right` if
White sits on the right), clocks start with the first move, the engine
plays to the clock, and a flag ends the game. The DGT3000's buttons, left
to right:

| Button | Does |
|---|---|
| back | Show the engine's last move again |
| minus | Level down |
| play | Pause or resume a running clock; otherwise switch sides (the engine takes over the side to move) |
| plus | Level up |
| the right-hand one | The menu |

**The clock's menu**, as PicoChess has it: the right-hand button opens it,
minus and plus go through the choices, the right-hand button picks, back
goes up a level (and out at the top), play leaves it, and so does a move on
the board. In it:

* **time**: untimed, PicoChess's sudden-death (1–60 minutes) and Fischer
  (1+1 to 90+30) times, and whatever `--time` set. Before the first move it
  applies at once, otherwise from the next game.
* **level**: 1–8.
* **side**: the side you play.
* **book**: every `.bin` in `--books` (by default the folder of `--book`),
  or none.
* **engine**: open-chess's own, `--uci`, and every program in `--engines`
  (with `--picochess-ini`, the folder of its engine).
* **version**.

On a Centaur the menu shows on the e-paper screen; it also shows in the
app, on the terminal screen, in `GET /api/device` and in PicoChess's
`Clock` messages.

**Online.** `--online` plays games on lichess.org (or an open-chess server:
`--online https://chess.example.org`) instead of the engine, through the
Board API, with no browser. Create an API token with the `board:play` scope
(and `challenge:write` to challenge from the device) at
lichess.org/account/oauth/token, and give it as `--token` or
`OPEN_CHESS_TOKEN`:

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
set up again. The Board API allows rapid, classical and correspondence
games (and blitz on lichess.org for some accounts), not bullet.

**Lichess's DGT page** ([lichess.org/dgt](https://lichess.org/dgt)).
`--livechess` makes the device the board for Lichess's own DGT page, in
place of DGT's LiveChess app, for any board it drives:

```sh
open-chess-device --board /dev/ttyACM0 --livechess      # or --centaur, --ble …
# then open lichess.org/dgt in a browser on the same computer, and play
```

It answers LiveChess's websocket on `127.0.0.1:1982` (`--livechess ADDR`
for another): the page finds the board, follows it, and sends the moves
made on it to Lichess; when the opponent moves, the page sends the new
position, which the device plays on the board to be made there (lit, on
boards with lights). Browsers let lichess.org reach a plain websocket only
on `localhost`, so the browser must run on the computer the device runs
on. (`--online` does the same without a browser.)

## The app and the device's APIs

With `--listen` the device serves, on that address: the Chess lab app, a
Lichess-style API for it and other clients, and PicoChess's web API.
`--listen 0.0.0.0:8080` is every network the device is on; give one
address to narrow it. There is no sign-in: anyone who can reach the
address can watch and change the settings, so listen on a home network,
never the internet. Responses carry no CORS headers.

**The Chess lab app as the board's screen.** Open the device's address on a
phone or tablet on the same network:

```sh
open-chess-device --board /dev/ttyACM0 --listen 0.0.0.0:8080 \
    --app chess-app/ --name "Living room board"
# chess-app/: the release's chess-app-vX.Y.Z.tar.gz unpacked (or scripts/build-chess-app.sh)
# then open http://<the device's address>:8080/ on the phone
```

The app opens on the board's game: the moves as they're made, the engine's
move waiting to be made on the board, both clocks, and the app's own help
(win odds, lines, move explanations) if you want it. Its Play online tab
becomes the device's panel: the engine's level, which side you play, and
the time control. Moves are still made on the board.

### The Lichess-style API

The same paths and JSON as Lichess's Board API (see
[api.md](api.md#live-games)), for the one game on the board. No token is
needed (an `Authorization` header is ignored).

| Endpoint | What |
|---|---|
| `GET /` | `{"site": <--name>, "device": true}` (browsers go to the app) |
| `GET /api/account` | The person at the board: `{"id": "board", "username": "Board", …}` |
| `GET /api/account/playing` | `{"nowPlaying": [<the board's game>]}` |
| `GET /api/stream/event` | NDJSON: `gameStart` when a game starts on the board, `gameFinish` when it ends (or a new one is set up) |
| `GET /api/board/game/stream/{id}` | NDJSON: `gameFull`, then `gameState` as moves are made |
| `GET /api/stream/game/{id}` | NDJSON: the game, then the position and clocks after each move |
| `POST /api/board/game/{id}/…` | Every Board API action (moves, resign, draw, takeback, abort, chat …) answers 400 with a message: on a board they're made with the pieces |
| `GET /api/device` | The device's state (below) |
| `POST /api/device` | Change it (below) |
| `GET /app/…` | The app (from `--app`; `/app` redirects to `/app/`) |

**`GET /api/device`:**

```json
{"gameId": "a1b2c3d4", "level": 4, "play": "white", "time": "5+3",
 "engine": "open-chess level 4", "builtin": true, "thinking": false,
 "awaitingBoard": false, "paused": false, "status": "started", "menu": null}
```

`time` is `null` untimed; `menu` is what the clock's menu shows while it's
open.

**`POST /api/device`**, form-encoded, any of `level=1…8`,
`play=white|black`, `time=5+3|none`:

```sh
curl -d level=6 -d time=10+5 http://board.local:8080/api/device
```

It answers the new state with `said`, what the device made of each change
(`["The engine plays at level 6.", "The next game is 10+5."]`); 400 for a
value it can't take (or a side switch while the engine's move waits on the
board), 503 if the device doesn't answer within 5 s.

### PicoChess's web API

What PicoChess v4's web server answers, so its web page and tools work with
the device:

| Endpoint | What |
|---|---|
| `GET /event` (websocket) | The game as PicoChess sends it, below |
| `POST /channel` | `action=clockbutton&button=0…4` (a clock button, from the left); `action=new_time&time_mode=1\|2&time=<minutes>&fischer=<seconds>` (1 sudden death, 2 Fischer; anything else untimed). An empty 200, or 400 "…: not on this device" for PicoChess actions the device doesn't take (moves are made on the board) |
| `GET /dgt?action=get_last_move` | The latest board message (as on `/event`) |
| `GET /info?action=get_headers` | `{"Event", "Site", "White", "Black", "Result"}` |
| `GET /info?action=get_clock_text` | The clock as PicoChess's HTML |
| `GET /info?action=get_system_info` | `{"version": "open-chess X.Y.Z", "pending_engine_move": <the engine's move waits on the board>}` |
| `GET /info?action=get_ip_info` | `{}` |

Unknown actions on `/dgt` and `/info` get an empty 200. On `/event`, a
client first gets the latest board message, `Header`, `Light` if a move is
waiting and `Clock`; then:

| Message | When |
|---|---|
| `{"event": "Game", "fen", "pgn", "move": "0000", "play": "newgame"}` | A new game |
| `{"event": "Fen", "fen", "pgn", "move": "e2e4", "play": "user"\|"computer"\|"reload"}` | A move: yours, the engine's (once made on the board), or the position set back |
| `{"event": "Light", "move": "e7e5"}`, `{"event": "Clear"}` | The engine's move waiting on the board, and made |
| `{"event": "Clock", "msg": <HTML>, "running": true, "menu_active": false}` | Every second while a clock runs |
| `{"event": "Header", "headers": {…}}` | The players or the result changed |
| `{"event": "GameEnd", "result": "1-0"}` | The game is over |

### LiveChess's websocket

With `--livechess`, at `ws://127.0.0.1:1982/api/v1.0`, as DGT's LiveChess
answers Lichess's DGT page (any client that speaks it works):

| The client sends | The device answers |
|---|---|
| `{"id": 1, "call": "eboards"}` | `{"response": "call", "id": 1, "param": [{"serialnr": "OPENCHESS", "source": "open-chess", "state": "ACTIVE", "board": <FEN placement>, "flipped": false, "clock": null, …}]}` |
| `{"id": 2, "call": "subscribe", "param": {"feed": "eboardevent", "param": {"serialnr": "OPENCHESS"}}}` | An acknowledgement, then a `{"response": "feed", "id": 2, "param": {"serialnr", "board", "flipped", "clock": null, "san": [<moves since the last setup>]}}` after every move |
| `{"id": 3, "call": "call", "param": {"method": "setup", "param": {"fen": "…"}}}` | `{"response": "call", "id": 3, "param": true}`; the position becomes the board's game (a move on from it is played on the board, to be made there) |

The device pings every 30 seconds.

## Command line

| Option | Default | What |
|---|---|---|
| `--board PORT` | | A DGT board's port; with `--centaur`, the controller's (`/dev/serial0`) |
| `--centaur` | | On a DGT Centaur |
| `--ble ADDR` | | A Bluetooth LE board |
| `--ble-board chessnut\|pegasus` | `chessnut` | Which kind |
| `--random-address` | | The board has a random Bluetooth address (Linux) |
| `--usb PATH` | | A Chessnut board on USB (`/dev/hidraw0`) |
| `--tui` | when no board is named, in a terminal | The terminal screen |
| `--no-screen` | | Leave the Centaur's e-paper screen alone |
| `--picochess-ini FILE` | | Settings from PicoChess (below) |
| `--level 1…8` | `4` | open-chess's engine level |
| `--uci PATH` | | A UCI engine instead |
| `--move-time SECONDS` | `2` | The UCI engine's time per move, without a clock |
| `--uci-option NAME=VALUE` | | UCI options (repeat) |
| `--book FILE` | | A Polyglot opening book |
| `--books DIR` | the folder of `--book` | Books the menu offers |
| `--engines DIR` | | UCI engines the menu offers |
| `--play white\|black` | `white` | Your side |
| `--time M+S` | untimed | The time control |
| `--white-clock left\|right` | `left` | Which side of the clock is White's |
| `--clock board\|dgtpi\|sim` | `board` | The clock on the board's cable, a DGT Pi's own, or a simulated one |
| `--listen ADDR` | | Serve the app and the APIs here |
| `--app DIR` | | The Chess lab app's files |
| `--name NAME` | `open-chess board` | The device's name, in the app |
| `--online [URL]` | `https://lichess.org` | Play online instead of the engine |
| `--token TOKEN` (or `OPEN_CHESS_TOKEN`) | | The account's API token for `--online` |
| `--challenge USER` | | With `--online`, challenge them at the start |
| `--livechess [ADDR]` | `127.0.0.1:1982` | Be the board for Lichess's DGT page |

`open-chess-device --help` says the same.

**PicoChess's settings.** `--picochess-ini /opt/picochess/picochess.ini`
takes what a PicoChess setup already says. Options on the command line win;
keys the device doesn't use are listed in the log.

| Key | Becomes |
|---|---|
| `dgt-port` | `--board` |
| `dgtpi = true` | `--clock dgtpi` |
| `engine` | `--uci` (a path relative to the ini's folder), and its folder `--engines` |
| `engine-level` | `Elo@2200`: `UCI_LimitStrength=true` and `UCI_Elo=2200`; `Level@5` or `Skill@5`: `Skill Level=5` |
| `book` | `--book` (relative to the ini's folder) |
| `time = 5 3` | `--time 5+3` (`time = 10`: ten minutes) |
| `web-server = 80` | `--listen 0.0.0.0:80` |
| `clockside = left\|right` | `--white-clock` |

## Devices

| Device | Inside | Status |
|---|---|---|
| DGT USB / Bluetooth e-Board, Smart Board, Revelation II | — (a computer drives it) | `--board` |
| DGT Pi | Raspberry Pi 3 in a DGT3000 clock; the board on USB or Bluetooth | `--board … --clock dgtpi`. Install: a 64-bit Raspberry Pi OS card with the runtime, like PicoChess v4 |
| DGT Centaur | Raspberry Pi Zero W (or a Zero 2 W swapped in), board controller on serial, e-paper | `--centaur`: board, lights, buttons, sounds, e-paper screen |
| DGT Pegasus | Bluetooth LE | `--ble … --ble-board pegasus` |
| Chessnut Air, Air+, Pro, Go | Bluetooth LE and USB; recognises pieces | `--ble … --ble-board chessnut`, or `--usb` |
| Raspberry Pi 4 / 5 with any USB or Bluetooth board | — | With the arm64 release; a DGT3000 on a Pi 5's I²C needs a different clock backend |

Nothing here touches a device's own firmware: a DGT Pi or Centaur boots from
an SD card, so trying open-chess means a second card, and putting the
original back restores it.

**Not yet tried on real hardware.** Each driver follows a published
implementation message for message and is tested against simulated
hardware: the DGT Pi's clock follows `dgtpicom` (its tests check the bytes
against DGT's), the Centaur DGTCentaurMods' packets, the Chessnut boards
PicoChess's driver, the Pegasus the community drivers. Reports from real
boards are welcome.

## Licences

The board protocol and the following of games (`chess-dgt`) are MIT or
Apache-2.0, like the rest of open-chess, and so is the Bluetooth layer
(`chess-ble`). The DGT Pi clock driver (`chess-dgtpi`) is a port of DGT's
`dgtpicom` (GPL-3.0); the Centaur's controller (`chess-centaur`) follows the
protocol as the GPL DGTCentaurMods project worked it out, and the Chessnut
driver (`chess-chessnut`) PicoChess's (GPL-3.0); the device runtime
(`chess-device`) uses them and follows PicoChess's web API, menu and
settings. All four are GPL-3.0-or-later. Nothing MIT/Apache depends on
them, and `deny.toml` allows GPL for those crates only.
