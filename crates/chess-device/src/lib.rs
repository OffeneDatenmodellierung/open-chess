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
//!
//! On a DGT Pi the clock is the Pi's own ([`dgtpi`], over I²C) rather than
//! one on the board's cable. That driver is a port of DGT's GPL `dgtpicom`,
//! so this crate is GPL-3.0-or-later.

pub mod board;
pub mod controller;
pub mod dgtpi;
pub mod opponent;
pub mod view;
pub mod web;
