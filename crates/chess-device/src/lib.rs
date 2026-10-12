//! Play chess on an electronic board.
//!
//! The first piece of open-chess's board runtime: a game on a DGT board
//! (`chess-dgt`) against open-chess's engine or any UCI engine, with an
//! opening book, the moves on a DGT3000 clock and the squares lit on a
//! Revelation II. The `open-chess-device` binary runs it; the
//! [`controller`] is the game logic without I/O, tested on its own.

pub mod controller;
pub mod opponent;
