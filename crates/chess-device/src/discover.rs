//! Finding boards: serial and USB ports, Chessnut boards on USB, and
//! Bluetooth LE boards nearby.

/// A Bluetooth LE board's kind, from its advertised name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BleKind {
    Chessnut,
    Pegasus,
}

impl BleKind {
    /// What a board advertising this name is.
    pub fn from_name(name: &str) -> Option<BleKind> {
        let n = name.to_ascii_lowercase();
        if n.starts_with("chessnut") {
            Some(BleKind::Chessnut)
        } else if n.contains("pegasus") {
            Some(BleKind::Pegasus)
        } else {
            None
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            BleKind::Chessnut => "chessnut",
            BleKind::Pegasus => "pegasus",
        }
    }
}

/// A board that can be connected to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// A DGT board on a serial, USB or Bluetooth-serial port.
    Serial { path: String, label: String },
    /// The DGT Centaur's own board controller.
    Centaur { path: String },
    /// A Chessnut board on USB.
    ChessnutUsb { path: String, label: String },
    /// A Bluetooth LE board.
    Ble { address: String, name: String, kind: BleKind, random: bool },
}

impl Found {
    /// One line for a list.
    pub fn describe(&self) -> String {
        match self {
            Found::Serial { path, label } => format!("DGT board on {path} ({label})"),
            Found::Centaur { path } => format!("DGT Centaur controller on {path}"),
            Found::ChessnutUsb { path, label } => format!("{label} on USB ({path})"),
            Found::Ble { address, name, random, .. } => {
                format!("{name} over Bluetooth ({address}{})", if *random { ", random" } else { "" })
            }
        }
    }
}

/// COM ports where a DGT board may be (Windows).
#[cfg(windows)]
pub fn serial_ports() -> Vec<Found> {
    serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|p| {
            let label = match p.port_type {
                serialport::SerialPortType::UsbPort(u) => u.product.unwrap_or_else(|| "USB".into()),
                serialport::SerialPortType::BluetoothPort => "Bluetooth".into(),
                _ => "serial".into(),
            };
            Found::Serial { path: p.port_name, label }
        })
        .collect()
}

/// Serial-style ports where a DGT board may be.
#[cfg(not(windows))]
pub fn serial_ports() -> Vec<Found> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir("/dev") else { return out };
    let mut names: Vec<String> = dir.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect();
    names.sort();
    for n in names {
        let label = if n.starts_with("ttyACM") || n.starts_with("cu.usbmodem") {
            "USB"
        } else if n.starts_with("rfcomm") {
            "Bluetooth"
        } else if n.starts_with("ttyUSB") || n.starts_with("cu.usbserial") {
            "USB serial adapter"
        } else {
            continue;
        };
        out.push(Found::Serial { path: format!("/dev/{n}"), label: label.into() });
    }
    // On a Centaur, its controller is on the Pi's own serial port.
    if centaur_here() {
        out.push(Found::Centaur { path: "/dev/serial0".into() });
    }
    out
}

/// Whether this looks like a DGT Centaur (a Pi with DGTCentaurMods or
/// open-chess installed for it, or told so by `OPEN_CHESS_CENTAUR`).
#[cfg(not(windows))]
fn centaur_here() -> bool {
    std::path::Path::new("/dev/serial0").exists()
        && (std::env::var_os("OPEN_CHESS_CENTAUR").is_some()
            || std::path::Path::new("/opt/DGTCentaurMods").exists()
            || std::path::Path::new("/etc/open-chess/centaur").exists())
}

/// Chessnut's USB vendor id, and the product ids its boards use (the
/// high byte), as Chessnut's EasyLink SDK lists them.
const CHESSNUT_VID: u32 = 0x2d80;
const CHESSNUT_PIDS: [u32; 7] = [0x8000, 0x8100, 0x8200, 0x8300, 0x8400, 0x8500, 0x8600];

/// Whether a HID id (`0003:00002D80:00008100`) is a Chessnut board.
pub fn is_chessnut_hid(hid_id: &str) -> bool {
    let parts: Vec<&str> = hid_id.split(':').collect();
    let (Some(v), Some(p)) = (parts.get(1), parts.get(2)) else { return false };
    let (Ok(v), Ok(p)) = (u32::from_str_radix(v, 16), u32::from_str_radix(p, 16)) else { return false };
    v == CHESSNUT_VID && CHESSNUT_PIDS.contains(&(p & 0xff00))
}

/// Chessnut boards on USB (macOS and Windows: through hidapi).
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn chessnut_usb() -> Vec<Found> {
    crate::usb::chessnut_boards()
}

/// Chessnut boards on USB (Linux: their `hidraw` nodes, the interface on
/// Chessnut's vendor usage page 0xFF00).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn chessnut_usb() -> Vec<Found> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir("/sys/class/hidraw") else { return out };
    for e in dir.flatten() {
        let node = e.file_name().to_string_lossy().into_owned();
        let dev = e.path().join("device");
        let Ok(uevent) = std::fs::read_to_string(dev.join("uevent")) else { continue };
        let field = |k: &str| uevent.lines().find_map(|l| l.strip_prefix(k)).map(str::to_owned);
        let Some(hid_id) = field("HID_ID=") else { continue };
        if !is_chessnut_hid(&hid_id) {
            continue;
        }
        let descriptor = std::fs::read(dev.join("report_descriptor")).unwrap_or_default();
        if !descriptor.windows(3).any(|w| w == [0x06, 0x00, 0xff]) {
            continue;
        }
        let label = field("HID_NAME=").unwrap_or_else(|| "Chessnut".into());
        out.push(Found::ChessnutUsb { path: format!("/dev/{node}"), label });
    }
    out.sort_by_key(Found::describe);
    out
}

/// One `bluetoothctl` line's device, if it's a board we know:
/// `[NEW] Device AA:BB:CC:DD:EE:FF Chessnut Air` or `Device … name`.
pub fn bluetoothctl_device(line: &str) -> Option<(String, String, BleKind)> {
    let rest = line[line.find("Device ")? + 7..].trim();
    let (address, name) = rest.split_once(' ')?;
    let name = name.trim();
    let valid = address.len() == 17 && address.split(':').count() == 6;
    let kind = BleKind::from_name(name)?;
    valid.then(|| (address.to_owned(), name.to_owned(), kind))
}

/// Bluetooth LE boards nearby, scanning for `secs` (Linux: through
/// BlueZ's `bluetoothctl`).
pub fn ble_scan(secs: u64) -> Result<Vec<Found>, String> {
    use std::process::Command;
    let run = |args: &[&str]| -> Result<String, String> {
        let out = Command::new("bluetoothctl")
            .args(args)
            .output()
            .map_err(|e| format!("bluetoothctl: {e} (is BlueZ installed?)"))?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let scan = run(&["--timeout", &secs.to_string(), "scan", "on"])?;
    let known = run(&["devices"])?;
    let mut found: Vec<Found> = Vec::new();
    for line in scan.lines().chain(known.lines()) {
        let Some((address, name, kind)) = bluetoothctl_device(line) else { continue };
        if found.iter().any(|f| matches!(f, Found::Ble { address: a, .. } if *a == address)) {
            continue;
        }
        let info = run(&["info", &address]).unwrap_or_default();
        let random = info.lines().next().is_some_and(|l| l.contains("(random)"));
        found.push(Found::Ble { address, name, kind, random });
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_boards() {
        assert!(is_chessnut_hid("0003:00002D80:00008103"));
        assert!(!is_chessnut_hid("0003:00002D80:00001234"));
        assert!(!is_chessnut_hid("0003:0000046D:0000C52B"));
        assert_eq!(
            bluetoothctl_device("[NEW] Device 12:34:56:78:9A:BC Chessnut Air"),
            Some(("12:34:56:78:9A:BC".into(), "Chessnut Air".into(), BleKind::Chessnut))
        );
        assert_eq!(
            bluetoothctl_device("Device 12:34:56:78:9A:BD DGT_PEGASUS_1234").map(|d| d.2),
            Some(BleKind::Pegasus)
        );
        assert_eq!(bluetoothctl_device("[NEW] Device 12:34:56:78:9A:BE Someone's headphones"), None);
    }
}
