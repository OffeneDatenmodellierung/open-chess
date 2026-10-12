//! Talking to the DGT3000: each command, the answer it waits for, and the
//! retries, over any [`Bus`] (the Pi's I²C hardware, or a simulated clock).
//!
//! A port of DGT's `dgtpicom` (GPL-3.0): the same messages, answers,
//! timeouts and recovery (wake a clock that's off, take central control,
//! switch to mode 25, reset the I²C hardware when a line sticks), on one
//! thread that also reads the clock's messages, rather than a receive
//! thread and locks.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crate::protocol::{self as p, BROADCAST, CLOCK, Hms, Incoming, PI, Run, WAKE, cmd};

/// Why sending failed, as the I²C hardware sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// Nobody acknowledged the address: the clock is probably off.
    Silent,
    /// Another message was on the bus (the clock talking): try again.
    Collision,
    /// A line stayed low or the hardware stuck: reset it.
    Timeout,
}

/// Why a message from the clock was unreadable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiveError {
    /// The hardware's buffer overflowed or the message was cut short.
    Overrun,
    /// The message took too long to arrive.
    Timeout,
}

/// The I²C link to the clock: the Pi is master when it sends and slave
/// (at an address it chooses) when the clock answers.
pub trait Bus {
    /// Send a whole message (as [`crate::protocol`] builds it, address
    /// byte first) to I²C address `to`, listening on `listen` for the
    /// answer.
    fn send(&mut self, message: &[u8], to: u8, listen: u8) -> Result<(), SendError>;
    /// Listen on this address (0: broadcasts).
    fn listen(&mut self, address: u8);
    /// A message that has arrived, if any, starting with the address it
    /// was received on. Listening goes back to broadcasts after each.
    fn receive(&mut self) -> Option<Result<Vec<u8>, ReceiveError>>;
    /// Reset the I²C hardware.
    fn reset(&mut self);
    /// Wait a little.
    fn pause(&mut self, d: Duration) {
        thread::sleep(d);
    }
}

/// Why a command failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Send(SendError),
    /// The clock didn't answer in time.
    NoAnswer,
    /// The clock answered no (not under central control, not in mode 25,
    /// busy showing a text).
    Refused,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Send(SendError::Silent) => f.write_str("the clock doesn't answer (off, or not a DGT Pi?)"),
            Error::Send(SendError::Collision) => f.write_str("the I²C bus was busy"),
            Error::Send(SendError::Timeout) => f.write_str("an I²C line is stuck (is the jack plugged in?)"),
            Error::NoAnswer => f.write_str("the clock didn't acknowledge"),
            Error::Refused => f.write_str("the clock refused the command"),
        }
    }
}

impl std::error::Error for Error {}

/// Something the clock reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockEvent {
    /// Front buttons pressed, as [`p::buttons`] bits: every button held
    /// down at once (so two pressed together come as both bits).
    Buttons(u8),
    /// The on/off button went down (`true`) or up.
    PowerButton(bool),
    /// The lever moved; the bit is set when its right side is down.
    Lever(bool),
    /// Both sides' times, as the clock shows them.
    Time { left: Hms, right: Hms },
}

/// A DGT3000 on a [`Bus`].
#[derive(Debug)]
pub struct Dgt3000<B: Bus> {
    bus: B,
    ack: Option<(u8, u8)>,
    hello: bool,
    held: u8,
    lever: Option<bool>,
    events: VecDeque<ClockEvent>,
    bad_messages: u32,
}

const ANSWER: Duration = Duration::from_millis(10);
const FAST_ANSWER: Duration = Duration::from_micros(1200);
const TRIES: usize = 3;

impl<B: Bus> Dgt3000<B> {
    pub fn new(bus: B) -> Dgt3000<B> {
        Dgt3000 {
            bus,
            ack: None,
            hello: false,
            held: 0,
            lever: None,
            events: VecDeque::new(),
            bad_messages: 0,
        }
    }

    pub fn bus(&self) -> &B {
        &self.bus
    }

    /// Messages that arrived damaged (noise on the line), so far.
    pub fn bad_messages(&self) -> u32 {
        self.bad_messages
    }

    /// Read what the clock has sent. Call often: the hardware holds only
    /// a few bytes.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Some(r) = self.bus.receive() {
            any = true;
            match r.map(|m| p::parse(&m)) {
                Ok(Ok(Some(m))) => self.take(m),
                Ok(Ok(None)) => {}
                Ok(Err(_)) | Err(_) => self.bad_messages += 1,
            }
        }
        any
    }

    /// What the clock reported since last asked.
    pub fn events(&mut self) -> impl Iterator<Item = ClockEvent> + '_ {
        self.events.drain(..)
    }

    fn take(&mut self, m: Incoming) {
        match m {
            Incoming::Ack { command, value } => self.ack = Some((command, value)),
            Incoming::Hello => self.hello = true,
            Incoming::Time { left, right, lever_right_down } => {
                self.lever = Some(lever_right_down);
                self.events.push_back(ClockEvent::Time { left, right });
            }
            Incoming::Buttons { now, before } => {
                if now & p::buttons::FRONT != 0 {
                    self.held |= now & p::buttons::FRONT;
                    self.events.push_back(ClockEvent::Buttons(self.held));
                } else {
                    self.held = 0;
                }
                if (now ^ before) & p::buttons::ON != 0 {
                    self.events.push_back(ClockEvent::PowerButton(now & p::buttons::ON != 0));
                }
                if (now ^ before) & p::buttons::LEVER_RIGHT_DOWN != 0 {
                    let right_down = now & p::buttons::LEVER_RIGHT_DOWN != 0;
                    self.lever = Some(right_down);
                    self.events.push_back(ClockEvent::Lever(right_down));
                }
            }
            Incoming::Other { .. } => {}
        }
    }

    fn send(&mut self, message: &[u8], to: u8, listen: u8) -> Result<(), Error> {
        self.ack = None;
        self.hello = false;
        self.bus.send(message, to, listen).map_err(Error::Send)
    }

    /// Wait for the clock's answer to `command` on `listen`: its value.
    fn answer(&mut self, listen: u8, command: u8, within: Duration) -> Option<u8> {
        let until = Instant::now() + within;
        self.bus.listen(listen);
        loop {
            if self.poll() {
                self.bus.listen(listen);
            }
            if let Some((c, v)) = self.ack
                && c == command
            {
                self.bus.listen(BROADCAST);
                return Some(v);
            }
            if Instant::now() >= until {
                self.bus.listen(BROADCAST);
                return None;
            }
            self.bus.pause(Duration::from_micros(100));
        }
    }

    fn wake(&mut self) -> Result<(), Error> {
        // A wake message is never acknowledged; the clock says hello.
        match self.send(&p::ping(), WAKE, BROADCAST) {
            Ok(()) => return Err(Error::Refused),
            Err(Error::Send(SendError::Silent)) => {}
            Err(e) => return Err(e),
        }
        let until = Instant::now() + ANSWER;
        while Instant::now() < until {
            self.poll();
            if self.hello {
                return Ok(());
            }
            self.bus.pause(Duration::from_micros(100));
        }
        Err(Error::NoAnswer)
    }

    fn set_central_control(&mut self) -> Result<(), Error> {
        self.send(&p::set_central_control(), CLOCK, PI)?;
        match self.answer(PI, cmd::SET_CENTRAL_CONTROL, ANSWER) {
            Some(v) if v & 8 == 8 => Ok(()),
            Some(_) => Err(Error::Refused),
            None => Err(Error::NoAnswer),
        }
    }

    fn mode_25(&mut self) -> Result<(), Error> {
        self.send(&p::mode_25(), CLOCK, PI)?;
        match self.answer(PI, cmd::CHANGE_STATE, ANSWER) {
            Some(8) => Ok(()),
            Some(_) => Err(Error::Refused),
            None => Err(Error::NoAnswer),
        }
    }

    fn end_display_once(&mut self) -> Result<(), Error> {
        self.send(&p::end_display(), CLOCK, PI)?;
        // A quick answer: there was no text.
        if let Some(v) = self.answer(PI, cmd::END_DISPLAY, FAST_ANSWER) {
            return if v & 7 == 5 { Ok(()) } else { Err(Error::Refused) };
        }
        // Otherwise a broadcast once the text has gone.
        match self.answer(BROADCAST, cmd::END_DISPLAY, ANSWER) {
            Some(v) if v.trailing_zeros() >= 3 => Ok(()), // the low three bits clear
            Some(_) => Err(Error::Refused),
            None => Err(Error::NoAnswer),
        }
    }

    fn display_once(&mut self, message: &[u8]) -> Result<(), Error> {
        self.send(message, CLOCK, BROADCAST)?;
        match self.answer(BROADCAST, cmd::DISPLAY, ANSWER) {
            Some(v) if v & 0xf3 == 0x23 => Err(Error::Refused),
            Some(_) => Ok(()),
            None => Err(Error::NoAnswer),
        }
    }

    fn set_and_run_once(&mut self, message: &[u8]) -> Result<(), Error> {
        self.send(message, CLOCK, PI)?;
        match self.answer(PI, cmd::SET_AND_RUN, ANSWER) {
            Some(8) => Ok(()),
            Some(_) => Err(Error::Refused),
            None => Err(Error::NoAnswer),
        }
    }

    fn tries(&mut self, mut f: impl FnMut(&mut Self) -> Result<(), Error>) -> Result<(), Error> {
        let mut last = Error::NoAnswer;
        for _ in 0..TRIES {
            match f(self) {
                Ok(()) => return Ok(()),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// Get the clock ready: switch it on if it's off, take central
    /// control, mode 25. Run first, and again if commands fail.
    pub fn configure(&mut self) -> Result<(), Error> {
        let (mut wakes, mut controls, mut resets, mut collisions) = (0, 0, 0, 0);
        loop {
            match self.mode_25() {
                Ok(()) => {
                    self.bus.pause(Duration::from_millis(5));
                    return Ok(());
                }
                Err(Error::Refused | Error::NoAnswer) => {
                    controls += 1;
                    if controls > TRIES {
                        return Err(Error::Refused);
                    }
                    self.bus.pause(Duration::from_millis(10));
                    // Its result shows in the next mode 25.
                    let _ = self.set_central_control();
                }
                Err(Error::Send(SendError::Timeout)) => {
                    resets += 1;
                    if resets > 1 {
                        return Err(Error::Send(SendError::Timeout));
                    }
                    self.bus.reset();
                }
                Err(Error::Send(SendError::Collision)) => {
                    collisions += 1;
                    if collisions > 20 {
                        return Err(Error::Send(SendError::Collision));
                    }
                }
                Err(Error::Send(SendError::Silent)) => {
                    wakes += 1;
                    if wakes > TRIES {
                        return Err(Error::Send(SendError::Silent));
                    }
                    let _ = self.wake();
                }
            }
        }
    }

    /// Show a text (up to eleven characters) with a beep of `beep` × 62.5
    /// ms and icons ([`p::icons`]) on each side.
    pub fn set_text(&mut self, text: &str, beep: u8, left_icons: u8, right_icons: u8) -> Result<(), Error> {
        let message = p::display(text, beep, left_icons, right_icons);
        self.tries(Self::end_display_once)?;
        self.tries(|c| c.display_once(&message))
    }

    /// Back to the times.
    pub fn end_text(&mut self) -> Result<(), Error> {
        self.tries(Self::end_display_once)
    }

    /// Set both sides' times and how each runs.
    pub fn set_and_run(&mut self, left: Hms, left_run: Run, right: Hms, right_run: Run) -> Result<(), Error> {
        let message = p::set_and_run(left, left_run, right, right_run);
        self.tries(|c| c.set_and_run_once(&message))
    }

    /// Switch the clock off.
    pub fn off(&mut self) -> Result<(), Error> {
        self.send(&p::off(), CLOCK, BROADCAST)
    }
}

/// A command for the clock's thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Text { text: String, beep: u8, left_icons: u8, right_icons: u8 },
    EndText,
    SetAndRun { left: Hms, left_run: Run, right: Hms, right_run: Run },
    Off,
}

/// What the clock's thread reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    Event(ClockEvent),
    /// The clock is ready (after starting, or recovering).
    Ready,
    /// A command or getting ready failed.
    Failed(String),
}

/// The clock on a thread of its own, which keeps reading its messages
/// (the hardware holds only a few bytes) and carries out requests.
#[derive(Debug)]
pub struct Clock {
    tx: Sender<Request>,
    rx: Receiver<Report>,
}

impl Clock {
    pub fn spawn<B: Bus + Send + 'static>(bus: B) -> std::io::Result<Clock> {
        let (tx, requests) = mpsc::channel::<Request>();
        let (reports, rx) = mpsc::channel::<Report>();
        thread::Builder::new()
            .name("dgt3000".into())
            .spawn(move || run(Dgt3000::new(bus), &requests, &reports))?;
        Ok(Clock { tx, rx })
    }

    pub fn send(&self, r: Request) {
        let _ = self.tx.send(r);
    }

    /// Reports since last asked.
    pub fn reports(&self) -> impl Iterator<Item = Report> + '_ {
        self.rx.try_iter()
    }
}

fn run<B: Bus>(mut clock: Dgt3000<B>, requests: &Receiver<Request>, reports: &Sender<Report>) {
    let mut ready = report_ready(&mut clock, reports);
    loop {
        clock.poll();
        for e in clock.events() {
            if reports.send(Report::Event(e)).is_err() {
                return;
            }
        }
        match requests.try_recv() {
            Ok(r) => {
                if !ready {
                    ready = report_ready(&mut clock, reports);
                }
                let mut result = carry_out(&mut clock, &r);
                if result.is_err() && report_ready(&mut clock, reports) {
                    result = carry_out(&mut clock, &r);
                }
                if let Err(e) = result {
                    ready = false;
                    let _ = reports.send(Report::Failed(format!("{r:?}: {e}")));
                }
            }
            Err(TryRecvError::Empty) => clock.bus.pause(Duration::from_micros(400)),
            Err(TryRecvError::Disconnected) => return,
        }
    }
}

fn report_ready<B: Bus>(clock: &mut Dgt3000<B>, reports: &Sender<Report>) -> bool {
    match clock.configure() {
        Ok(()) => {
            let _ = reports.send(Report::Ready);
            true
        }
        Err(e) => {
            let _ = reports.send(Report::Failed(format!("getting the clock ready: {e}")));
            false
        }
    }
}

fn carry_out<B: Bus>(clock: &mut Dgt3000<B>, r: &Request) -> Result<(), Error> {
    match r {
        Request::Text { text, beep, left_icons, right_icons } => {
            clock.set_text(text, *beep, *left_icons, *right_icons)
        }
        Request::EndText => clock.end_text(),
        Request::SetAndRun { left, left_run, right, right_run } => {
            clock.set_and_run(*left, *left_run, *right, *right_run)
        }
        Request::Off => clock.off(),
    }
}
