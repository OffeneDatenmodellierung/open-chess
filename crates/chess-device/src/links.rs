//! The boards, whichever kind, with their I/O: what the play loops drive.

use std::io;
use std::time::{Duration, Instant};

use chess_core::{Game, GameError, Played};
use chess_dgt::{ClockButton, Command, Event, Follower, Session};
use chess_dgtpi::driver::{Clock, Report};

use crate::ble::Transport;
use crate::board::{Board, centaur_button};
use crate::dgtpi;
use crate::log::{self, Source};
use crate::serial::Serial;

/// What a board hands the play loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Event(Event),
    Button(ClockButton),
}

/// A board on a port: the game as it follows it, and the I/O.
pub trait Link: Board {
    /// Write what's queued, read what's arrived (waiting up to a tenth of
    /// a second).
    fn exchange(&mut self, now: Instant) -> io::Result<Vec<Input>>;
}

/// How long to wait for a wireless or USB board's messages each time round.
const WAIT: Duration = Duration::from_millis(100);

macro_rules! board_via_session {
    ($t:ty) => {
        impl Board for $t {
            fn game(&self) -> &Game {
                self.session.game()
            }
            fn awaiting_board(&self) -> bool {
                Board::awaiting_board(&self.session)
            }
            fn play(&mut self, uci: &str) -> Result<Played, GameError> {
                Board::play(&mut self.session, uci)
            }
            fn set_position(&mut self, fen: &str) -> Result<(), GameError> {
                Board::set_position(&mut self.session, fen)
            }
            fn send(&mut self, c: Command) {
                Board::send(&mut self.session, c);
            }
        }
    };
}

/// A DGT board (USB, serial, Bluetooth serial), with a clock on its cable
/// or a DGT Pi's own.
pub struct DgtLink {
    pub session: Session,
    pub port: Serial,
    pub clock: Option<Clock>,
}

impl DgtLink {
    pub fn open(path: &str, clock: Option<Clock>, now: Instant) -> io::Result<DgtLink> {
        let port = Serial::open(path, 9600)?;
        let mut session = Session::new(Follower::new());
        session.start(now);
        Ok(DgtLink { session, port, clock })
    }
}

board_via_session!(DgtLink);

impl Link for DgtLink {
    fn exchange(&mut self, now: Instant) -> io::Result<Vec<Input>> {
        for c in self.session.take_commands() {
            match &self.clock {
                Some(clock) if dgtpi::is_clock(&c) => {
                    if let Some(r) = dgtpi::request(&c) {
                        clock.send(r);
                    }
                }
                _ => self.port.write_all(&c.encode())?,
            }
        }
        let mut buf = [0u8; 512];
        let n = self.port.read(&mut buf)?;
        let mut inputs: Vec<Input> =
            self.session.receive(&buf[..n], now).into_iter().map(Input::Event).collect();
        if let Some(clock) = &self.clock {
            for r in clock.reports() {
                match r {
                    Report::Event(e) => inputs.extend(dgtpi::button(e).map(Input::Button)),
                    Report::Ready => log::line(Source::Clock, "ready"),
                    Report::Failed(why) => log::line(Source::Clock, why),
                }
            }
        }
        self.session.poll(now);
        Ok(inputs)
    }
}

/// A DGT Centaur's board controller.
pub struct CentaurLink {
    pub session: chess_centaur::Session,
    pub port: Serial,
}

impl CentaurLink {
    pub fn open(path: &str, now: Instant) -> io::Result<CentaurLink> {
        let port = Serial::open(path, 1_000_000)?;
        let mut session = chess_centaur::Session::new(chess_centaur::Follower::new());
        session.start(now);
        Ok(CentaurLink { session, port })
    }
}

board_via_session!(CentaurLink);

impl Link for CentaurLink {
    fn exchange(&mut self, now: Instant) -> io::Result<Vec<Input>> {
        self.session.poll(now);
        for w in self.session.take_writes() {
            self.port.write_all(&w)?;
        }
        let mut buf = [0u8; 512];
        let n = self.port.read(&mut buf)?;
        let mut inputs = Vec::new();
        for e in self.session.receive(&buf[..n], Instant::now()) {
            match e {
                chess_centaur::Event::Connected(a) => {
                    inputs.push(Input::Event(Event::Connected { major: a.0, minor: a.1 }));
                }
                chess_centaur::Event::Game(o) => inputs.push(Input::Event(Event::Game(o))),
                chess_centaur::Event::Button(b) => inputs.extend(centaur_button(b).map(Input::Button)),
                chess_centaur::Event::Battery { level, charging } => {
                    if level <= 3 && !charging {
                        log::line(Source::Board, format!("battery low ({level}/20)"));
                    }
                }
            }
        }
        Ok(inputs)
    }
}

/// How a Chessnut board's messages travel: Bluetooth LE or USB.
pub trait Wire: Send {
    fn write(&mut self, data: &[u8]) -> io::Result<()>;
    fn read(&mut self, wait: Duration) -> io::Result<Vec<Vec<u8>>>;
}

struct BleWire(Box<dyn Transport>);

impl Wire for BleWire {
    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.0.write(chess_chessnut::protocol::WRITE, data)
    }

    fn read(&mut self, wait: Duration) -> io::Result<Vec<Vec<u8>>> {
        self.0.notifications(wait)
    }
}

#[cfg(target_os = "linux")]
impl Wire for crate::usb::Hid {
    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        crate::usb::Hid::write(self, data)
    }

    fn read(&mut self, wait: Duration) -> io::Result<Vec<Vec<u8>>> {
        self.reports(wait)
    }
}

/// A Chessnut Air / Air+ / Pro / Go.
pub struct ChessnutLink {
    pub session: chess_chessnut::Session,
    wire: Box<dyn Wire>,
}

impl ChessnutLink {
    /// Over Bluetooth LE.
    pub fn bluetooth(address: &str, random: bool, now: Instant) -> io::Result<ChessnutLink> {
        let t = crate::ble::connect(address, random, chess_chessnut::protocol::READ)?;
        Ok(ChessnutLink::over(Box::new(BleWire(t)), now))
    }

    /// Over USB (Linux: the board's `hidraw` node).
    #[cfg(target_os = "linux")]
    pub fn usb(path: &str, now: Instant) -> io::Result<ChessnutLink> {
        Ok(ChessnutLink::over(Box::new(crate::usb::Hid::open(path)?), now))
    }

    #[cfg(not(target_os = "linux"))]
    pub fn usb(_path: &str, _now: Instant) -> io::Result<ChessnutLink> {
        Err(io::Error::other("Chessnut boards on USB need Linux for now; use Bluetooth"))
    }

    pub fn over(wire: Box<dyn Wire>, now: Instant) -> ChessnutLink {
        let mut session = chess_chessnut::Session::new(Follower::new());
        session.start(now);
        ChessnutLink { session, wire }
    }
}

board_via_session!(ChessnutLink);

impl Link for ChessnutLink {
    fn exchange(&mut self, now: Instant) -> io::Result<Vec<Input>> {
        for w in self.session.take_writes() {
            self.wire.write(&w)?;
        }
        for n in self.wire.read(WAIT)? {
            for e in self.session.receive(&n, now) {
                if let chess_chessnut::Event::Battery { percent, charging: false } = e
                    && percent < 10
                {
                    log::line(Source::Board, format!("battery low ({percent}%)"));
                }
            }
        }
        Ok(self
            .session
            .poll(Instant::now())
            .into_iter()
            .filter_map(|e| match e {
                chess_chessnut::Event::Game(o) => Some(Input::Event(Event::Game(o))),
                chess_chessnut::Event::Battery { .. } => None,
            })
            .collect())
    }
}

/// A DGT Pegasus over Bluetooth LE: the DGT protocol over the Nordic UART
/// service.
pub struct PegasusLink {
    pub session: Session,
    transport: Box<dyn Transport>,
}

impl PegasusLink {
    pub fn bluetooth(address: &str, random: bool, now: Instant) -> io::Result<PegasusLink> {
        let transport = crate::ble::connect(address, random, chess_dgt::pegasus::NOTIFY)?;
        let mut session = chess_dgt::pegasus::session(Follower::new());
        session.start(now);
        Ok(PegasusLink { session, transport })
    }
}

impl Board for PegasusLink {
    fn game(&self) -> &Game {
        self.session.game()
    }
    fn awaiting_board(&self) -> bool {
        Board::awaiting_board(&self.session)
    }
    fn play(&mut self, uci: &str) -> Result<Played, GameError> {
        Board::play(&mut self.session, uci)
    }
    fn set_position(&mut self, fen: &str) -> Result<(), GameError> {
        Board::set_position(&mut self.session, fen)
    }
    fn send(&mut self, c: Command) {
        if let Some(c) = chess_dgt::pegasus::translate(c) {
            self.session.send(c);
        }
    }
}

impl Link for PegasusLink {
    fn exchange(&mut self, now: Instant) -> io::Result<Vec<Input>> {
        for c in self.session.take_commands() {
            if let Some(c) = chess_dgt::pegasus::translate(c) {
                self.transport.write(chess_dgt::pegasus::WRITE, &c.encode())?;
            }
        }
        let mut inputs = Vec::new();
        for n in self.transport.notifications(WAIT)? {
            inputs.extend(self.session.receive(&n, now).into_iter().map(Input::Event));
        }
        self.session.poll(Instant::now());
        Ok(inputs)
    }
}

/// Any of them, to choose at run time.
pub enum AnyLink {
    Dgt(DgtLink),
    Centaur(CentaurLink),
    Chessnut(ChessnutLink),
    Pegasus(PegasusLink),
}

macro_rules! each {
    ($self:ident, $l:ident => $e:expr) => {
        match $self {
            AnyLink::Dgt($l) => $e,
            AnyLink::Centaur($l) => $e,
            AnyLink::Chessnut($l) => $e,
            AnyLink::Pegasus($l) => $e,
        }
    };
}

impl Board for AnyLink {
    fn game(&self) -> &Game {
        each!(self, l => l.game())
    }
    fn awaiting_board(&self) -> bool {
        each!(self, l => l.awaiting_board())
    }
    fn play(&mut self, uci: &str) -> Result<Played, GameError> {
        each!(self, l => l.play(uci))
    }
    fn set_position(&mut self, fen: &str) -> Result<(), GameError> {
        each!(self, l => l.set_position(fen))
    }
    fn send(&mut self, c: Command) {
        each!(self, l => l.send(c));
    }
}

impl Link for AnyLink {
    fn exchange(&mut self, now: Instant) -> io::Result<Vec<Input>> {
        each!(self, l => l.exchange(now))
    }
}
