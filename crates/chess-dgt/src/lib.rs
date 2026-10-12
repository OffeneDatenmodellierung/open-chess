//! DGT electronic chessboards, without I/O.
//!
//! DGT's boards (the wooden USB, serial and Bluetooth e-Boards, the Smart
//! Board, the Revelation II) all speak one serial protocol: single-byte
//! commands from the computer, length-prefixed messages back. A clock on
//! the board's cable (DGT3000, DGT XL) is reached through the board too.
//! This crate is that protocol as plain data:
//!
//! * [`Command`] encodes what to send (board requests, clock text, clock
//!   set-and-run, Revelation II square lights);
//! * [`Decoder`] turns the byte stream into [`Message`]s, resynchronising
//!   after noise;
//! * [`Placement`] is a board reading, with its orientation;
//! * [`Follower`] turns a sequence of settled readings into what happened
//!   in the game: a legal move, a takeback, a new game, a position set up.
//!
//! The transport (a serial port, a Bluetooth RFCOMM channel) and the timing
//! (when a reading has settled) belong to the caller, so everything here is
//! tested with bytes alone.
//!
//! The DGT Pi's built-in clock is not on this protocol: it hangs off the
//! Pi's I²C bus (a separate crate).
//!
//! Protocol details follow DGT's published board protocol, cross-checked
//! against the open implementations that drive real boards (PicoChess,
//! godgt, dgtdrv).

mod board;
mod clock;
mod follow;
mod protocol;

pub use board::{Orientation, Piece, PieceKind, Placement, STARTING_PLACEMENT};
pub use clock::{ClockAck, ClockButton, ClockReport, ClockSide, ClockStatus, ClockTimes, SideTime};
pub use follow::{Follower, Observation};
pub use protocol::{Command, Decoder, Message, RunMode, Square};
