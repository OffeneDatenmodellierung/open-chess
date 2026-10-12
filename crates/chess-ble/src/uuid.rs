//! Bluetooth UUIDs: 128-bit, or 16-bit ones on the Bluetooth base UUID.

use std::fmt;
use std::str::FromStr;

/// A 128-bit UUID, stored big-endian (as written).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Uuid(pub [u8; 16]);

const BASE: [u8; 16] = [0, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0x80, 0x5f, 0x9b, 0x34, 0xfb];

impl Uuid {
    /// A 16-bit UUID (0x2902) on the base UUID.
    pub const fn short(n: u16) -> Uuid {
        let mut b = BASE;
        b[2] = (n >> 8) as u8;
        b[3] = n as u8;
        Uuid(b)
    }

    /// From the little-endian bytes on the wire: 2 or 16 of them.
    pub fn from_wire(bytes: &[u8]) -> Option<Uuid> {
        match bytes.len() {
            2 => Some(Uuid::short(u16::from_le_bytes([bytes[0], bytes[1]]))),
            16 => {
                let mut b = [0u8; 16];
                for (i, &x) in bytes.iter().rev().enumerate() {
                    b[i] = x;
                }
                Some(Uuid(b))
            }
            _ => None,
        }
    }

    /// The little-endian bytes for the wire: 2 for a base UUID, else 16.
    pub fn to_wire(&self) -> Vec<u8> {
        if self.0[..2] == [0, 0] && self.0[4..] == BASE[4..] {
            vec![self.0[3], self.0[2]]
        } else {
            self.0.iter().rev().copied().collect()
        }
    }
}

impl FromStr for Uuid {
    type Err = String;

    fn from_str(s: &str) -> Result<Uuid, String> {
        let hex: String = s.chars().filter(|c| *c != '-').collect();
        if hex.len() == 4 {
            return u16::from_str_radix(&hex, 16).map(Uuid::short).map_err(|e| format!("{s}: {e}"));
        }
        if hex.len() != 32 {
            return Err(format!("{s}: not a UUID"));
        }
        let mut b = [0u8; 16];
        for (i, slot) in b.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|e| format!("{s}: {e}"))?;
        }
        Ok(Uuid(b))
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, b) in self.0.iter().enumerate() {
            if matches!(i, 4 | 6 | 8 | 10) {
                f.write_str("-")?;
            }
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_uuids() {
        let u: Uuid = "1b7e8262-2877-41c3-b46e-cf057c562023".parse().unwrap();
        assert_eq!(u.to_string(), "1b7e8262-2877-41c3-b46e-cf057c562023");
        assert_eq!(Uuid::from_wire(&u.to_wire()), Some(u));
        assert_eq!(u.to_wire()[0], 0x23, "little-endian on the wire");
        let cccd = Uuid::short(0x2902);
        assert_eq!(cccd.to_string(), "00002902-0000-1000-8000-00805f9b34fb");
        assert_eq!(cccd.to_wire(), [0x02, 0x29]);
        assert_eq!("2902".parse::<Uuid>(), Ok(cccd));
    }
}
