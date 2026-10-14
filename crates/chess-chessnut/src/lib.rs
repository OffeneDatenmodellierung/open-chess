//! Chessnut's boards: the Air, Air+, Pro and (we expect) the Go.
//!
//! They recognise every piece and report the whole position, so a game
//! is followed exactly as on a DGT board (`chess_dgt::Follower`); they
//! light any squares. This is their Bluetooth LE protocol ([`protocol`])
//! and a session that debounces the board's readings ([`session`]), both
//! without I/O. MIT or Apache-2.0: the code is ours, and the byte values
//! and characteristic UUIDs are the board's interface, needed to talk to
//! it. PicoChess (GPL-3.0) speaks the same protocol; nothing of its code is
//! here.

pub mod protocol;
pub mod session;

pub use session::{Event, Session};
