//! The DGT Pi's built-in clock.
//!
//! A DGT Pi is a Raspberry Pi 3 inside a DGT3000 clock. The board talks to
//! it over USB or Bluetooth like any DGT board (`open-chess-dgt`); the
//! clock is wired to the Pi's I²C pins instead of the board's cable, and
//! speaks its own I²C protocol. This crate is that protocol and its driver:
//! a Rust port of DGT's `dgtpicom` (GPL-3.0), and so GPL-3.0-or-later.
//!
//! * [`protocol`]: the messages, without I/O.
//! * [`driver`]: each command, the answer it waits for and the retries,
//!   over any [`driver::Bus`]; and [`driver::Clock`], the clock on a thread
//!   of its own.
//! * `bcm` (Linux): the Pi's I²C hardware, through `/dev/mem` (root).
//! * [`sim`]: a simulated DGT3000, for tests and trying it without one.

pub mod driver;
pub mod protocol;
pub mod sim;

#[cfg(target_os = "linux")]
pub mod bcm;
