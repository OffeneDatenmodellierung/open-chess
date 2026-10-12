//! The DGT3000's I²C messages, without I/O.
//!
//! Every message is `[to, from, length, command, data…, crc]`: `to` and
//! `from` are I²C addresses shifted left one bit (the clock is 8, written
//! 16; the Pi is 16, written 32; 0 is a broadcast), `length` counts the
//! whole message including the address byte and the CRC, and the CRC is
//! CRC-8 (polynomial 0x07) over everything before it. When sending, the
//! I²C hardware puts the address on the wire itself, so the bytes written
//! start at `from`; when receiving, the first byte is the address the Pi
//! was listening on.

/// The clock's I²C address.
pub const CLOCK: u8 = 8;
/// The Pi's I²C address (as a slave, where the clock answers).
pub const PI: u8 = 16;
/// The address a sleeping clock wakes on.
pub const WAKE: u8 = 40;
/// Broadcasts.
pub const BROADCAST: u8 = 0;

/// Message types (the `command` byte).
pub mod cmd {
    pub const ACK: u8 = 1;
    pub const HELLO: u8 = 2;
    pub const TIME: u8 = 4;
    pub const BUTTON: u8 = 5;
    pub const DISPLAY: u8 = 6;
    pub const END_DISPLAY: u8 = 7;
    pub const SET_AND_RUN: u8 = 10;
    pub const CHANGE_STATE: u8 = 11;
    pub const PING: u8 = 13;
    pub const SET_CENTRAL_CONTROL: u8 = 15;
}

/// CRC-8, polynomial 0x07, initial value 0.
pub fn crc8(bytes: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &b in bytes {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x07 } else { crc << 1 };
        }
    }
    crc
}

/// A whole message from the Pi to `to`, with its length and CRC.
fn message(to: u8, command: u8, data: &[u8]) -> Vec<u8> {
    let len = u8::try_from(data.len() + 5).expect("short message");
    let mut m = vec![to << 1, PI << 1, len, command];
    m.extend_from_slice(data);
    m.push(crc8(&m));
    m
}

/// Wake a clock that's off (sent to [`WAKE`]; it answers with a hello).
/// The checksum is the one for the clock's own address, as DGT sends it:
/// a sleeping clock checks the message as if addressed to it.
pub fn ping() -> Vec<u8> {
    let mut m = message(CLOCK, cmd::PING, &[]);
    m[0] = WAKE << 1;
    m
}

/// Take control of the clock from its own program.
pub fn set_central_control() -> Vec<u8> {
    message(CLOCK, cmd::SET_CENTRAL_CONTROL, &[])
}

/// Switch the clock to mode 25, where the Pi sets its times.
pub fn mode_25() -> Vec<u8> {
    message(CLOCK, cmd::CHANGE_STATE, &[32 + 25])
}

/// Switch the clock off (it starts in its own timing mode next time).
pub fn off() -> Vec<u8> {
    message(CLOCK, cmd::CHANGE_STATE, &[0])
}

/// Back to the times after a text.
pub fn end_display() -> Vec<u8> {
    message(CLOCK, cmd::END_DISPLAY, &[])
}

/// Icons and dots beside the text.
pub mod icons {
    pub const FLAG: u8 = 1;
    pub const WHITE_KING: u8 = 2;
    pub const BLACK_KING: u8 = 4;
    pub const COLON: u8 = 8;
    pub const DOT: u8 = 16;
    /// The left side only.
    pub const EXTRA_DOT: u8 = 32;
}

/// Characters the display shows: up to eleven.
pub const DISPLAY_WIDTH: usize = 11;

/// Show a text (up to eleven characters), with a beep of `beep` × 62.5 ms
/// (at most 48, three seconds) and icons on the left and right
/// ([`icons`]).
pub fn display(text: &str, beep: u8, left_icons: u8, right_icons: u8) -> Vec<u8> {
    let mut chars = [b' '; DISPLAY_WIDTH];
    for (slot, c) in chars.iter_mut().zip(text.chars()) {
        *slot = if c.is_ascii() && !c.is_ascii_control() { c as u8 } else { b'?' };
    }
    let mut data = chars.to_vec();
    data.extend_from_slice(&[0xff, beep.min(48), 3, left_icons, right_icons]);
    message(CLOCK, cmd::DISPLAY, &data)
}

/// How a side of the clock runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Run {
    Stop,
    Down,
    Up,
}

impl Run {
    fn bits(self) -> u8 {
        match self {
            Run::Stop => 0,
            Run::Down => 1,
            Run::Up => 2,
        }
    }
}

/// One side's time: hours (0–9), minutes, seconds.
pub type Hms = (u8, u8, u8);

fn bcd(n: u8) -> u8 {
    ((n / 10) << 4) | (n % 10)
}

fn from_bcd(b: u8) -> u8 {
    (b >> 4) * 10 + (b & 0x0f)
}

/// Set both sides' times and how each runs.
pub fn set_and_run(left: Hms, left_run: Run, right: Hms, right_run: Run) -> Vec<u8> {
    let side = |(h, m, s): Hms| [h.min(9), bcd(m.min(59)), bcd(s.min(59))];
    let mut data = Vec::with_capacity(7);
    data.extend_from_slice(&side(left));
    data.extend_from_slice(&side(right));
    data.push(left_run.bits() | (right_run.bits() << 2));
    message(CLOCK, cmd::SET_AND_RUN, &data)
}

/// The front buttons, as bits.
pub mod buttons {
    pub const BACK: u8 = 0x01;
    pub const MINUS: u8 = 0x02;
    pub const PLAY_PAUSE: u8 = 0x04;
    pub const PLUS: u8 = 0x08;
    pub const FORWARD: u8 = 0x10;
    pub const FRONT: u8 = 0x1f;
    /// The on/off button (the clock is on when set).
    pub const ON: u8 = 0x20;
    /// The lever: set when its right side is down.
    pub const LEVER_RIGHT_DOWN: u8 = 0x40;
}

/// A message from the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Incoming {
    /// The clock's answer to a command: which, and its value.
    Ack {
        command: u8,
        value: u8,
    },
    /// A clock that woke up.
    Hello,
    /// Both sides' times, and whether the lever's right side is down.
    Time {
        left: Hms,
        right: Hms,
        lever_right_down: bool,
    },
    /// The buttons' state now and before.
    Buttons {
        now: u8,
        before: u8,
    },
    Other {
        command: u8,
    },
}

/// Why a received message was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadMessage {
    TooShort,
    /// Not from the clock.
    WrongSender(u8),
    /// The length byte doesn't match what arrived.
    Length {
        said: u8,
        got: usize,
    },
    Crc,
}

/// Read a received message (starting with the address listened on).
/// `Ok(None)` for the empty messages the clock sends after some packets.
pub fn parse(m: &[u8]) -> Result<Option<Incoming>, BadMessage> {
    if m.len() == 3 && m[1] == 0 && m[2] == 0 || m.len() <= 1 {
        return Ok(None);
    }
    if m.len() < 5 {
        return Err(BadMessage::TooShort);
    }
    if m[1] != CLOCK << 1 {
        return Err(BadMessage::WrongSender(m[1]));
    }
    if usize::from(m[2]) != m.len() {
        return Err(BadMessage::Length { said: m[2], got: m.len() });
    }
    let (body, crc) = m.split_at(m.len() - 1);
    if crc8(body) != crc[0] {
        return Err(BadMessage::Crc);
    }
    let at = |i: usize| m.get(i).copied().unwrap_or(0);
    Ok(Some(match m[3] {
        cmd::ACK => Incoming::Ack { command: at(4), value: at(5) },
        cmd::HELLO => Incoming::Hello,
        cmd::TIME if m.len() >= 21 => Incoming::Time {
            left: (at(5) & 0x0f, from_bcd(at(6)), from_bcd(at(7))),
            right: (at(11) & 0x0f, from_bcd(at(12)), from_bcd(at(13))),
            lever_right_down: at(19) & 1 == 1,
        },
        cmd::BUTTON => Incoming::Buttons { now: at(4), before: at(5) },
        command => Incoming::Other { command },
    }))
}

/// Build a message as the clock would send it to `listen` (for tests and
/// the simulated clock).
pub fn from_clock(listen: u8, command: u8, data: &[u8]) -> Vec<u8> {
    let len = u8::try_from(data.len() + 5).expect("short message");
    let mut m = vec![listen << 1, CLOCK << 1, len, command];
    m.extend_from_slice(data);
    m.push(crc8(&m));
    m
}

/// A time message as the clock sends it.
pub fn time_message(left: Hms, right: Hms, lever_right_down: bool) -> Vec<u8> {
    let mut data = [0u8; 16];
    data[1] = left.0;
    data[2] = bcd(left.1);
    data[3] = bcd(left.2);
    data[7] = right.0;
    data[8] = bcd(right.1);
    data[9] = bcd(right.2);
    data[15] = u8::from(lever_right_down);
    from_clock(BROADCAST, cmd::TIME, &data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The messages DGT's dgtpicom sends, byte for byte.
    #[test]
    fn messages_match_dgts_library() {
        assert_eq!(ping(), [80, 32, 5, 13, 70]);
        assert_eq!(set_central_control(), [16, 32, 5, 15, 72]);
        assert_eq!(mode_25(), [16, 32, 6, 11, 57, 185]);
        assert_eq!(end_display(), [16, 32, 5, 7, 112]);
        let d = display("hello", 2, icons::COLON, 0);
        assert_eq!(d.len(), 21);
        assert_eq!(&d[..4], [16, 32, 21, 6]);
        assert_eq!(&d[4..15], b"hello      ");
        assert_eq!(&d[15..20], [0xff, 2, 3, 8, 0]);
        assert_eq!(crc8(&d[..20]), d[20]);
        let s = set_and_run((0, 5, 0), Run::Down, (1, 23, 45), Run::Stop);
        assert_eq!(&s[..4], [16, 32, 12, 10]);
        assert_eq!(&s[4..11], [0, 0x05, 0x00, 1, 0x23, 0x45, 0x01]);
        assert_eq!(crc8(&s[..11]), s[11]);
    }

    #[test]
    fn reads_the_clocks_messages() {
        let ack = from_clock(PI, cmd::ACK, &[cmd::CHANGE_STATE, 8]);
        assert_eq!(parse(&ack), Ok(Some(Incoming::Ack { command: cmd::CHANGE_STATE, value: 8 })));
        assert_eq!(parse(&from_clock(BROADCAST, cmd::HELLO, &[])), Ok(Some(Incoming::Hello)));
        assert_eq!(
            parse(&time_message((0, 4, 59), (1, 0, 7), true)),
            Ok(Some(Incoming::Time { left: (0, 4, 59), right: (1, 0, 7), lever_right_down: true }))
        );
        let b = from_clock(BROADCAST, cmd::BUTTON, &[buttons::PLUS | buttons::ON, buttons::ON]);
        assert_eq!(parse(&b), Ok(Some(Incoming::Buttons { now: 0x28, before: 0x20 })));
        assert_eq!(parse(&[0, 0, 0]), Ok(None));

        let mut bad = ack.clone();
        bad[5] ^= 1;
        assert_eq!(parse(&bad), Err(BadMessage::Crc));
        assert_eq!(parse(&ack[..6]), Err(BadMessage::Length { said: 7, got: 6 }));
        let mut other = ack;
        other[1] = 20;
        assert_eq!(parse(&other), Err(BadMessage::WrongSender(20)));
    }
}
