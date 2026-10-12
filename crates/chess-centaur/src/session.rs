//! A conversation with the Centaur's board controller, without I/O.
//!
//! The controller only answers questions, so the session asks: at start
//! it wakes the controller and learns its address, reads the whole board
//! once, and from then on polls for pieces lifted and put down and for
//! buttons, every few tens of milliseconds, the battery now and then, and
//! the whole board again when nothing is happening (to catch up with
//! anything missed). The application writes [`Session::take_writes`] to
//! the port and hands what it reads to [`Session::receive`].

use std::time::{Duration, Instant};

use chess_dgt::Observation;

use crate::follow::Follower;
use crate::protocol::{self as p, Address, Button, Decoder, Message, Sound};

/// What the session reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The controller answered with its address.
    Connected(Address),
    /// What a piece lifted or put down (or a fresh reading) meant.
    Game(Observation),
    Button(Button),
    Battery {
        level: u8,
        charging: bool,
    },
}

/// How often to ask.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub poll: Duration,
    pub battery: Duration,
    /// Read the whole board after this long without a lift or put-down.
    pub resync: Duration,
    pub ask_address: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            poll: Duration::from_millis(60),
            battery: Duration::from_secs(15),
            resync: Duration::from_secs(5),
            ask_address: Duration::from_millis(250),
        }
    }
}

#[derive(Debug)]
pub struct Session {
    timing: Timing,
    address: Option<Address>,
    decoder: Decoder,
    follower: Follower,
    writes: Vec<Vec<u8>>,
    next_poll: Option<Instant>,
    next_battery: Option<Instant>,
    last_field: Option<Instant>,
    resynced: bool,
}

impl Session {
    pub fn new(follower: Follower) -> Session {
        Session::with_timing(follower, Timing::default())
    }

    pub fn with_timing(follower: Follower, timing: Timing) -> Session {
        Session {
            timing,
            address: None,
            decoder: Decoder::default(),
            follower,
            writes: Vec::new(),
            next_poll: None,
            next_battery: None,
            last_field: None,
            resynced: false,
        }
    }

    pub fn follower(&self) -> &Follower {
        &self.follower
    }

    pub fn follower_mut(&mut self) -> &mut Follower {
        &mut self.follower
    }

    pub fn address(&self) -> Option<Address> {
        self.address
    }

    /// Wake the controller and ask its address.
    pub fn start(&mut self, now: Instant) {
        for w in p::wake() {
            self.writes.push(w.to_vec());
        }
        self.writes.push(p::ask_address());
        self.next_poll = Some(now + self.timing.ask_address);
    }

    /// Bytes to write now, in order.
    pub fn take_writes(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.writes)
    }

    /// Light a move's two squares (0 = a1).
    pub fn lights(&mut self, from: u8, to: u8) {
        if let Some(a) = self.address {
            self.writes.push(p::leds_from_to(a, from, to, 5));
        }
    }

    pub fn lights_off(&mut self) {
        if let Some(a) = self.address {
            self.writes.push(p::leds_off(a));
        }
    }

    pub fn beep(&mut self, sound: Sound) {
        if let Some(a) = self.address {
            self.writes.push(p::beep(a, sound));
        }
    }

    /// Time passing: ask what's due.
    pub fn poll(&mut self, now: Instant) {
        if self.next_poll.is_none_or(|t| now < t) {
            return;
        }
        let Some(a) = self.address else {
            self.writes.push(p::ask_address());
            self.next_poll = Some(now + self.timing.ask_address);
            return;
        };
        self.writes.push(p::poll_fields(a));
        self.writes.push(p::poll_buttons(a));
        if self.next_battery.is_none_or(|t| now >= t) {
            self.writes.push(p::ask_battery(a));
            self.next_battery = Some(now + self.timing.battery);
        }
        let quiet = self.last_field.is_none_or(|t| now.duration_since(t) >= self.timing.resync);
        if quiet && !self.resynced {
            self.writes.push(p::ask_board(a));
            self.resynced = true;
        }
        self.next_poll = Some(now + self.timing.poll);
    }

    /// Bytes from the controller.
    pub fn receive(&mut self, bytes: &[u8], now: Instant) -> Vec<Event> {
        self.decoder.push(bytes);
        let mut events = Vec::new();
        while let Some(packet) = self.decoder.next_packet() {
            let message = p::parse(&packet);
            if self.address.is_none() {
                // The first answer carries the address.
                let a = Address(packet.get(3).copied().unwrap_or(0), packet.get(4).copied().unwrap_or(0));
                self.address = Some(a);
                self.next_poll = Some(now);
                self.next_battery = Some(now);
                self.resynced = false;
                self.last_field = None;
                self.writes.push(p::ask_board(a));
                self.resynced = true;
                events.push(Event::Connected(a));
                continue;
            }
            match message {
                Message::Fields(fields) => {
                    for f in fields {
                        self.last_field = Some(now);
                        self.resynced = false;
                        events.push(Event::Game(self.follower.field(f)));
                    }
                }
                Message::Occupancy(o) => events.push(Event::Game(self.follower.sync(o))),
                Message::Button(b) => events.push(Event::Button(b)),
                Message::Battery { level, charging } => events.push(Event::Battery { level, charging }),
                Message::Idle | Message::Other { .. } => {}
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::follow::START;
    use crate::protocol::Field;

    const A: Address = Address(0x06, 0x50);

    #[test]
    fn learns_the_address_reads_the_board_and_follows_moves() {
        let t0 = Instant::now();
        let mut s = Session::new(Follower::new());
        s.start(t0);
        assert_eq!(s.take_writes(), [vec![0x4d], vec![0x4e], p::ask_address()]);

        // The controller answers with its address.
        let events = s.receive(&p::from_controller(0x87, A, &[0x01]), t0);
        assert_eq!(events, [Event::Connected(A)]);
        assert_eq!(s.take_writes(), [p::ask_board(A)]);
        s.poll(t0);
        let w = s.take_writes();
        assert!(w.contains(&p::poll_fields(A)) && w.contains(&p::poll_buttons(A)), "{w:?}");

        assert_eq!(s.receive(&p::board_reading(A, START), t0), [Event::Game(Observation::Unchanged)]);
        let events = s.receive(&p::fields_reading(A, &[Field::Lifted(12), Field::Placed(28)]), t0);
        assert_eq!(
            events,
            [
                Event::Game(Observation::Waiting),
                Event::Game(Observation::Move { uci: "e2e4".into(), san: "e4".into() })
            ]
        );
        assert_eq!(s.receive(&p::button_reading(A, Some(Button::Tick)), t0), [Event::Button(Button::Tick)]);
        assert_eq!(s.receive(&p::button_reading(A, None), t0), []);

        // Quiet for a while: the whole board is read again, once.
        let later = t0 + Duration::from_secs(10);
        s.poll(later);
        assert!(s.take_writes().contains(&p::ask_board(A)));
        s.poll(later + Duration::from_secs(1));
        assert!(!s.take_writes().contains(&p::ask_board(A)));
    }

    #[test]
    fn keeps_asking_for_the_address_until_it_answers() {
        let t0 = Instant::now();
        let mut s = Session::new(Follower::new());
        s.start(t0);
        s.take_writes();
        s.poll(t0 + Duration::from_millis(100));
        assert!(s.take_writes().is_empty());
        s.poll(t0 + Duration::from_millis(300));
        assert_eq!(s.take_writes(), [p::ask_address()]);
    }
}
