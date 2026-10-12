//! The Chessnut Air's Bluetooth LE protocol.
//!
//! Commands go to one characteristic ([`WRITE`]) and the board's messages
//! come as notifications from another ([`READ`]). After connecting, ask for
//! real-time mode ([`realtime`]) and the board sends the whole position
//! whenever it changes: `01 24`, then 32 bytes with a 4-bit piece code per
//! square, then 4 more. [`battery`] asks for the battery (`2a 02 percent
//! charging`). [`lights`] lights any squares (`0a 08` and a 64-bit map).

use chess_dgt::{Orientation, Piece, PieceKind, Placement};

/// The characteristic the board notifies on.
pub const READ: &str = "1b7e8262-2877-41c3-b46e-cf057c562023";
/// The characteristic commands are written to.
pub const WRITE: &str = "1b7e8272-2877-41c3-b46e-cf057c562023";

/// Real-time mode: the position whenever it changes.
pub fn realtime() -> Vec<u8> {
    vec![0x21, 0x01, 0x00]
}

/// Ask for the battery.
pub fn battery() -> Vec<u8> {
    vec![0x29, 0x01, 0x00]
}

/// Light these squares (0 = a1), the board turned as `orientation` says.
pub fn lights(squares: &[u8], orientation: Orientation) -> Vec<u8> {
    let mut map = [0u8; 8];
    for &s in squares.iter().filter(|&&s| s < 64) {
        let s = if orientation == Orientation::Rotated { 63 - s } else { s };
        // Byte 0 is rank 8; within a byte, the a-file is the top bit.
        let (rank, file) = (s / 8, s % 8);
        map[usize::from(7 - rank)] |= 0x80 >> file;
    }
    let mut c = vec![0x0a, 0x08];
    c.extend_from_slice(&map);
    c
}

/// Every light off.
pub fn lights_off() -> Vec<u8> {
    lights(&[], Orientation::Normal)
}

fn piece(code: u8) -> Result<Option<Piece>, u8> {
    let (kind, white) = match code {
        0 => return Ok(None),
        0x07 => (PieceKind::Pawn, true),
        0x06 => (PieceKind::Rook, true),
        0x0a => (PieceKind::Knight, true),
        0x09 => (PieceKind::Bishop, true),
        0x0b => (PieceKind::Queen, true),
        0x0c => (PieceKind::King, true),
        0x04 => (PieceKind::Pawn, false),
        0x08 => (PieceKind::Rook, false),
        0x05 => (PieceKind::Knight, false),
        0x03 => (PieceKind::Bishop, false),
        0x01 => (PieceKind::Queen, false),
        0x02 => (PieceKind::King, false),
        other => return Err(other),
    };
    Ok(Some(Piece { kind, white }))
}

fn code(p: Option<Piece>) -> u8 {
    let Some(p) = p else { return 0 };
    match (p.kind, p.white) {
        (PieceKind::Pawn, true) => 0x07,
        (PieceKind::Rook, true) => 0x06,
        (PieceKind::Knight, true) => 0x0a,
        (PieceKind::Bishop, true) => 0x09,
        (PieceKind::Queen, true) => 0x0b,
        (PieceKind::King, true) => 0x0c,
        (PieceKind::Pawn, false) => 0x04,
        (PieceKind::Rook, false) => 0x08,
        (PieceKind::Knight, false) => 0x05,
        (PieceKind::Bishop, false) => 0x03,
        (PieceKind::Queen, false) => 0x01,
        (PieceKind::King, false) => 0x02,
    }
}

/// Which square a nibble of the position describes: byte `b` (0–31),
/// high nibble or low. Byte 0 is the h8 end of rank 8, read backwards:
/// its high nibble is g8 and its low nibble h8.
fn square_of(b: usize, high: bool) -> usize {
    let (row, col) = (b / 4, b % 4);
    (7 - row) * 8 + (3 - col) * 2 + usize::from(!high)
}

/// A message from the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Position(Placement),
    /// Percent, and whether it's charging.
    Battery {
        percent: u8,
        charging: bool,
    },
}

/// The board's 32 position bytes for a placement (for tests and
/// simulations).
pub fn position_bytes(p: &Placement) -> Vec<u8> {
    let mut data = vec![0u8; 32];
    for (b, byte) in data.iter_mut().enumerate() {
        *byte = (code(p.0[square_of(b, true)]) << 4) | code(p.0[square_of(b, false)]);
    }
    let mut m = vec![0x01, 0x24];
    m.extend_from_slice(&data);
    m.extend_from_slice(&[0, 0, 0, 0]);
    m
}

/// Splits the notifications into messages (one may span two).
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        if self.buf.len() > 4096 {
            self.buf.clear();
        }
    }

    pub fn next_message(&mut self) -> Option<Message> {
        loop {
            match self.buf.as_slice() {
                [0x01, 0x24, rest @ ..] => {
                    if rest.len() < 32 {
                        return None;
                    }
                    let mut p = Placement::EMPTY;
                    let mut ok = true;
                    for (b, &byte) in rest[..32].iter().enumerate() {
                        match (piece(byte >> 4), piece(byte & 0x0f)) {
                            (Ok(hi), Ok(lo)) => {
                                p.0[square_of(b, true)] = hi;
                                p.0[square_of(b, false)] = lo;
                            }
                            _ => ok = false,
                        }
                    }
                    let take = (2 + 36).min(self.buf.len());
                    self.buf.drain(..take);
                    if ok {
                        return Some(Message::Position(p));
                    }
                }
                [0x2a, 0x02, percent, charging, ..] => {
                    let m = Message::Battery { percent: (*percent).min(100), charging: *charging == 1 };
                    self.buf.drain(..4);
                    return Some(m);
                }
                // The start of a message: wait for the rest.
                [] | [0x01 | 0x2a] | [0x2a, 0x02, ..] => return None,
                _ => {
                    self.buf.remove(0);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chess_dgt::STARTING_PLACEMENT;

    #[test]
    fn reads_positions_like_picochess() {
        let start = Placement::from_fen(STARTING_PLACEMENT).unwrap();
        let bytes = position_bytes(&start);
        // Rank 8 comes first, from h8 backwards: "rnbqkbnr" reversed in
        // pairs as (g8,h8) (e8,f8) (c8,d8) (a8,b8): n r, k b, b q, r n.
        assert_eq!(&bytes[2..6], [0x58, 0x23, 0x31, 0x85]);
        // Rank 1 last: (g1,h1) = N R = 0xa6.
        assert_eq!(bytes[30], 0xa6);
        let mut d = Decoder::default();
        d.push(&bytes[..20]);
        assert_eq!(d.next_message(), None, "half a message");
        d.push(&bytes[20..]);
        d.push(&[0x2a, 0x02, 87, 1]);
        assert_eq!(d.next_message(), Some(Message::Position(start)));
        assert_eq!(d.next_message(), Some(Message::Battery { percent: 87, charging: true }));
        assert_eq!(d.next_message(), None);
    }

    #[test]
    fn lights_squares() {
        // e2 (12) and e4 (28): rank 2 is byte 6, rank 4 byte 4; e is 0x08.
        assert_eq!(lights(&[12, 28], Orientation::Normal), [0x0a, 0x08, 0, 0, 0, 0, 0x08, 0, 0x08, 0]);
        // Turned round, e2 is where d7 would be.
        assert_eq!(lights(&[12], Orientation::Rotated), [0x0a, 0x08, 0, 0x10, 0, 0, 0, 0, 0, 0]);
        assert_eq!(lights_off(), [0x0a, 0x08, 0, 0, 0, 0, 0, 0, 0, 0]);
    }
}
