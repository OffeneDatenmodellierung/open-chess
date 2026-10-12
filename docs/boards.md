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

## Coming next

In order; each lands in open-chess as it's ready.

1. **Play the engine on the board.** Done (above): levels, UCI engines,
   Polyglot books, time controls on the clock and its buttons. PicoChess's
   full clock menu comes with PicoChess compatibility (4).
2. **The Chess lab app as the board's screen.** The device serves the app;
   a phone on the same Wi-Fi shows the game, the engine's lines and the
   clock, and drives the device's settings. It speaks the same Lichess-style
   API as the server, so the app needs only a "device" entry.
3. **Online play from the board** on lichess.org or an open-chess server,
   through the Board API, no browser needed.
4. **PicoChess compatibility.** The device answers PicoChess's web API (the
   `/event` websocket and `/channel` commands), so PicoChess's own web page,
   its relay and its MCP server work against it, and the `picochess.ini`
   settings people already have carry over.
5. **Lichess's DGT connector** ([lichess.org/dgt](https://lichess.org/dgt)):
   the device answers the LiveChess websocket that page talks to
   (`ws://localhost:1982/api/v1.0`), so the page works with boards LiveChess
   doesn't drive. Browsers allow that plain websocket only to `localhost`:
   on the device's own screen, or the runtime on the computer with the
   browser.

## Devices

| Device | Inside | Status |
|---|---|---|
| DGT USB / Bluetooth e-Board, Smart Board, Revelation II | — (a computer drives it) | Protocol done (`chess-dgt`); runtime next |
| DGT Pi | Raspberry Pi 3 in a DGT3000 clock; the board on USB or Bluetooth | Board via `chess-dgt`; the built-in clock (I²C) is a port of DGT's GPL `dgtpicom` to Rust, planned. Install: a 64-bit Raspberry Pi OS card with the runtime, like PicoChess v4 |
| DGT Centaur | Raspberry Pi Zero (swap for a Zero 2 W), board controller on serial, e-paper | Planned, after the DGT Pi; occupancy-only sensing needs lift-and-place move reading |
| DGT Pegasus | Bluetooth LE | Later |
| Raspberry Pi 4 / 5 with any USB or Bluetooth board | — | With the runtime; a DGT3000 on a Pi 5 needs a different clock backend |

Nothing here touches a device's own firmware: a DGT Pi or Centaur boots from
an SD card, so trying open-chess means a second card, and putting the
original back restores it.

## Licences

The board protocol and the following of games (`chess-dgt`) are MIT or
Apache-2.0, like the rest of open-chess. The device runtime will port parts
of PicoChess (GPL-3.0) and DGT's `dgtpicom` (GPL-3.0), so those crates will be
GPL-3.0-or-later; nothing MIT/Apache depends on them.
