//! A small Bluetooth LE client for chess boards.
//!
//! Electronic boards with Bluetooth LE (the DGT Pegasus, Chessnut's boards)
//! each offer a GATT service: a characteristic to write commands to and
//! one that notifies what the board says. This crate is just enough GATT
//! to use them: find the characteristics, subscribe, write, and read
//! notifications ([`gatt`], no I/O), over the Linux kernel's L2CAP socket
//! for the attribute protocol ([`l2cap`]), so it needs neither BlueZ's
//! D-Bus API nor any system library, and cross-compiles for a Raspberry Pi
//! as it is.

pub mod gatt;
pub mod uuid;

#[cfg(target_os = "linux")]
pub mod l2cap;

pub use gatt::{Characteristic, Gatt, Link};
pub use uuid::Uuid;
