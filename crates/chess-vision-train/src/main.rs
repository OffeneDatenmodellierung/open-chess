//! `chess-vision-train`: the board reader's trainer on its own (the same
//! as `open-chess train board-reader`; see the library).

use clap::Parser;

fn main() -> anyhow::Result<()> {
    open_chess_vision_train::run(&open_chess_vision_train::Args::parse())
}
