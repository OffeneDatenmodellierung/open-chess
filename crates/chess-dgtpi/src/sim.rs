//! A simulated DGT3000, for tests and for trying the runtime without a
//! DGT Pi: it answers the Pi's messages as the clock does (off until woken,
//! central control, mode 25, texts and times), and only delivers an answer
//! when the Pi is listening on its address, as the I²C hardware would
//! (otherwise the clock sends it again later).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::driver::{Bus, ReceiveError, SendError};
use crate::protocol::{self as p, BROADCAST, CLOCK, Hms, PI, Run, WAKE, cmd};

#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)] // the clock's switches
struct State {
    on: bool,
    central: bool,
    mode_25: bool,
    text: Option<String>,
    times: Option<(Hms, Run, Hms, Run)>,
    buttons: u8,
    listen: u8,
    /// Messages on their way to the Pi: (address, bytes).
    outbox: VecDeque<(u8, Vec<u8>)>,
    /// Messages the Pi sent: (address, command).
    sent: Vec<(u8, u8)>,
    resets: usize,
    /// Answer nothing at all (no clock on the wire).
    absent: bool,
}

/// A simulated clock; clones share it, so a test can watch the clock a
/// driver's thread owns.
#[derive(Debug, Clone, Default)]
pub struct SimClock(Arc<Mutex<State>>);

impl SimClock {
    /// A clock that's switched off.
    pub fn off() -> SimClock {
        SimClock::default()
    }

    /// A clock that's on, running its own program.
    pub fn on() -> SimClock {
        let c = SimClock::default();
        c.state().on = true;
        c
    }

    /// No clock on the wire at all.
    pub fn absent() -> SimClock {
        let c = SimClock::default();
        c.state().absent = true;
        c
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn is_on(&self) -> bool {
        self.state().on
    }

    pub fn in_mode_25(&self) -> bool {
        self.state().mode_25
    }

    /// The text showing, if any.
    pub fn text(&self) -> Option<String> {
        self.state().text.clone()
    }

    /// The times last set, and how each side runs.
    pub fn times(&self) -> Option<(Hms, Run, Hms, Run)> {
        self.state().times
    }

    /// The commands the Pi sent, in order.
    pub fn commands(&self) -> Vec<u8> {
        self.state().sent.iter().map(|&(_, c)| c).collect()
    }

    pub fn resets(&self) -> usize {
        self.state().resets
    }

    /// Press (or release, with 0) front buttons and the on/off button, as
    /// [`p::buttons`] bits.
    pub fn buttons(&self, now: u8) {
        let mut s = self.state();
        let before = s.buttons;
        s.buttons = now;
        s.outbox.push_back((BROADCAST, p::from_clock(BROADCAST, cmd::BUTTON, &[now, before])));
    }

    /// The clock reports its times.
    pub fn report_time(&self, left: Hms, right: Hms, lever_right_down: bool) {
        self.state().outbox.push_back((BROADCAST, p::time_message(left, right, lever_right_down)));
    }
}

fn ack(s: &mut State, to: u8, command: u8, value: u8) {
    s.outbox.push_back((to, p::from_clock(to, cmd::ACK, &[command, value])));
}

impl Bus for SimClock {
    fn send(&mut self, message: &[u8], to: u8, listen: u8) -> Result<(), SendError> {
        let mut s = self.state();
        s.listen = listen;
        // The clock checks a message as if addressed to itself.
        let checks = |m: &[u8]| {
            let mut body = m[..m.len() - 1].to_vec();
            body[0] = CLOCK << 1;
            p::crc8(&body) == m[m.len() - 1]
        };
        if s.absent || message.len() < 5 || !checks(message) {
            return Err(SendError::Silent);
        }
        let command = message[3];
        s.sent.push((to, command));
        if !s.on {
            if to == WAKE && command == cmd::PING {
                s.on = true;
                s.outbox.push_back((BROADCAST, p::from_clock(BROADCAST, cmd::HELLO, &[])));
            }
            return Err(SendError::Silent);
        }
        if to != CLOCK {
            return Err(SendError::Silent);
        }
        match command {
            cmd::SET_CENTRAL_CONTROL => {
                s.central = true;
                ack(&mut s, PI, command, 8);
            }
            cmd::CHANGE_STATE => match message[4] {
                0 => {
                    s.on = false;
                    s.central = false;
                    s.mode_25 = false;
                }
                57 => {
                    s.mode_25 = s.central;
                    let v = if s.central { 8 } else { 0 };
                    ack(&mut s, PI, command, v);
                }
                _ => ack(&mut s, PI, command, 0),
            },
            cmd::END_DISPLAY => {
                if s.text.take().is_some() {
                    ack(&mut s, BROADCAST, command, 0);
                } else {
                    ack(&mut s, PI, command, 5);
                }
            }
            cmd::DISPLAY => {
                if s.text.is_some() {
                    ack(&mut s, BROADCAST, command, 0x23);
                } else {
                    let text = String::from_utf8_lossy(&message[4..15]).trim_end().to_owned();
                    s.text = Some(text);
                    ack(&mut s, BROADCAST, command, 0);
                }
            }
            cmd::SET_AND_RUN => {
                if s.mode_25 {
                    let d = &message[4..11];
                    let side =
                        |h: u8, m: u8, sec: u8| (h, (m >> 4) * 10 + (m & 15), (sec >> 4) * 10 + (sec & 15));
                    let run = |bits: u8| match bits & 3 {
                        1 => Run::Down,
                        2 => Run::Up,
                        _ => Run::Stop,
                    };
                    s.times =
                        Some((side(d[0], d[1], d[2]), run(d[6]), side(d[3], d[4], d[5]), run(d[6] >> 2)));
                    ack(&mut s, PI, command, 8);
                } else {
                    ack(&mut s, PI, command, 0);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn listen(&mut self, address: u8) {
        self.state().listen = address;
    }

    fn receive(&mut self) -> Option<Result<Vec<u8>, ReceiveError>> {
        let mut s = self.state();
        // A message for an address nobody's listening on isn't
        // acknowledged, so the clock sends it again later.
        let listen = s.listen;
        let i = s.outbox.iter().position(|(to, _)| *to == listen)?;
        let (_, m) = s.outbox.remove(i)?;
        s.listen = BROADCAST;
        Some(Ok(m))
    }

    fn reset(&mut self) {
        let mut s = self.state();
        s.resets += 1;
        s.listen = BROADCAST;
    }

    fn pause(&mut self, d: Duration) {
        std::thread::sleep(d.min(Duration::from_micros(50)));
    }
}
