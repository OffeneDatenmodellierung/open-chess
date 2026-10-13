//! Chessnut boards on USB: the same messages as over Bluetooth, as HID
//! reports (Linux: a `/dev/hidraw` node, plain reads and writes).

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, Timespec, poll};

/// An open Chessnut board on USB.
#[derive(Debug)]
pub struct Hid {
    file: File,
}

impl Hid {
    pub fn open(path: &str) -> io::Result<Hid> {
        let file = OpenOptions::new().read(true).write(true).open(path).map_err(|e| {
            if e.kind() == io::ErrorKind::PermissionDenied {
                io::Error::new(e.kind(), format!("{path}: no permission (add a udev rule, or run as root)"))
            } else {
                e
            }
        })?;
        Ok(Hid { file })
    }

    /// Send a command (its first byte is the report's id, as Chessnut's
    /// own SDK writes them).
    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.file.write_all(data)
    }

    /// The board's reports for up to `wait`.
    pub fn reports(&mut self, wait: Duration) -> io::Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        let mut timeout = wait;
        loop {
            let ts = Timespec {
                tv_sec: timeout.as_secs().try_into().unwrap_or(i64::MAX),
                tv_nsec: timeout.subsec_nanos().into(),
            };
            let mut fds = [PollFd::new(&self.file, PollFlags::IN)];
            if poll(&mut fds, Some(&ts))? == 0 {
                return Ok(out);
            }
            let mut buf = [0u8; 256];
            let n = self.file.read(&mut buf)?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "the board was unplugged"));
            }
            // A report carries its length in its second byte.
            let len = usize::from(buf.get(1).copied().unwrap_or(0)) + 2;
            out.push(buf[..n.min(len.max(2))].to_vec());
            timeout = Duration::ZERO;
        }
    }
}
