//! The open-chess engine as a UCI program.
//!
//! `open-chess-uci` speaks the Universal Chess Interface, so the engine
//! plays in any GUI and in the match runners the engine-testing world
//! uses (fastchess, cutechess-cli, OpenBench). The same binary carries
//! the two commands a testing rig needs:
//!
//! - `bench [depth]`: the engine's node-count signature and speed
//!   ([`bench`]);
//! - `datagen …`: self-play training data for the evaluation network, in
//!   the trainer's binary format ([`datagen`]).
//!
//! Everything chess lives in `chess-analysis` and `chess-train`; this
//! crate is the I/O around them.

pub mod bench;
pub mod datagen;
pub mod uci;
