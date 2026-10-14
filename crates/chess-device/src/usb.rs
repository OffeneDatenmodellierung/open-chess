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

/// The reports a HID report descriptor declares: for each report id (0:
/// unnumbered), the bytes of its input and output reports.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reports {
    pub input: Vec<(u8, usize)>,
    pub output: Vec<(u8, usize)>,
}

impl Reports {
    /// Read a report descriptor (only what sizes the reports: Report ID,
    /// Report Size, Report Count, Input, Output, Push and Pop).
    pub fn parse(desc: &[u8]) -> Reports {
        let mut r = Reports::default();
        let (mut id, mut size, mut count) = (0u8, 0u32, 0u32);
        let mut stack: Vec<(u8, u32, u32)> = Vec::new();
        let mut bits_in: Vec<(u8, u32)> = Vec::new();
        let mut bits_out: Vec<(u8, u32)> = Vec::new();
        let add = |v: &mut Vec<(u8, u32)>, id: u8, n: u32| match v.iter_mut().find(|(i, _)| *i == id) {
            Some((_, b)) => *b += n,
            None => v.push((id, n)),
        };
        let mut i = 0;
        while i < desc.len() {
            let prefix = desc[i];
            if prefix == 0xfe {
                // A long item: skip it.
                let len = usize::from(desc.get(i + 1).copied().unwrap_or(0));
                i += 3 + len;
                continue;
            }
            let len = match prefix & 0x03 {
                3 => 4,
                n => usize::from(n),
            };
            let mut value = 0u32;
            for (k, b) in desc.iter().skip(i + 1).take(len).enumerate() {
                value |= u32::from(*b) << (8 * k);
            }
            match prefix & 0xfc {
                0x84 => id = u8::try_from(value).unwrap_or(0),
                0x74 => size = value,
                0x94 => count = value,
                0x80 => add(&mut bits_in, id, size * count),
                0x90 => add(&mut bits_out, id, size * count),
                0xa4 => stack.push((id, size, count)),
                0xb4 => {
                    if let Some((a, b, c)) = stack.pop() {
                        (id, size, count) = (a, b, c);
                    }
                }
                _ => {}
            }
            i += 1 + len;
        }
        let bytes = |v: Vec<(u8, u32)>| v.into_iter().map(|(i, b)| (i, (b as usize).div_ceil(8))).collect();
        r.input = bytes(bits_in);
        r.output = bytes(bits_out);
        r
    }

    /// A command as the device takes it: Chessnut's commands start with
    /// their own code (0x21 …). With numbered output reports and that code
    /// one of them, as it is, padded to the report's size; with unnumbered
    /// reports, after a 0 report id, padded; otherwise as it is.
    pub fn frame(&self, data: &[u8]) -> Vec<u8> {
        let numbered = self.output.iter().any(|(i, _)| *i != 0);
        if let Some(&(_, size)) = self.output.iter().find(|(i, _)| numbered && Some(i) == data.first()) {
            let mut v = data.to_vec();
            v.resize(v.len().max(size + 1), 0);
            v
        } else if let Some(&(_, size)) = self.output.iter().find(|(i, _)| !numbered && *i == 0) {
            let mut v = vec![0];
            v.extend_from_slice(data);
            v.resize(v.len().max(size + 1), 0);
            v
        } else {
            data.to_vec()
        }
    }

    pub fn describe(&self) -> String {
        let list = |v: &[(u8, usize)]| {
            if v.is_empty() {
                "none".to_owned()
            } else {
                v.iter().map(|(i, n)| format!("id {i:#04x} {n} bytes")).collect::<Vec<_>>().join(", ")
            }
        };
        format!("input {}; output {}", list(&self.input), list(&self.output))
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod hidapi_impl {
    use std::io;
    use std::time::Duration;

    use crate::discover::Found;

    const VID: u16 = 0x2d80;
    const USAGE_PAGE: u16 = 0xff00;

    /// An open Chessnut board on USB: every HID interface of it on
    /// Chessnut's vendor page (macOS shows each collection as its own
    /// device; which one carries the board's reports varies).
    pub struct Hid {
        devices: Vec<(String, hidapi::HidDevice, super::Reports)>,
        /// How commands are framed: 0, as the descriptor says; then, while
        /// the board stays quiet, the other ways boards take them.
        framing: u8,
        heard: bool,
        last_switch: Option<std::time::Instant>,
        wrote: u64,
        read: u64,
        /// When it first wrote: a board quiet for a while after that is
        /// asked for its position report directly (GET_REPORT), as some
        /// systems never pass its interrupt reports on.
        first_write: Option<std::time::Instant>,
        polling: bool,
        hinted: bool,
    }

    impl std::fmt::Debug for Hid {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Hid")
        }
    }

    #[allow(clippy::needless_pass_by_value)] // used as map_err(err)
    fn err(e: hidapi::HidError) -> io::Error {
        io::Error::other(e.to_string())
    }

    /// An open that failed, with what to do about it where we know.
    fn explain(e: &hidapi::HidError) -> io::Error {
        let msg = e.to_string();
        if cfg!(target_os = "macos") && msg.contains("privilege violation") {
            io::Error::other(format!(
                "{msg}: macOS refused the board. Allow your terminal under System Settings › Privacy & Security › Input Monitoring, then open a new terminal"
            ))
        } else {
            io::Error::other(msg)
        }
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

    fn hex(data: &[u8]) -> String {
        let mut s: Vec<String> = data.iter().take(24).map(|b| format!("{b:02x}")).collect();
        if data.len() > 24 {
            s.push(format!("… ({} bytes)", data.len()));
        }
        s.join(" ")
    }

    impl Hid {
        pub fn open(path: &str) -> io::Result<Hid> {
            let api = hidapi::HidApi::new().map_err(err)?;
            // hidapi seizes devices on macOS by default, which only root
            // may do: "(iokit/common) privilege violation". Shared access
            // reads and writes the board just the same.
            #[cfg(target_os = "macos")]
            api.set_open_exclusive(false);
            // The board's interfaces: the one named, and its siblings (same
            // product and serial) on the vendor page.
            let named = api.device_list().find(|d| d.path().to_string_lossy() == path);
            let mut paths: Vec<String> = vec![path.to_owned()];
            if let Some(n) = named {
                for d in api.device_list() {
                    let p = d.path().to_string_lossy().into_owned();
                    if p != path
                        && d.vendor_id() == n.vendor_id()
                        && d.product_id() == n.product_id()
                        && d.serial_number() == n.serial_number()
                        && d.usage_page() == USAGE_PAGE
                    {
                        paths.push(p);
                    }
                }
            }
            let mut devices = Vec::new();
            let mut first_err = None;
            for p in &paths {
                let c = std::ffi::CString::new(p.as_str()).map_err(io::Error::other)?;
                match api.open_path(&c) {
                    Ok(dev) => {
                        let mut buf = [0u8; 4096];
                        let reports = dev
                            .get_report_descriptor(&mut buf)
                            .map(|n| super::Reports::parse(&buf[..n]))
                            .unwrap_or_default();
                        crate::log::line(
                            crate::log::Source::Board,
                            format!("USB interface {p}: {}", reports.describe()),
                        );
                        devices.push((p.clone(), dev, reports));
                    }
                    Err(e) => {
                        crate::log::line(crate::log::Source::Board, format!("USB interface {p}: {e}"));
                        first_err.get_or_insert(e);
                    }
                }
            }
            if devices.is_empty() {
                return Err(explain(&first_err.unwrap_or(hidapi::HidError::HidApiErrorEmpty)));
            }
            Ok(Hid {
                devices,
                framing: 0,
                heard: false,
                last_switch: None,
                wrote: 0,
                read: 0,
                first_write: None,
                polling: false,
                hinted: false,
            })
        }

        pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
            // Quiet for a while after writing: try the next way of framing,
            // unless the board's descriptor settled how (its reports
            // include this command's).
            let now = std::time::Instant::now();
            self.first_write.get_or_insert(now);
            let settled =
                self.devices.iter().any(|(_, _, r)| r.output.iter().any(|(i, _)| Some(i) == data.first()));
            if !settled
                && !self.heard
                && self.wrote > 0
                && self.last_switch.is_none_or(|t| now.duration_since(t).as_secs() >= 4)
            {
                self.framing = (self.framing + 1) % 3;
                self.last_switch = Some(now);
                crate::log::line(
                    crate::log::Source::Board,
                    format!(
                        "USB: no reports yet, writing commands {} instead",
                        [
                            "as the board's descriptor says",
                            "after a 0 report id, padded to 64 bytes",
                            "as they are, padded to 64 bytes"
                        ][usize::from(self.framing)]
                    ),
                );
            }
            self.last_switch.get_or_insert(now);
            let mut ok = false;
            let mut last_err = None;
            for (p, dev, reports) in &self.devices {
                let bytes = match self.framing {
                    0 => reports.frame(data),
                    1 => {
                        let mut v = vec![0];
                        v.extend_from_slice(data);
                        v.resize(65, 0);
                        v
                    }
                    _ => {
                        let mut v = data.to_vec();
                        v.resize(64, 0);
                        v
                    }
                };
                match dev.write(&bytes) {
                    Ok(_) => {
                        ok = true;
                        crate::log::debug(crate::log::Source::Board, format!("USB {p} → {}", hex(&bytes)));
                    }
                    Err(e) => {
                        crate::log::line(
                            crate::log::Source::Board,
                            format!("USB {p} → {} failed: {e}", hex(&bytes)),
                        );
                        last_err = Some(e);
                    }
                }
            }
            self.wrote += 1;
            match (ok, last_err) {
                (false, Some(e)) => Err(err(e)),
                _ => Ok(()),
            }
        }

        pub fn reports(&mut self, wait: Duration) -> io::Result<Vec<Vec<u8>>> {
            let mut out = Vec::new();
            let n = i32::try_from(self.devices.len()).unwrap_or(1).max(1);
            let slice = i32::try_from(wait.as_millis()).unwrap_or(i32::MAX) / n;
            for (p, dev, _) in &self.devices {
                let mut ms = slice;
                loop {
                    let mut buf = [0u8; 256];
                    let got = dev.read_timeout(&mut buf, ms).map_err(err)?;
                    if got == 0 {
                        break;
                    }
                    self.read += 1;
                    if crate::log::debugging() && (self.read <= 20 || self.read.is_multiple_of(100)) {
                        crate::log::line(
                            crate::log::Source::Board,
                            format!("USB {p} ← #{} {}", self.read, hex(&buf[..got])),
                        );
                    }
                    // Some systems hand over the report id first: Chessnut's
                    // messages start with their code (01 = position, 2a =
                    // battery), whose second byte is the length.
                    let msg = if buf[0] == 0 && got > 1 { &buf[1..got] } else { &buf[..got] };
                    let len = usize::from(msg.get(1).copied().unwrap_or(0)) + 2;
                    out.push(msg[..msg.len().min(len.max(2))].to_vec());
                    ms = 0;
                }
            }
            // Nothing two seconds after asking: ask for the position report
            // directly (the board's descriptor has it as input report 0x01).
            if out.is_empty()
                && !self.heard
                && self.first_write.is_some_and(|t| t.elapsed() >= Duration::from_secs(2))
            {
                if !self.polling {
                    self.polling = true;
                    crate::log::line(
                        crate::log::Source::Board,
                        "USB: no reports from the board: asking it for its position instead (GET_REPORT)",
                    );
                }
                for (p, dev, reports) in &self.devices {
                    let Some(&(_, size)) = reports.input.iter().find(|(i, _)| *i == 0x01) else { continue };
                    let mut buf = vec![0u8; size + 1];
                    buf[0] = 0x01;
                    match dev.get_input_report(&mut buf) {
                        Ok(n) if n > 1 => {
                            self.read += 1;
                            if crate::log::debugging() && (self.read <= 20 || self.read.is_multiple_of(100)) {
                                crate::log::line(
                                    crate::log::Source::Board,
                                    format!("USB {p} ⇐ #{} {}", self.read, hex(&buf[..n])),
                                );
                            }
                            let len = usize::from(buf.get(1).copied().unwrap_or(0)) + 2;
                            out.push(buf[..n.min(len.max(2))].to_vec());
                        }
                        Ok(_) => {}
                        Err(e) => {
                            crate::log::debug(crate::log::Source::Board, format!("USB {p} GET_REPORT: {e}"))
                        }
                    }
                }
                if out.is_empty() {
                    std::thread::sleep(wait.min(Duration::from_millis(100)));
                    if !self.hinted
                        && self.first_write.is_some_and(|t| t.elapsed() >= Duration::from_secs(12))
                    {
                        self.hinted = true;
                        crate::log::line(
                            crate::log::Source::Board,
                            if cfg!(target_os = "macos") {
                                "USB: the board still says nothing. macOS passes on what USB input devices send only to apps allowed under System Settings › Privacy & Security › Input Monitoring: add your terminal app there, quit it and open it again. Also close Chessnut's own app, which may hold the board."
                            } else {
                                "USB: the board still says nothing: close Chessnut's own app, which may hold the board, and unplug and replug it."
                            },
                        );
                    }
                }
                return Ok(out);
            }
            if !out.is_empty() && !self.heard {
                self.heard = true;
                crate::log::line(crate::log::Source::Board, "USB: the board is sending its position");
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Reports;

    #[test]
    fn report_descriptors() {
        // Vendor page, unnumbered: 64-byte input and output reports.
        let plain = [
            0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x75, 0x08, 0x95, 0x40, 0x09, 0x01, 0x81, 0x02, 0x95,
            0x40, 0x09, 0x01, 0x91, 0x02, 0xc0,
        ];
        let r = Reports::parse(&plain);
        assert_eq!((r.input.clone(), r.output.clone()), (vec![(0, 64)], vec![(0, 64)]));
        let f = r.frame(&[0x21, 0x01, 0x00]);
        assert_eq!((f.len(), &f[..4]), (65, &[0x00, 0x21, 0x01, 0x00][..]));

        // Numbered: input report 0x01 (61 bytes), output report 0x21 (2 bytes).
        let numbered = [
            0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x01, 0x75, 0x08, 0x95, 0x3d, 0x81, 0x02, 0x85,
            0x21, 0x95, 0x02, 0x91, 0x02, 0xc0,
        ];
        let r = Reports::parse(&numbered);
        assert_eq!((r.input.clone(), r.output.clone()), (vec![(0x01, 61)], vec![(0x21, 2)]));
        assert_eq!(r.frame(&[0x21, 0x01, 0x00]), vec![0x21, 0x01, 0x00]);
        assert!(r.describe().contains("id 0x21 2 bytes"));
        // A command that isn't one of its reports goes as it is.
        assert_eq!(r.frame(&[0x0a, 0x08]), vec![0x0a, 0x08]);
        assert_eq!(Reports::parse(&[]).frame(&[1, 2]), vec![1, 2]);
    }
}
