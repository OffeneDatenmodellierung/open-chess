//! A conversation with one board: the handshake, waiting for the board to
//! settle, and keeping the link alive, still without I/O. The transport
//! feeds it bytes and the time, and sends what it asks for.

use std::time::{Duration, Instant};

use crate::board::Placement;
use crate::clock::ClockReport;
use crate::follow::{Follower, Observation};
use crate::protocol::{Command, Decoder, Message};

/// What happened, for the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The board answered the handshake.
    Connected { major: u8, minor: u8 },
    /// A settled reading of the whole board, as the board reported it.
    Reading(Placement),
    /// What the reading meant for the game (moves, takebacks, new games, a
    /// move played elsewhere now made on the board).
    Game(Observation),
    /// The clock on the board's cable: times, or a DGT3000 button or answer.
    Clock(ClockReport),
    /// Anything else the board said (serial number, trademark, battery …).
    Info(Message),
}

/// Timings, with defaults that suit people moving pieces by hand.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// Quiet time after the last square change before the board is read
    /// as a whole. Long enough for a capture (lift, lift, put down), short
    /// enough not to feel slow.
    pub settle: Duration,
    /// Silence after which the link is checked (the board is asked for its
    /// serial number, which every board answers).
    pub keepalive: Duration,
    /// How long to wait for the handshake's answer before asking again.
    pub handshake_retry: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            settle: Duration::from_millis(250),
            keepalive: Duration::from_secs(5),
            handshake_retry: Duration::from_secs(2),
        }
    }
}

/// One board's session.
#[derive(Debug)]
pub struct Session {
    decoder: Decoder,
    follower: Follower,
    timing: Timing,
    outbox: Vec<Command>,
    connected: bool,
    asked_version_at: Option<Instant>,
    changed_at: Option<Instant>,
    heard_at: Option<Instant>,
    pinged_at: Option<Instant>,
    /// Sent first when starting (a Pegasus's unlocking).
    preamble: Vec<Command>,
    /// How the board is asked to report changes.
    updates: Command,
}

impl Session {
    pub fn new(follower: Follower) -> Session {
        Session::with_timing(follower, Timing::default())
    }

    pub fn with_timing(follower: Follower, timing: Timing) -> Session {
        Session {
            decoder: Decoder::new(),
            follower,
            timing,
            outbox: Vec::new(),
            connected: false,
            asked_version_at: None,
            changed_at: None,
            heard_at: None,
            pinged_at: None,
            preamble: Vec::new(),
            updates: Command::SendUpdateNice,
        }
    }

    /// A different opening: commands sent first, and how the board is
    /// asked for updates (the Pegasus wants unlocking and
    /// [`Command::SendUpdateBoard`]).
    #[must_use]
    pub fn with_handshake(mut self, preamble: Vec<Command>, updates: Command) -> Session {
        self.preamble = preamble;
        self.updates = updates;
        self
    }

    pub fn follower(&self) -> &Follower {
        &self.follower
    }

    /// For moves played elsewhere ([`Follower::play`]) and set-up positions.
    pub fn follower_mut(&mut self) -> &mut Follower {
        &mut self.follower
    }

    pub fn connected(&self) -> bool {
        self.connected
    }

    /// Open the conversation: square changes from now on (clock times only
    /// when they change), then the version, whose answer starts things.
    pub fn start(&mut self, now: Instant) {
        self.outbox.extend(self.preamble.iter().cloned());
        self.outbox.push(self.updates.clone());
        self.outbox.push(Command::Version);
        self.asked_version_at = Some(now);
    }

    /// Queue a command of the application's (clock text, lights …).
    pub fn send(&mut self, c: Command) {
        self.outbox.push(c);
    }

    /// Commands to write now, in order.
    pub fn take_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.outbox)
    }

    /// Bytes from the board.
    pub fn receive(&mut self, bytes: &[u8], now: Instant) -> Vec<Event> {
        self.decoder.push(bytes);
        let mut events = Vec::new();
        while let Some(m) = self.decoder.next() {
            self.heard_at = Some(now);
            self.pinged_at = None;
            match m {
                Message::Version { major, minor } => {
                    if !self.connected {
                        self.connected = true;
                        events.push(Event::Connected { major, minor });
                    }
                    self.outbox.push(Command::SendBoard);
                }
                Message::Field { .. } => self.changed_at = Some(now),
                Message::Board(p) => {
                    if !self.connected {
                        // Some boards stream before answering the version.
                        self.connected = true;
                    }
                    events.push(Event::Reading(p));
                    match self.follower.observe(&p) {
                        Observation::Unchanged | Observation::Waiting => {}
                        o => events.push(Event::Game(o)),
                    }
                }
                Message::Clock(c) => events.push(Event::Clock(c)),
                other => events.push(Event::Info(other)),
            }
        }
        events
    }

    /// Let time pass: read the board once it has settled, keep the link
    /// alive, retry the handshake.
    pub fn poll(&mut self, now: Instant) {
        if let Some(t) = self.changed_at
            && now.duration_since(t) >= self.timing.settle
        {
            self.changed_at = None;
            self.outbox.push(Command::SendBoard);
        }
        if !self.connected {
            if self.asked_version_at.is_some_and(|t| now.duration_since(t) >= self.timing.handshake_retry) {
                self.outbox.extend(self.preamble.iter().cloned());
                self.outbox.push(self.updates.clone());
                self.outbox.push(Command::Version);
                self.asked_version_at = Some(now);
            }
            return;
        }
        let quiet_since = self.pinged_at.or(self.heard_at);
        if quiet_since.is_some_and(|t| now.duration_since(t) >= self.timing.keepalive) {
            self.outbox.push(Command::SerialNumber);
            self.pinged_at = Some(now);
        }
    }

    /// When [`Session::poll`] next has something to do.
    pub fn next_deadline(&self) -> Option<Instant> {
        let settle = self.changed_at.map(|t| t + self.timing.settle);
        let other = if self.connected {
            self.pinged_at.or(self.heard_at).map(|t| t + self.timing.keepalive)
        } else {
            self.asked_version_at.map(|t| t + self.timing.handshake_retry)
        };
        match (settle, other) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::STARTING_PLACEMENT;
    use chess_core::Game;

    fn frame(id: u8, data: &[u8]) -> Vec<u8> {
        let len = data.len() + 3;
        let mut v = vec![id | 0x80, (len >> 7) as u8, (len & 0x7f) as u8];
        v.extend_from_slice(data);
        v
    }

    /// A board dump of a position (board order: a8 first).
    fn dump(fen: &str) -> Vec<u8> {
        let p = Placement::from_fen(fen).unwrap();
        let mut codes = [0u8; 64];
        for (i, code) in codes.iter_mut().enumerate() {
            let square = (7 - i / 8) * 8 + i % 8;
            *code = p.0[square].map_or(0, |piece| {
                let base = match piece.kind {
                    crate::board::PieceKind::Pawn => 1,
                    crate::board::PieceKind::Rook => 2,
                    crate::board::PieceKind::Knight => 3,
                    crate::board::PieceKind::Bishop => 4,
                    crate::board::PieceKind::King => 5,
                    crate::board::PieceKind::Queen => 6,
                };
                if piece.white { base } else { base + 6 }
            });
        }
        frame(0x06, &codes)
    }

    fn fen_after(moves: &str) -> String {
        Game::from_moves(None, moves).unwrap().fen()
    }

    #[test]
    fn handshake_then_board_then_moves() {
        let t0 = Instant::now();
        let mut s = Session::new(Follower::new());
        s.start(t0);
        assert_eq!(s.take_commands(), [Command::SendUpdateNice, Command::Version]);

        let ev = s.receive(&frame(0x13, &[1, 9]), t0);
        assert_eq!(ev, [Event::Connected { major: 1, minor: 9 }]);
        assert_eq!(s.take_commands(), [Command::SendBoard]);

        let ev = s.receive(&dump(STARTING_PLACEMENT), t0);
        assert!(matches!(ev[..], [Event::Reading(_)]), "the start position is no news: {ev:?}");

        // e2 lifted, e4 placed: two field updates, then quiet.
        let t1 = t0 + Duration::from_secs(1);
        assert!(s.receive(&frame(0x0e, &[52, 0]), t1).is_empty());
        assert!(s.receive(&frame(0x0e, &[36, 1]), t1 + Duration::from_millis(100)).is_empty());
        s.poll(t1 + Duration::from_millis(200));
        assert!(s.take_commands().is_empty(), "not settled yet");
        s.poll(t1 + Duration::from_millis(400));
        assert_eq!(s.take_commands(), [Command::SendBoard]);

        let ev = s.receive(&dump(&fen_after("e2e4")), t1 + Duration::from_millis(450));
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[1], Event::Game(Observation::Move { uci: "e2e4".into(), san: "e4".into() }));
    }

    #[test]
    fn handshake_is_retried_until_answered() {
        let t0 = Instant::now();
        let mut s = Session::new(Follower::new());
        s.start(t0);
        s.take_commands();
        s.poll(t0 + Duration::from_secs(1));
        assert!(s.take_commands().is_empty());
        assert_eq!(s.next_deadline(), Some(t0 + Duration::from_secs(2)));
        s.poll(t0 + Duration::from_secs(2));
        assert_eq!(s.take_commands(), [Command::SendUpdateNice, Command::Version]);
    }

    #[test]
    fn a_quiet_link_is_checked() {
        let t0 = Instant::now();
        let mut s = Session::new(Follower::new());
        s.start(t0);
        s.receive(&frame(0x13, &[1, 0]), t0);
        s.take_commands();
        s.poll(t0 + Duration::from_secs(4));
        assert!(s.take_commands().is_empty());
        s.poll(t0 + Duration::from_secs(5));
        assert_eq!(s.take_commands(), [Command::SerialNumber]);
        // Not asked again straight away.
        s.poll(t0 + Duration::from_secs(6));
        assert!(s.take_commands().is_empty());
        // The answer counts as hearing from it.
        let ev = s.receive(&frame(0x11, b"12345"), t0 + Duration::from_secs(6));
        assert_eq!(ev, [Event::Info(Message::SerialNumber("12345".into()))]);
        assert_eq!(s.next_deadline(), Some(t0 + Duration::from_secs(11)));
    }

    #[test]
    fn clock_reports_pass_through() {
        let t0 = Instant::now();
        let mut s = Session::new(Follower::new());
        let ev = s.receive(&frame(0x0d, &[0x00, 0x05, 0x00, 0x00, 0x04, 0x59, 0x01]), t0);
        assert!(matches!(ev[..], [Event::Clock(ClockReport::Times(_))]), "{ev:?}");
    }
}
