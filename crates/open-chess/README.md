# open-chess

open-chess in one program: the Lichess-compatible chess server, the
electronic-board runtime, the photo reader and its training, each a Cargo
feature (all by default).

```sh
cargo install open-chess                                          # both
cargo install open-chess --no-default-features --features server  # a server only
cargo install open-chess --no-default-features --features device  # a board only (a Pi)
cargo install open-chess --features metal                          # everything, reading and training on a Mac's GPU
```

* `open-chess serve | analyse | migrate | bot | photos | healthcheck`: the
  server ([open-chess-server](https://crates.io/crates/open-chess-server)).
* `open-chess device …`: play on an electronic board
  ([open-chess-device](https://crates.io/crates/open-chess-device)). Run
  as `open-chess-device` (a link to it, as the release archives have) it
  is the board runtime straight away.
* `open-chess photo read | start | move` (feature `vision`): read a board
  from a photo, start an online game from it, or play the move a new photo
  shows in a running game ([docs/photos.md](https://github.com/OffeneDatenmodellierung/open-chess/blob/main/docs/photos.md)).
* `open-chess train board-reader …` (feature `train`): train the reader
  ([open-chess-vision-train](https://crates.io/crates/open-chess-vision-train));
  `--features metal` or `cuda` for a GPU.
* `open-chess` alone serves when `DATABASE_URL` is set, and otherwise
  opens the board screen.

The board runtime links GPL-3.0-or-later board drivers, so a build with
the `device` feature is distributed under GPL-3.0-or-later; this crate's
own code and a server-only build are MIT or Apache-2.0.
