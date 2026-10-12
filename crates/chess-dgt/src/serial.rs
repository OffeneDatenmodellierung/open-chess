//! Talking to a board over a serial line (feature `serial`): a USB board
//! (`/dev/ttyACM0` on Linux, `/dev/cu.usbmodem…` on a Mac), a serial
//! adapter, or a Bluetooth board bound to an RFCOMM port (`/dev/rfcomm0`).
//! All of these are terminals, set here to raw 9600 baud 8N1.
//!
//! [`run`] is a plain loop on the calling thread: it writes what the
//! [`Session`] asks for, reads what the board sends, and hands each
//! [`Event`] to the application, which can queue commands on the session.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Instant;

use rustix::fs::{Mode, OFlags};
use rustix::termios::{OptionalActions, QueueSelector, SpecialCodeIndex, tcflush, tcgetattr, tcsetattr};

use crate::session::{Event, Session};

/// An open board port.
#[derive(Debug)]
pub struct Port {
    file: File,
}

impl Port {
    /// Open and configure a board's port: raw, 9600 baud, 8N1, reads that
    /// return after at most a tenth of a second.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Port> {
        Port::open_at(path, 9600)
    }

    /// The same at another speed (a DGT Centaur's board controller runs at
    /// 1,000,000 baud).
    pub fn open_at(path: impl AsRef<Path>, baud: u32) -> io::Result<Port> {
        let fd =
            rustix::fs::open(path.as_ref(), OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC, Mode::empty())?;
        let mut t = tcgetattr(&fd)?;
        t.make_raw();
        t.set_speed(baud)?;
        t.special_codes[SpecialCodeIndex::VMIN] = 0;
        t.special_codes[SpecialCodeIndex::VTIME] = 1;
        tcsetattr(&fd, OptionalActions::Now, &t)?;
        // Whatever was waiting from before is stale.
        tcflush(&fd, QueueSelector::IOFlush)?;
        Ok(Port { file: File::from(fd) })
    }

    /// Send a command.
    pub fn send(&mut self, c: &crate::Command) -> io::Result<()> {
        self.write_all(&c.encode())
    }

    /// Write raw bytes (for boards with protocols of their own).
    pub fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all(bytes)?;
        self.file.flush()
    }

    /// Bytes waiting, or none after the read timeout (a tenth of a second),
    /// for applications running their own loop instead of [`run`].
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.file.read(buf) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Ok(0),
            r => r,
        }
    }
}

/// What the application wants after an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

/// Run a session on a port until the handler says stop or the port fails
/// (a board unplugged gives an error or end of file; reopen and run again
/// with the same session to carry on the game).
pub fn run<F>(port: &mut Port, session: &mut Session, mut on_event: F) -> io::Result<()>
where
    F: FnMut(&Event, &mut Session) -> Flow,
{
    if !session.connected() {
        session.start(Instant::now());
    }
    let mut buf = [0u8; 512];
    loop {
        for c in session.take_commands() {
            port.send(&c)?;
        }
        let n = port.read(&mut buf)?;
        let now = Instant::now();
        if n > 0 {
            for e in session.receive(&buf[..n], now) {
                if on_event(&e, session) == Flow::Stop {
                    return Ok(());
                }
            }
        }
        session.poll(now);
    }
}
