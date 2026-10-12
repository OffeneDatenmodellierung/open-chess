//! A clock on the board's cable (DGT3000, DGT XL): its times and status,
//! and the acknowledgements a DGT3000 sends back for clock commands. Both
//! arrive in the board's clock message (`BWTIME`).

/// One side of the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SideTime {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    /// The flag has fallen and the clock has stopped this side.
    pub flag_fallen: bool,
    /// The clock is in a time-per-move mode.
    pub time_per_move: bool,
    /// The flag has fallen and the clock shows it.
    pub flag_shown: bool,
}

impl SideTime {
    pub fn total_seconds(&self) -> u32 {
        u32::from(self.hours) * 3600 + u32::from(self.minutes) * 60 + u32::from(self.seconds)
    }

    fn parse(hours: u8, minutes: u8, seconds: u8) -> Option<SideTime> {
        let t = SideTime {
            hours: hours & 0x0f,
            minutes: bcd(minutes)?,
            seconds: bcd(seconds)?,
            flag_fallen: hours & 0x10 != 0,
            time_per_move: hours & 0x20 != 0,
            flag_shown: hours & 0x40 != 0,
        };
        (t.hours <= 9 && t.minutes <= 59 && t.seconds <= 59).then_some(t)
    }
}

/// The clock's status byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClockStatus {
    pub running: bool,
    /// The lever's position bit. It flips when a player presses the lever;
    /// which side is down depends on how the clock faces the board.
    pub lever: bool,
    pub battery_low: bool,
    pub right_to_move: bool,
    pub left_to_move: bool,
    /// A clock is on the board's cable and answering.
    pub connected: bool,
}

impl ClockStatus {
    fn parse(b: u8) -> ClockStatus {
        ClockStatus {
            running: b & 0x01 != 0,
            lever: b & 0x02 != 0,
            battery_low: b & 0x04 != 0,
            right_to_move: b & 0x08 != 0,
            left_to_move: b & 0x10 != 0,
            connected: b & 0x20 == 0,
        }
    }
}

/// A reading of the clock: both sides' times and its status. The board
/// reports the right-hand side first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClockTimes {
    pub left: SideTime,
    pub right: SideTime,
    pub status: ClockStatus,
}

/// Which side of the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockSide {
    Left,
    Right,
}

/// A DGT3000 front button. The five buttons are numbered from the left;
/// pressing the outer two together is its own signal (PicoChess uses it to
/// switch the clock off).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockButton {
    Button(u8),
    ZeroAndFour,
}

/// The DGT3000's answer to a clock command, or a button press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockAck {
    /// Which command this answers (the clock subcommand number), or 0x88
    /// for a button, 0x81 for "ready".
    pub code: u8,
    pub data: [u8; 2],
    /// The clock flagged an error.
    pub error: bool,
}

impl ClockAck {
    pub const BUTTON: u8 = 0x88;
    pub const VERSION: u8 = 0x09;
    pub const READY: u8 = 0x81;

    /// The button pressed, when this is a button report.
    pub fn button(&self) -> Option<ClockButton> {
        if self.code != Self::BUTTON {
            return None;
        }
        match self.data[1] {
            b'1' => Some(ClockButton::Button(0)),
            b'4' => Some(ClockButton::Button(1)),
            b'3' => Some(ClockButton::Button(2)),
            b'2' => Some(ClockButton::Button(3)),
            b'5' if self.data[0] == b'E' => Some(ClockButton::ZeroAndFour),
            b'5' => Some(ClockButton::Button(4)),
            _ => None,
        }
    }

    /// The clock's firmware version (major, minor), when this is the
    /// answer to a version request.
    pub fn version(&self) -> Option<(u8, u8)> {
        (self.code == Self::VERSION).then(|| (self.data[0] >> 4, self.data[0] & 0x0f))
    }
}

/// What a clock message carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockReport {
    Times(ClockTimes),
    Ack(ClockAck),
    /// Times that can't be right (minutes over 59 and the like): the
    /// board sends these now and then while the clock is being set.
    Garbled,
}

/// Parse the seven data bytes of a clock message.
pub(crate) fn parse_clock(d: [u8; 7]) -> ClockReport {
    if d[0] & 0x0f == 0x0a || d[3] & 0x0f == 0x0a {
        // An acknowledgement, spread over the bytes with their top bits
        // moved into the hours bytes.
        let a0 = (d[1] & 0x7f) | ((d[3] << 3) & 0x80);
        let a1 = (d[2] & 0x7f) | ((d[3] << 2) & 0x80);
        let a2 = (d[4] & 0x7f) | ((d[0] << 3) & 0x80);
        let a3 = (d[5] & 0x7f) | ((d[0] << 2) & 0x80);
        return ClockReport::Ack(ClockAck { code: a1, data: [a2, a3], error: a0 != 0x10 });
    }
    match (SideTime::parse(d[0], d[1], d[2]), SideTime::parse(d[3], d[4], d[5])) {
        (Some(right), Some(left)) => {
            ClockReport::Times(ClockTimes { left, right, status: ClockStatus::parse(d[6]) })
        }
        _ => ClockReport::Garbled,
    }
}

fn bcd(b: u8) -> Option<u8> {
    let (hi, lo) = (b >> 4, b & 0x0f);
    (hi <= 9 && lo <= 9).then(|| hi * 10 + lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_bcd_right_side_first() {
        // Right 1:23:45 with the flag shown, left 0:05:09; running, left to move.
        let r = parse_clock([0x41, 0x23, 0x45, 0x00, 0x05, 0x09, 0x11]);
        let ClockReport::Times(t) = r else { panic!("{r:?}") };
        assert_eq!((t.right.hours, t.right.minutes, t.right.seconds), (1, 23, 45));
        assert!(t.right.flag_shown && !t.right.flag_fallen);
        assert_eq!(t.left.total_seconds(), 309);
        assert!(t.status.running && t.status.left_to_move && !t.status.right_to_move);
        assert!(t.status.connected);
        let ClockReport::Times(off) = parse_clock([0, 0, 0, 0, 0, 0, 0x20]) else { panic!() };
        assert!(!off.status.connected);
    }

    #[test]
    fn impossible_times_are_garbled() {
        assert_eq!(parse_clock([0x00, 0x6a, 0x00, 0x00, 0x00, 0x00, 0x01]), ClockReport::Garbled);
        assert_eq!(parse_clock([0x00, 0x00, 0x60, 0x00, 0x00, 0x00, 0x01]), ClockReport::Garbled);
    }

    /// Spread an ack over the seven bytes the way the clock does.
    fn ack_bytes(a0: u8, a1: u8, a2: u8, a3: u8) -> [u8; 7] {
        let mut d = [0u8; 7];
        d[0] = 0x0a | ((a2 & 0x80) >> 3) | ((a3 & 0x80) >> 2);
        d[3] = 0x0a | ((a0 & 0x80) >> 3) | ((a1 & 0x80) >> 2);
        d[1] = a0 & 0x7f;
        d[2] = a1 & 0x7f;
        d[4] = a2 & 0x7f;
        d[5] = a3 & 0x7f;
        d
    }

    #[test]
    fn button_acks() {
        let cases = [
            (b'1', b'x', ClockButton::Button(0)),
            (b'4', b'x', ClockButton::Button(1)),
            (b'3', b'x', ClockButton::Button(2)),
            (b'2', b'x', ClockButton::Button(3)),
            (b'5', b'x', ClockButton::Button(4)),
            (b'5', b'E', ClockButton::ZeroAndFour),
        ];
        for (a3, a2, want) in cases {
            let ClockReport::Ack(a) = parse_clock(ack_bytes(0x10, 0x88, a2, a3)) else { panic!() };
            assert!(!a.error);
            assert_eq!(a.code, ClockAck::BUTTON, "the high bit of the code survives the spreading");
            assert_eq!(a.button(), Some(want));
        }
    }

    #[test]
    fn version_and_error_acks() {
        let ClockReport::Ack(a) = parse_clock(ack_bytes(0x10, 0x09, 0x23, 0)) else { panic!() };
        assert_eq!(a.version(), Some((2, 3)));
        assert_eq!(a.button(), None);
        let ClockReport::Ack(bad) = parse_clock(ack_bytes(0x11, 0x0a, 0, 0)) else { panic!() };
        assert!(bad.error);
    }
}
