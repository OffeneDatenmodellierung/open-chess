//! Chessnut's boards: the Air, Air+, Pro and (we expect) the Go.
//!
//! They recognise every piece and report the whole position, so a game
//! is followed exactly as on a DGT board (`chess_dgt::Follower`); they
//! light any squares. This is their Bluetooth LE protocol ([`protocol`])
//! and a session that debounces the board's readings ([`session`]), both
//! without I/O. The protocol is as PicoChess (GPL-3.0) speaks it, so this
//! crate is GPL-3.0-or-later.

pub mod protocol;
pub mod session;

pub use session::{Event, Session};
