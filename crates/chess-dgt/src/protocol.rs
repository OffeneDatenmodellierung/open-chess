//! The wire format: commands to the board, and the board's messages.

use std::fmt;

use crate::board::{Piece, Placement, field_to_square, square_to_field};
use crate::clock::{ClockReport, parse_clock};

/// A square, a1 = 0, b1 = 1, … h8 = 63.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Square(pub u8);

impl Square {
    /// From a name like `"e4"`.
    pub fn from_name(name: &str) -> Option<Square> {
        let b = name.as_bytes();
        if b.len() != 2 || !(b'a'..=b'h').contains(&b[0]) || !(b'1'..=b'8').contains(&b[1]) {
            return None;
        }
        Some(Square((b[1] - b'1') * 8 + (b[0] - b'a')))
    }

    pub fn name(self) -> String {
        let (file, rank) = (self.0 % 8, self.0 / 8);
        format!("{}{}", char::from(b'a' + file), char::from(b'1' + rank))
    }
}

impl fmt::Debug for Square {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

/// How the clock runs after a set-and-run command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Paused,
    LeftRunning,
    RightRunning,
}

/// What the computer sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Back to idle: no more updates.
    Reset,
    /// One clock message.
    SendClock,
    /// One board dump.
    SendBoard,
    /// Square changes and clock messages from now on.
    SendUpdate,
    /// Square changes only, from now on.
    SendUpdateBoard,
    /// Square changes, and clock messages only when the time changes:
    /// what to use for play.
    SendUpdateNice,
    SerialNumber,
    LongSerialNumber,
    BusAddress,
    Trademark,
    Version,
    /// Bluetooth boards only.
    BatteryStatus,
    /// The moves the board stored while no computer was attached.
    StoredMoves,
    /// Up to eight characters on a DGT3000 (padded with spaces), with an
    /// optional beep: 0 is none, else 62.5 ms plus `beep`/16 of a second
    /// (at most 15).
    ClockText {
        text: String,
        beep: u8,
    },
    /// Up to eleven characters on a Revelation II's display.
    RevelationText {
        text: String,
        beep: u8,
    },
    /// Back to showing the times after a text.
    ClockEndText,
    /// Beep for `units` × 64 ms.
    ClockBeep {
        units: u8,
    },
    /// Ask the clock which buttons are down.
    ClockButtons,
    ClockVersion,
    /// Set both sides' times and start (or pause) the clock. Times are
    /// (hours, minutes, seconds).
    ClockSetAndRun {
        left: (u8, u8, u8),
        right: (u8, u8, u8),
        mode: RunMode,
        lever_toggles: bool,
    },
    /// Revelation II: light the squares from `from` to `to` (one square
    /// when they're the same).
    Lights {
        from: Square,
        to: Square,
    },
    /// Revelation II: all lights off.
    LightsOff,
    /// Bytes as they are, for a board's own extensions (the Pegasus's
    /// lights and unlocking).
    Raw(Vec<u8>),
}

const CLOCK_MESSAGE: u8 = 0x2b;
const CLOCK_START: u8 = 0x03;
const CLOCK_END: u8 = 0x00;

impl Command {
    /// The bytes to write.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Command::Reset => vec![0x40],
            Command::SendClock => vec![0x41],
            Command::SendBoard => vec![0x42],
            Command::SendUpdate => vec![0x43],
            Command::SendUpdateBoard => vec![0x44],
            Command::SerialNumber => vec![0x45],
            Command::BusAddress => vec![0x46],
            Command::Trademark => vec![0x47],
            Command::StoredMoves => vec![0x49],
            Command::SendUpdateNice => vec![0x4b],
            Command::BatteryStatus => vec![0x4c],
            Command::Version => vec![0x4d],
            Command::LongSerialNumber => vec![0x55],
            Command::ClockText { text, beep } => {
                clock(0x0c, &[&padded::<8>(text)[..], &[(*beep).min(15)]].concat())
            }
            Command::RevelationText { text, beep } => {
                clock(0x0d, &[&padded::<11>(text)[..], &[(*beep).min(15)]].concat())
            }
            Command::ClockEndText => clock(0x03, &[]),
            Command::ClockBeep { units } => clock(0x0b, &[*units]),
            Command::ClockButtons => clock(0x08, &[]),
            Command::ClockVersion => clock(0x09, &[]),
            Command::ClockSetAndRun { left, right, mode, lever_toggles } => {
                let run = match mode {
                    RunMode::LeftRunning => 0x01,
                    RunMode::RightRunning => 0x02,
                    RunMode::Paused => 0x04,
                } | if *lever_toggles { 0x08 } else { 0 };
                let (l, r) = (clamp_time(*left), clamp_time(*right));
                clock(0x0a, &[l.0, l.1, l.2, r.0, r.1, r.2, run])
            }
            Command::Lights { from, to } => {
                vec![
                    0x60,
                    0x04,
                    0x01,
                    square_to_field(usize::from(from.0)),
                    square_to_field(usize::from(to.0)),
                    0x00,
                ]
            }
            Command::LightsOff => vec![0x60, 0x04, 0x00, 0x40, 0x40, 0x00],
            Command::Raw(bytes) => bytes.clone(),
        }
    }
}

/// A clock command: `2b`, the length of what follows, `03`, the
/// subcommand, its data, `00`.
fn clock(sub: u8, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(data.len() + 5);
    v.extend_from_slice(&[CLOCK_MESSAGE, (data.len() + 3) as u8, CLOCK_START, sub]);
    v.extend_from_slice(data);
    v.push(CLOCK_END);
    v
}

/// Printable ASCII only (others become spaces), cut or padded to `N`.
fn padded<const N: usize>(text: &str) -> [u8; N] {
    let mut out = [b' '; N];
    for (slot, c) in out.iter_mut().zip(text.chars()) {
        *slot = if c.is_ascii_graphic() || c == ' ' { c as u8 } else { b' ' };
    }
    out
}

fn clamp_time((h, m, s): (u8, u8, u8)) -> (u8, u8, u8) {
    (h.min(9), m.min(59), s.min(59))
}

/// What the board sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Every square.
    Board(Placement),
    /// One square changed: what is on it now.
    Field {
        square: Square,
        piece: Option<Piece>,
    },
    /// The clock's times, or its answer to a clock command.
    Clock(ClockReport),
    Version {
        major: u8,
        minor: u8,
    },
    SerialNumber(String),
    LongSerialNumber(String),
    Trademark(String),
    BusAddress(u16),
    /// Bluetooth boards: the raw battery report.
    Battery(Vec<u8>),
    /// The stored-moves memory, raw.
    StoredMoves(Vec<u8>),
    /// A message this crate doesn't decode (its id without the top bit).
    Other {
        id: u8,
        data: Vec<u8>,
    },
}

/// The longest message accepted. The stored-moves dump is the longest
/// real one (about 8 KiB); anything claiming more is noise.
const MAX_MESSAGE: usize = 16 * 1024;

/// The fixed size of each message (header included), where it has one.
fn fixed_size(id: u8) -> Option<usize> {
    match id {
        0x06 => Some(67), // board dump
        0x0d => Some(10), // clock
        0x0e => Some(5),  // field update
        0x10 => Some(5),  // bus address
        0x11 => Some(8),  // serial number
        0x13 => Some(5),  // version
        0x22 => Some(13), // long serial number
        _ => None,
    }
}

/// Splits the board's byte stream into messages. Feed it whatever the
/// port delivered; take messages until it has no more. Bytes that can't
/// start a message are skipped, so it recovers from a garbled stream.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
    skipped: usize,
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Bytes thrown away so far while looking for a message.
    pub fn skipped(&self) -> usize {
        self.skipped
    }

    /// The next whole message, if one has arrived.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Message> {
        loop {
            // A message starts with a byte whose top bit is set.
            let start = self.buf.iter().position(|b| b & 0x80 != 0);
            match start {
                None => {
                    self.skip(self.buf.len());
                    return None;
                }
                Some(i) => self.skip(i),
            }
            if self.buf.len() < 3 {
                return None;
            }
            let id = self.buf[0] & 0x7f;
            let (hi, lo) = (self.buf[1], self.buf[2]);
            let len = (usize::from(hi) << 7) | usize::from(lo);
            let plausible = hi & 0x80 == 0
                && lo & 0x80 == 0
                && (3..=MAX_MESSAGE).contains(&len)
                && fixed_size(id).is_none_or(|n| n == len);
            if !plausible {
                self.skip(1);
                continue;
            }
            if self.buf.len() < len {
                return None;
            }
            let data: Vec<u8> = self.buf[3..len].to_vec();
            self.buf.drain(..len);
            return Some(decode(id, data));
        }
    }

    fn skip(&mut self, n: usize) {
        self.skipped += n;
        self.buf.drain(..n);
    }
}

fn decode(id: u8, data: Vec<u8>) -> Message {
    let text = |d: &[u8]| String::from_utf8_lossy(d).trim_end_matches(['\0', ' ']).to_owned();
    match id {
        0x06 => {
            let codes: [u8; 64] = data[..64].try_into().unwrap_or([0; 64]);
            Message::Board(Placement::from_dump(&codes))
        }
        0x0e => match field_to_square(data[0]) {
            Some(s) => Message::Field { square: Square(s as u8), piece: Piece::from_code(data[1]) },
            None => Message::Other { id, data },
        },
        0x0d => {
            let d: [u8; 7] = data[..7].try_into().unwrap_or([0; 7]);
            Message::Clock(parse_clock(d))
        }
        0x13 => Message::Version { major: data[0], minor: data[1] },
        0x11 => Message::SerialNumber(text(&data)),
        0x22 => Message::LongSerialNumber(text(&data)),
        0x12 => Message::Trademark(text(&data)),
        0x10 => Message::BusAddress((u16::from(data[0]) << 7) | u16::from(data[1])),
        0x20 => Message::Battery(data),
        0x0f => Message::StoredMoves(data),
        _ => Message::Other { id, data },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::STARTING_PLACEMENT;
    use crate::clock::ClockReport;

    fn frame(id: u8, data: &[u8]) -> Vec<u8> {
        let len = data.len() + 3;
        let mut v = vec![id | 0x80, (len >> 7) as u8, (len & 0x7f) as u8];
        v.extend_from_slice(data);
        v
    }

    fn start_dump() -> Vec<u8> {
        // Board order: a8 … h8 (black pieces), a7 … h7 (black pawns) … h1.
        let mut codes = vec![8, 9, 10, 12, 11, 10, 9, 8];
        codes.extend([7; 8]);
        codes.extend([0; 32]);
        codes.extend([1; 8]);
        codes.extend([2, 3, 4, 6, 5, 4, 3, 2]);
        frame(0x06, &codes)
    }

    #[test]
    fn single_byte_commands() {
        assert_eq!(Command::SendUpdateNice.encode(), [0x4b]);
        assert_eq!(Command::Version.encode(), [0x4d]);
        assert_eq!(Command::SendBoard.encode(), [0x42]);
        assert_eq!(Command::Reset.encode(), [0x40]);
    }

    #[test]
    fn clock_commands_have_their_documented_layout() {
        let t = Command::ClockText { text: "e2e4".into(), beep: 3 }.encode();
        assert_eq!(t, [0x2b, 0x0c, 0x03, 0x0c, b'e', b'2', b'e', b'4', b' ', b' ', b' ', b' ', 3, 0x00]);
        let long = Command::ClockText { text: "much too long".into(), beep: 99 }.encode();
        assert_eq!(&long[4..12], b"much too");
        assert_eq!(long[12], 15, "beep is capped");
        let r = Command::RevelationText { text: "hello".into(), beep: 0 }.encode();
        assert_eq!(r.len(), 17);
        assert_eq!(r[1], 0x0f);
        assert_eq!(Command::ClockEndText.encode(), [0x2b, 0x03, 0x03, 0x03, 0x00]);
        assert_eq!(Command::ClockBeep { units: 16 }.encode(), [0x2b, 0x04, 0x03, 0x0b, 16, 0x00]);
        let run = Command::ClockSetAndRun {
            left: (0, 5, 0),
            right: (1, 2, 70),
            mode: RunMode::RightRunning,
            lever_toggles: true,
        }
        .encode();
        assert_eq!(run, [0x2b, 0x0a, 0x03, 0x0a, 0, 5, 0, 1, 2, 59, 0x0a, 0x00]);
    }

    #[test]
    fn lights_use_the_boards_field_numbers() {
        let e2 = Square::from_name("e2").unwrap();
        let e4 = Square::from_name("e4").unwrap();
        // Field numbers count from a8: e2 is 52, e4 is 36.
        assert_eq!(Command::Lights { from: e2, to: e4 }.encode(), [0x60, 0x04, 0x01, 52, 36, 0x00]);
        assert_eq!(Command::LightsOff.encode(), [0x60, 0x04, 0x00, 0x40, 0x40, 0x00]);
    }

    #[test]
    fn square_names() {
        assert_eq!(Square::from_name("a1"), Some(Square(0)));
        assert_eq!(Square::from_name("h8"), Some(Square(63)));
        assert_eq!(Square(28).name(), "e4");
        assert_eq!(Square::from_name("i1"), None);
        assert_eq!(Square::from_name("e"), None);
    }

    #[test]
    fn decodes_a_board_dump() {
        let mut d = Decoder::new();
        d.push(&start_dump());
        let Some(Message::Board(p)) = d.next() else { panic!() };
        assert_eq!(p.fen(), STARTING_PLACEMENT);
        assert!(d.next().is_none());
    }

    #[test]
    fn messages_split_across_reads_and_noise_between() {
        let mut stream = vec![0x01, 0x02]; // noise before
        stream.extend(frame(0x13, &[1, 9]));
        stream.extend([0x7f]); // noise between
        stream.extend(frame(0x0e, &[36, 1])); // a white pawn arrives on e4
        stream.extend(frame(0x0d, &[0x00, 0x05, 0x00, 0x00, 0x04, 0x59, 0x09]));
        let mut d = Decoder::new();
        let mut got = Vec::new();
        for chunk in stream.chunks(2) {
            d.push(chunk);
            while let Some(m) = d.next() {
                got.push(m);
            }
        }
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(got[0], Message::Version { major: 1, minor: 9 });
        assert_eq!(
            got[1],
            Message::Field { square: Square::from_name("e4").unwrap(), piece: Piece::from_code(1) }
        );
        let Message::Clock(ClockReport::Times(t)) = &got[2] else { panic!("{:?}", got[2]) };
        assert_eq!(t.left.total_seconds(), 4 * 60 + 59);
        assert_eq!(d.skipped(), 3);
    }

    #[test]
    fn a_bad_length_resyncs() {
        // A "board dump" header claiming 10 bytes is not a board dump.
        let mut stream = vec![0x86, 0x00, 0x0a];
        stream.extend(frame(0x13, &[2, 0]));
        let mut d = Decoder::new();
        d.push(&stream);
        assert_eq!(d.next(), Some(Message::Version { major: 2, minor: 0 }));
    }

    #[test]
    fn variable_length_messages() {
        let mut d = Decoder::new();
        d.push(&frame(0x12, b"Digital Game Technology\0"));
        d.push(&frame(0x11, b"12345"));
        assert_eq!(d.next(), Some(Message::Trademark("Digital Game Technology".into())));
        assert_eq!(d.next(), Some(Message::SerialNumber("12345".into())));
        d.push(&frame(0x0f, &[0x6a, 0x01, 0x02]));
        assert_eq!(d.next(), Some(Message::StoredMoves(vec![0x6a, 0x01, 0x02])));
        d.push(&frame(0x30, &[9]));
        assert_eq!(d.next(), Some(Message::Other { id: 0x30, data: vec![9] }));
    }
}
