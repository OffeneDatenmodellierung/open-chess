//! Chessnut boards on USB: the same messages as over Bluetooth, as HID
//! reports (Linux: a `/dev/hidraw` node, plain reads and writes; macOS and
//! Windows: through hidapi).

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use hidapi_impl::{Hid, chessnut_boards};
#[cfg(target_os = "linux")]
pub use linux::Hid;

#[cfg(target_os = "linux")]
mod linux {
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
                    io::Error::new(
                        e.kind(),
                        format!("{path}: no permission (add a udev rule, or run as root)"),
                    )
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
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod hidapi_impl {
    use std::io;
    use std::time::Duration;

    use crate::discover::Found;

    const VID: u16 = 0x2d80;
    const USAGE_PAGE: u16 = 0xff00;

    /// An open Chessnut board on USB.
    pub struct Hid {
        device: hidapi::HidDevice,
    }

    impl std::fmt::Debug for Hid {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Hid")
        }
    }

    fn err(e: hidapi::HidError) -> io::Error {
        io::Error::other(e.to_string())
    }

    /// Chessnut boards on USB.
    pub fn chessnut_boards() -> Vec<Found> {
        let Ok(api) = hidapi::HidApi::new() else { return Vec::new() };
        api.device_list()
            .filter(|d| {
                d.vendor_id() == VID
                    && d.usage_page() == USAGE_PAGE
                    && crate::discover::is_chessnut_hid(&format!(
                        "0003:{:08X}:{:08X}",
                        d.vendor_id(),
                        d.product_id()
                    ))
            })
            .map(|d| Found::ChessnutUsb {
                path: d.path().to_string_lossy().into_owned(),
                label: d.product_string().unwrap_or("Chessnut").to_owned(),
            })
            .collect()
    }

    impl Hid {
        pub fn open(path: &str) -> io::Result<Hid> {
            let api = hidapi::HidApi::new().map_err(err)?;
            let c = std::ffi::CString::new(path).map_err(io::Error::other)?;
            Ok(Hid { device: api.open_path(&c).map_err(err)? })
        }

        pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
            self.device.write(data).map(|_| ()).map_err(err)
        }

        pub fn reports(&mut self, wait: Duration) -> io::Result<Vec<Vec<u8>>> {
            let mut out = Vec::new();
            let mut ms = i32::try_from(wait.as_millis()).unwrap_or(i32::MAX);
            loop {
                let mut buf = [0u8; 256];
                let n = self.device.read_timeout(&mut buf, ms).map_err(err)?;
                if n == 0 {
                    return Ok(out);
                }
                let len = usize::from(buf.get(1).copied().unwrap_or(0)) + 2;
                out.push(buf[..n.min(len.max(2))].to_vec());
                ms = 0;
            }
        }
    }
}
