# open-chess

open-chess in one program: the Lichess-compatible chess server and the
electronic-board runtime, each a Cargo feature (both by default).

```sh
cargo install open-chess                                          # both
cargo install open-chess --no-default-features --features server  # a server only
cargo install open-chess --no-default-features --features device  # a board only (a Pi)
```

* `open-chess serve | analyse | migrate | bot | photos | healthcheck`: the
  server ([open-chess-server](https://crates.io/crates/open-chess-server)).
* `open-chess device …`: play on an electronic board
  ([open-chess-device](https://crates.io/crates/open-chess-device)). Run
  as `open-chess-device` (a link to it, as the release archives have) it
  is the board runtime straight away.
* `open-chess` alone serves when `DATABASE_URL` is set, and otherwise
  opens the board screen.

The board runtime links GPL-3.0-or-later board drivers, so a build with
the `device` feature is distributed under GPL-3.0-or-later; this crate's
own code and a server-only build are MIT or Apache-2.0.
