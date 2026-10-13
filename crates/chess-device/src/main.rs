//! `open-chess-device`: the board runtime on its own (the same as
//! `open-chess device`; see [`open_chess_device::cli`]).

use std::process::ExitCode;

use clap::CommandFactory;
use open_chess_device::cli::{Args, main as run};

fn main() -> ExitCode {
    run(&Args::command().get_matches())
}
