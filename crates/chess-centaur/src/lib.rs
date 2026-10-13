//! The DGT Centaur.
//!
//! A Centaur is a Raspberry Pi Zero inside the board, with an e-paper
//! screen, talking to the board's controller over its serial port. The
//! controller senses whether each square has a piece (not which), lights
//! squares, plays sounds and reads six buttons.
//!
//! * [`protocol`]: the controller's packets, without I/O.
//! * [`follow`]: following a game from occupancy, lifts and put-downs.
//! * [`session`]: the conversation (address, polling), without I/O.
//! * [`screen`] and [`epd`]: the e-paper screen, what to draw and the
//!   panel's commands (and, on Linux, the Pi's SPI and GPIO for it).
//!
//! The protocol is as the DGTCentaurMods project (GPL-3.0) worked it out,
//! so this crate is GPL-3.0-or-later.

pub mod epd;
pub mod follow;
pub mod protocol;
pub mod screen;
pub mod session;

pub use follow::Follower;
pub use protocol::{Address, Button, Field, Sound};
pub use session::{Event, Session};
