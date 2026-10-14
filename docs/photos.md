# Reading a board from a photo

`open-chess photo` reads a chess position from a photo of a real board.
With it you can:

* **see the position** a photo shows, as a board and a FEN;
* **carry a game over to an online game**: start a game on lichess.org or
  an open-chess server from the position on your board;
* **keep playing from the board**: send the move a new photo shows to the
  online game, then hear the reply to make on the board.

The same reader runs in the Chess lab app (the camera button) and on the
server (`POST /api/esl/recognise`, in [api.md](api.md)). `open-chess train
board-reader` trains it (below).

## Where the reading happens

A reading gives each square's piece probabilities, so one or two misread
squares don't spoil a move: in a game, the photo is weighed against every
legal move from the position, and the likeliest is chosen.

| How | When | Options |
|---|---|---|
| **The reader here** | you have the weights (a `.safetensors` file) | `--weights FILE` or `OPEN_CHESS_BOARD_READER`; `--compute cpu\|metal\|cuda\|auto` |
| **An open-chess server** | no weights here; the server reads photos (`CHESS_VISION_LOCAL_MODEL` or `CHESS_VISION_ENDPOINT`) | `--server URL --token TOKEN` |
| **By hand** | no photo: you type the board | `--placement FEN-placement` |

Lichess doesn't read photos, so play on Lichess needs weights here.
A photo the server wasn't sure of is kept for review. When the move a
photo shows is played, its position goes back as the photo's label, so
it becomes training data.

Take photos from above or at an angle, from any side (the reader works out
which), with the whole board in the frame and good light. JPEG, PNG and
WebP all work. Convert iPhone HEIC photos to JPEG first.

## The position in a photo

```sh
open-chess photo read board.jpg --weights board-reader.safetensors
open-chess photo read board.jpg --server https://chess.example.com --token ocp_…
open-chess photo read board.jpg --json        # each square's probabilities too
```

It prints the board, with squares it was unsure of marked `?`, and the FEN
(White to move unless you pass `--turn black`). Castling is assumed
wherever the king and rook are on their home squares.

## A game from the board to online play

```sh
export OPEN_CHESS_SERVER=https://lichess.org OPEN_CHESS_TOKEN=lip_…   # or an open-chess server and its token
open-chess photo start board.jpg --opponent anna --turn black --time 15+10
open-chess photo start board.jpg --computer 4          # the server's computer, level 1–8
```

It shows the position it read and asks before starting (`--yes` doesn't
ask). The options:
* `--turn`: whose move it is;
* `--colour`: the side you play (default White);
* `--castling KQkq` or `-`: the castling rights, when the defaults don't
  fit (a king that moved and went back);
* `--time minutes+seconds`: the clock (default: none).

A challenge to a player starts once they accept. Games from a set
position are never rated: Lichess doesn't rate them.

The token needs `board:play` and `challenge:write`. On Lichess, make it at
lichess.org/account/oauth/token. On an open-chess server, make it on its
Account page.

## Playing on from the board

Make your move on the board, take a photo, and:

```sh
open-chess photo move board.jpg                 # your only game where it's your move
open-chess photo move board.jpg --game x4dmA43k --wait
```

It prints the move it found, and how likely it is, with any close
alternatives:
* **On a terminal**, it asks before playing.
* **Run from a script or a phone shortcut**, it plays only a match that's
  at least 90% likely.
* `--yes` plays the best match anyway.
* `--dry-run` only shows it.

With `--wait` it then waits for your opponent's reply and says how to make
it on the board ("Black played Nf6: on the board, move g8 to f6").

It also tells you when:
* the photo shows the position as it is, with nothing played yet;
* your opponent's last move isn't on the board yet ("make it on the
  board, then take the photo again");
* the photo doesn't match any legal move.

## Training the reader

The weights come from `open-chess train board-reader`, which takes the
same options as the `chess-vision-train` program:

```sh
open-chess train board-reader --datasets esl-review --data-root /data/chess \
    --dinov2 dinov2_vits14_pretrain.pth --encoder small --image-size 392 --out runs/small-392
```

The macOS downloads train on the Mac's GPU. Elsewhere, build with
`--features cuda` for an NVIDIA GPU.

Every dataset passes a licence check first. ChessReD's licence is
noncommercial, so a model trained on it needs `--internal` and is stamped
as internal-only. Your own server's confirmed photos make a training set
with `open-chess photos export --out /data/chess/esl-review`. Datasets,
splits and model cards are in the trainer's
[README](../crates/chess-vision-train/README.md).
