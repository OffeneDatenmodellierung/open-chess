//! The DGT Pegasus: a Bluetooth LE board that speaks the DGT protocol
//! over the Nordic UART service, with lights of its own.
//!
//! It wants unlocking with a developer key before it reports anything
//! (the key community drivers publish), and board updates
//! ([`Command::SendUpdateBoard`]) rather than the "nice" mode USB boards
//! use. Its lights take a list of squares.

use crate::follow::Follower;
use crate::protocol::Command;
use crate::session::Session;

/// The Nordic UART service, and its characteristics: the board notifies
/// on [`NOTIFY`]; commands are written to [`WRITE`].
pub const SERVICE: &str = "6e400001-b5a3-f393-e0a9-e50e24dcca9e";
pub const WRITE: &str = "6e400002-b5a3-f393-e0a9-e50e24dcca9e";
pub const NOTIFY: &str = "6e400003-b5a3-f393-e0a9-e50e24dcca9e";

const DEVELOPER_KEY: [u8; 6] = [0xbe, 0xf5, 0xae, 0xdd, 0xa9, 0x5f];

/// Unlock the board.
pub fn unlock() -> Command {
    let mut c = vec![0x63, DEVELOPER_KEY.len() as u8 + 1];
    c.extend_from_slice(&DEVELOPER_KEY);
    c.push(0x00);
    Command::Raw(c)
}

/// The board's square numbers start at a8; ours at a1.
fn field(square: u8) -> u8 {
    (7 - square / 8) * 8 + square % 8
}

/// Light squares (0 = a1); none turns them all off.
pub fn lights(squares: &[u8]) -> Command {
    if squares.is_empty() {
        return Command::Raw(vec![0x60, 0x02, 0x00, 0x00]);
    }
    let mut body = vec![0x05, 0x02, 0x00, 0x01];
    body.extend(squares.iter().filter(|&&s| s < 64).map(|&s| field(s)));
    body.push(0x00);
    let mut c = vec![0x60, body.len() as u8];
    c.extend(body);
    Command::Raw(c)
}

/// A session for a Pegasus.
pub fn session(follower: Follower) -> Session {
    Session::new(follower).with_handshake(vec![unlock(), Command::Reset], Command::SendUpdateBoard)
}

/// A board command as the Pegasus understands it: its own lights for the
/// Revelation II's, and nothing for a clock (it has no clock cable).
pub fn translate(c: Command) -> Option<Command> {
    match c {
        Command::Lights { from, to } => Some(lights(&[from.0, to.0])),
        Command::LightsOff => Some(lights(&[])),
        Command::ClockText { .. }
        | Command::RevelationText { .. }
        | Command::ClockEndText
        | Command::ClockBeep { .. }
        | Command::ClockButtons
        | Command::ClockVersion
        | Command::ClockSetAndRun { .. } => None,
        other => Some(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Square;
    use std::time::Instant;

    #[test]
    fn unlocks_then_asks_for_board_updates() {
        let mut s = session(Follower::new());
        s.start(Instant::now());
        let c = s.take_commands();
        assert_eq!(c[0].encode(), [0x63, 0x07, 0xbe, 0xf5, 0xae, 0xdd, 0xa9, 0x5f, 0x00]);
        assert_eq!(c[1], Command::Reset);
        assert_eq!(c[2], Command::SendUpdateBoard);
        assert_eq!(c[3], Command::Version);
    }

    #[test]
    fn lights_count_from_a8() {
        // e2 → the board's 52, e4 → 36.
        let c = translate(Command::Lights { from: Square(12), to: Square(28) }).unwrap();
        assert_eq!(c.encode(), [0x60, 0x07, 0x05, 0x02, 0x00, 0x01, 52, 36, 0x00]);
        assert_eq!(translate(Command::LightsOff).unwrap().encode(), [0x60, 0x02, 0x00, 0x00]);
        assert_eq!(translate(Command::ClockEndText), None);
        assert_eq!(translate(Command::SendBoard), Some(Command::SendBoard));
    }
}
