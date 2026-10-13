//! A conversation with a Chessnut board, without I/O: real-time mode at
//! the start, the battery now and then, and the board's positions turned
//! into moves once they've been still for a moment (a capture passes
//! through positions on the way).

use std::time::{Duration, Instant};

use chess_core::{Game, GameError, Played};
use chess_dgt::{Follower, Observation, Placement};

use crate::protocol::{self as p, Decoder, Message};

/// What the session reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Game(Observation),
    Battery { percent: u8, charging: bool },
}

#[derive(Debug)]
pub struct Session {
    follower: Follower,
    decoder: Decoder,
    writes: Vec<Vec<u8>>,
    /// The latest position and when it arrived, until it settles.
    pending: Option<(Placement, Instant)>,
    /// The last position that settled.
    settled: Option<Placement>,
    settle: Duration,
    next_battery: Option<Instant>,
}

impl Session {
    pub fn new(follower: Follower) -> Session {
        Session {
            follower,
            decoder: Decoder::default(),
            writes: Vec::new(),
            pending: None,
            settled: None,
            settle: Duration::from_millis(350),
            next_battery: None,
        }
    }

    pub fn follower(&self) -> &Follower {
        &self.follower
    }

    pub fn follower_mut(&mut self) -> &mut Follower {
        &mut self.follower
    }

    pub fn game(&self) -> &Game {
        self.follower.game()
    }

    pub fn play(&mut self, uci: &str) -> Result<Played, GameError> {
        self.follower.play(uci)
    }

    /// Ask for real-time mode (after connecting).
    pub fn start(&mut self, now: Instant) {
        self.writes.push(p::realtime());
        self.writes.push(p::battery());
        self.next_battery = Some(now + Duration::from_secs(30));
    }

    /// Ask again for real-time positions (a board that hasn't answered).
    pub fn restart(&mut self, now: Instant) {
        self.start(now);
    }

    /// Commands to write now, in order.
    pub fn take_writes(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.writes)
    }

    /// Light a move's squares (0 = a1).
    pub fn lights(&mut self, squares: &[u8]) {
        self.writes.push(p::lights(squares, self.follower.orientation()));
    }

    pub fn lights_off(&mut self) {
        self.writes.push(p::lights_off());
    }

    /// A notification's bytes.
    pub fn receive(&mut self, bytes: &[u8], now: Instant) -> Vec<Event> {
        self.decoder.push(bytes);
        let mut events = Vec::new();
        while let Some(m) = self.decoder.next_message() {
            match m {
                // Boards like the GO send the position many times a second
                // even when nothing moves: only a change restarts the wait.
                Message::Position(placement) => match self.pending {
                    Some((waiting, _)) if waiting == placement => {}
                    _ if self.pending.is_none() && self.settled == Some(placement) => {}
                    _ => self.pending = Some((placement, now)),
                },
                Message::Battery { percent, charging } => events.push(Event::Battery { percent, charging }),
            }
        }
        events
    }

    /// Time passing: a position that's been still long enough counts.
    pub fn poll(&mut self, now: Instant) -> Vec<Event> {
        let mut events = Vec::new();
        if let Some((placement, at)) = self.pending
            && now.duration_since(at) >= self.settle
        {
            self.pending = None;
            self.settled = Some(placement);
            let o = self.follower.observe(&placement);
            if o != Observation::Unchanged {
                events.push(Event::Game(o));
            }
        }
        if self.next_battery.is_some_and(|t| now >= t) {
            self.writes.push(p::battery());
            self.next_battery = Some(now + Duration::from_secs(30));
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chess_dgt::STARTING_PLACEMENT;

    fn after(moves: &str) -> Placement {
        let g = Game::from_moves(None, moves).unwrap();
        Placement::from_fen(&g.fen()).unwrap()
    }

    #[test]
    fn follows_a_game_from_settled_positions() {
        let t0 = Instant::now();
        let mut s = Session::new(Follower::new());
        s.start(t0);
        assert_eq!(s.take_writes(), [p::realtime(), p::battery()]);

        let start = Placement::from_fen(STARTING_PLACEMENT).unwrap();
        s.receive(&p::position_bytes(&start), t0);
        assert_eq!(s.poll(t0 + Duration::from_secs(1)), []);

        // e2 lifted (a passing position), then put down on e4.
        let mut lifted = start;
        lifted.0[12] = None;
        let t1 = t0 + Duration::from_secs(2);
        s.receive(&p::position_bytes(&lifted), t1);
        s.receive(&p::position_bytes(&after("e2e4")), t1 + Duration::from_millis(100));
        assert_eq!(s.poll(t1 + Duration::from_millis(200)), [], "not settled yet");
        assert_eq!(
            s.poll(t1 + Duration::from_millis(500)),
            [Event::Game(Observation::Move { uci: "e2e4".into(), san: "e4".into() })]
        );

        // The engine's reply waits for the board, lit.
        s.play("e7e5").unwrap();
        s.lights(&[52, 36]);
        assert_eq!(s.take_writes(), [p::lights(&[52, 36], chess_dgt::Orientation::Normal)]);
        let t2 = t1 + Duration::from_secs(3);
        s.receive(&p::position_bytes(&after("e2e4 e7e5")), t2);
        assert_eq!(s.poll(t2 + Duration::from_secs(1)), [Event::Game(Observation::Caught)]);
    }

    #[test]
    fn a_board_that_repeats_its_position_still_moves() {
        // The Chessnut GO sends the position ten times a second, changed or
        // not (the bytes after it count up).
        let t0 = Instant::now();
        let mut s = Session::new(Follower::new());
        let frames = |p: &Placement, n: u8| {
            let mut b = p::position_bytes(p);
            b[36] = n;
            b
        };
        let start = Placement::from_fen(STARTING_PLACEMENT).unwrap();
        let mut t = t0;
        let mut seen = Vec::new();
        for (i, p) in [start; 10].iter().chain([after("e2e4"); 10].iter()).enumerate() {
            s.receive(&frames(p, u8::try_from(i).unwrap()), t);
            seen.extend(s.poll(t));
            t += Duration::from_millis(100);
        }
        assert_eq!(seen, [Event::Game(Observation::Move { uci: "e2e4".into(), san: "e4".into() })]);
    }
}
