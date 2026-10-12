//! Play chess on an electronic board.
//!
//! The first piece of open-chess's board runtime: a game on a DGT board
//! (`chess-dgt`) against open-chess's engine or any UCI engine, with an
//! opening book, the moves on a DGT3000 clock and the squares lit on a
//! Revelation II. The `open-chess-device` binary runs it; the
//! [`controller`] is the game logic without I/O, tested on its own.
//!
//! The device can also serve the Chess lab app and a Lichess-style API
//! ([`web`]), so a phone on the same network shows the game ([`view`]) and
//! changes the device's settings.

pub mod controller;
pub mod opponent;
pub mod view;
pub mod web;
