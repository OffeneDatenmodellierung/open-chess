//! The game on the board against an engine, without I/O.
//!
//! The controller reacts to the board's events: when it's the engine's
//! turn it asks for a move ([`Think`]); when the move comes back it plays
//! it in the game, shows it on the clock and lights its squares
//! (Revelation II), and waits for the player to make it on the board. It
//! uses the opening book first when there is one, and shows the result
//! when the game ends.
//!
//! With a time control it keeps the game's clock itself (the DGT3000 shows
//! it, set at every turn), charges each side from its turn starting to its
//! move, adds the increment, and ends the game when a flag falls. Clocks
//! start with the first move, as on Lichess.
//!
//! The DGT3000's buttons, left to right: show the engine's last move
//! again, level down, switch sides (the engine plays the side to move),
//! level up, pause or resume the clock.

use std::time::Instant;

use chess_analysis::book::Book;
use chess_core::{Clock, Game, Side, Status};
use chess_dgt::{ClockButton, ClockReport, Command, Event, Observation, RunMode, Session, Square};

/// A request for the engine's move in a position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Think {
    /// The game's ply when asked: a reply for another ply is stale.
    pub ply: usize,
    pub initial_fen: Option<String>,
    pub moves: Vec<String>,
    /// open-chess's engine level (1–8) at the time of asking.
    pub level: u8,
    /// Both clocks when the game has a time control.
    pub clock: Option<ClockState>,
}

/// The clocks for an engine playing to a time control (milliseconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockState {
    pub white_ms: i64,
    pub black_ms: i64,
    pub increment_ms: i64,
}

/// A time control: minutes for the game plus seconds per move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeControl {
    pub minutes: u32,
    pub increment_secs: u32,
}

impl std::fmt::Display for TimeControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}+{}", self.minutes, self.increment_secs)
    }
}

impl std::str::FromStr for TimeControl {
    type Err = String;

    /// `"5+3"` (five minutes, three seconds a move) or `"10"`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (m, i) = s.split_once('+').unwrap_or((s, "0"));
        let minutes: u32 = m.trim().parse().map_err(|_| format!("{s}: minutes+increment, e.g. 5+3"))?;
        let increment_secs: u32 =
            i.trim().parse().map_err(|_| format!("{s}: minutes+increment, e.g. 5+3"))?;
        if minutes == 0 || minutes > 600 || increment_secs > 600 {
            return Err(format!("{s}: 1 to 600 minutes, increment up to 600 seconds"));
        }
        Ok(TimeControl { minutes, increment_secs })
    }
}

/// How the game is set up.
#[derive(Debug, Clone)]
pub struct Setup {
    /// The side the person plays.
    pub player: Side,
    /// open-chess's engine level, 1–8 (the clock's buttons change it).
    pub level: u8,
    pub book: Option<Book>,
    pub time: Option<TimeControl>,
    /// White's time is on the clock's left side (else the right).
    pub white_on_left: bool,
    /// For choosing among book moves.
    pub seed: u64,
}

impl Default for Setup {
    fn default() -> Self {
        Setup { player: Side::White, level: 4, book: None, time: None, white_on_left: true, seed: 1 }
    }
}

#[derive(Debug, Clone, Copy)]
struct Running {
    side: Side,
    since: Instant,
}

/// One board game against an engine.
#[derive(Debug)]
pub struct Controller {
    setup: Setup,
    player: Side,
    level: u8,
    rng: u64,
    thinking: Option<usize>,
    ended: bool,
    clock: Option<Clock>,
    running: Option<Running>,
    paused: bool,
    last_engine_san: Option<String>,
    flagged: Option<Side>,
    games: u32,
}

impl Controller {
    pub fn new(setup: Setup) -> Controller {
        let clock = setup.time.map(|t| Clock::new(t.minutes * 60, t.increment_secs));
        Controller {
            player: setup.player,
            level: setup.level.clamp(1, 8),
            rng: setup.seed | 1,
            thinking: None,
            ended: false,
            clock,
            running: None,
            paused: false,
            last_engine_san: None,
            flagged: None,
            games: 0,
            setup,
        }
    }

    pub fn player(&self) -> Side {
        self.player
    }

    pub fn level(&self) -> u8 {
        self.level
    }

    /// Whether a move has been asked for and not answered yet.
    pub fn thinking(&self) -> bool {
        self.thinking.is_some()
    }

    /// How many games have started (a new one each time the starting
    /// position is set up).
    pub fn games(&self) -> u32 {
        self.games
    }

    /// The side whose flag fell, ending the game.
    pub fn flagged(&self) -> Option<Side> {
        self.flagged
    }

    /// The side whose clock is running (not while paused).
    pub fn running(&self) -> Option<Side> {
        self.running.filter(|_| !self.paused).map(|r| r.side)
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    /// The time control new games are played to.
    pub fn time_control(&self) -> Option<TimeControl> {
        self.setup.time
    }

    /// Set the engine's level (1–8), from the next move it's asked for.
    pub fn set_level(&mut self, level: u8, s: &mut Session) {
        self.level = level.clamp(1, 8);
        say(s, &format!("level {}", self.level));
    }

    /// Play the other side: the engine takes over the side to move when
    /// it's the person's turn. Not while the engine thinks or its move is
    /// waiting to be made on the board, or once the game is over.
    pub fn set_player(
        &mut self,
        side: Side,
        s: &mut Session,
        now: Instant,
    ) -> Result<Option<Think>, &'static str> {
        if side == self.player {
            return Ok(None);
        }
        if self.ended {
            return Err("the game is over: set up the pieces for a new one");
        }
        if self.thinking.is_some() || s.follower().awaiting_board() {
            return Err("wait for the engine's move to be made on the board");
        }
        self.player = side;
        say(s, if self.player == Side::White { "you: wht" } else { "you: blk" });
        Ok(self.engine_turn(s, now))
    }

    /// Change the time control (`None`: untimed). It applies now if the
    /// game hasn't started, otherwise from the next game. Whether it
    /// applied now.
    pub fn set_time(&mut self, time: Option<TimeControl>, s: &mut Session, now: Instant) -> bool {
        self.setup.time = time;
        if s.follower().game().ply() == 0 && !self.ended {
            self.reset_clock(s, now);
            if time.is_none() {
                s.send(Command::ClockEndText);
            }
            return true;
        }
        false
    }

    /// Milliseconds left for a side, as of `now`, when there's a clock.
    pub fn remaining(&self, side: Side, now: Instant) -> Option<i64> {
        let c = self.clock?;
        let used =
            self.running.filter(|r| r.side == side && !self.paused).map_or(0, |r| elapsed(r.since, now));
        Some(c.remaining_ms[side.index()] - used)
    }

    /// React to the board. May ask for the engine's move.
    pub fn on_event(&mut self, event: &Event, s: &mut Session, now: Instant) -> Option<Think> {
        match event {
            Event::Connected { .. } => {
                say(s, "ready");
                self.show_clock(s, now);
                self.engine_turn(s, now)
            }
            Event::Game(Observation::NewGame) => {
                self.games += 1;
                self.thinking = None;
                self.ended = false;
                self.flagged = None;
                self.last_engine_san = None;
                self.reset_clock(s, now);
                s.send(Command::LightsOff);
                say(s, "new game");
                self.engine_turn(s, now)
            }
            Event::Game(Observation::Move { san, .. }) => {
                // The player's move: their clock stops, the engine's starts.
                if self.charge(s, now) {
                    return None;
                }
                say(s, san);
                self.start(s.follower().game().turn(), s, now);
                self.engine_turn(s, now)
            }
            Event::Game(Observation::Takeback { .. }) => {
                self.thinking = None;
                self.ended = false;
                self.flagged = None;
                self.stop(now);
                s.send(Command::LightsOff);
                say(s, "takeback");
                self.show_clock(s, now);
                self.engine_turn(s, now)
            }
            Event::Game(Observation::Caught) => {
                s.send(Command::LightsOff);
                s.send(Command::ClockEndText);
                self.check_end(s);
                None
            }
            Event::Clock(ClockReport::Ack(a)) => match a.button() {
                Some(b) => self.on_button(b, s, now),
                None => None,
            },
            _ => None,
        }
    }

    /// The engine's answer. Stale answers (the game moved on, a takeback,
    /// a new game) are dropped.
    pub fn on_reply(
        &mut self,
        ply: usize,
        uci: Option<&str>,
        s: &mut Session,
        now: Instant,
    ) -> Option<Think> {
        if self.thinking != Some(ply) || s.follower().game().ply() != ply || self.ended {
            return None;
        }
        self.thinking = None;
        match uci {
            Some(uci) => {
                if self.charge(s, now) {
                    return None;
                }
                self.play_engine_move(uci, s);
                self.start(s.follower().game().turn(), s, now);
            }
            None => say(s, "no move"),
        }
        None
    }

    /// Let time pass: a flag may fall.
    pub fn tick(&mut self, s: &mut Session, now: Instant) {
        let Some(r) = self.running else { return };
        if self.ended || self.paused {
            return;
        }
        if self.remaining(r.side, now).is_some_and(|ms| ms <= 0) {
            self.flag(r.side, s);
        }
    }

    fn on_button(&mut self, b: ClockButton, s: &mut Session, now: Instant) -> Option<Think> {
        match b {
            ClockButton::Button(0) => {
                let text = self.last_engine_san.clone().unwrap_or_else(|| "no move".into());
                say(s, &text);
                None
            }
            ClockButton::Button(1) => {
                self.set_level(self.level.saturating_sub(1), s);
                None
            }
            ClockButton::Button(3) => {
                self.set_level(self.level + 1, s);
                None
            }
            // Switch sides: the engine takes over the side to move.
            ClockButton::Button(2) => self.set_player(self.player.opposite(), s, now).ok().flatten(),
            ClockButton::Button(4) => {
                if self.clock.is_some() && !self.ended {
                    self.toggle_pause(s, now);
                }
                None
            }
            _ => None,
        }
    }

    fn engine_turn(&mut self, s: &mut Session, now: Instant) -> Option<Think> {
        if self.check_end(s) || self.thinking.is_some() || s.follower().awaiting_board() {
            return None;
        }
        let game = s.follower().game();
        if game.turn() == self.player {
            return None;
        }
        if let Some(book) = &self.setup.book {
            self.rng = splitmix(self.rng);
            if let Some(uci) = book.pick(game.board(), (self.rng >> 32) as u32) {
                self.play_engine_move(&uci, s);
                self.start(s.follower().game().turn(), s, now);
                return None;
            }
        }
        let think = Think {
            ply: game.ply(),
            initial_fen: game.initial_fen().map(str::to_owned),
            moves: game.uci_moves(),
            level: self.level,
            clock: self.clock.map(|c| ClockState {
                white_ms: self.remaining(Side::White, now).unwrap_or(c.remaining_ms[0]).max(1),
                black_ms: self.remaining(Side::Black, now).unwrap_or(c.remaining_ms[1]).max(1),
                increment_ms: c.increment_ms,
            }),
        };
        self.thinking = Some(think.ply);
        if self.clock.is_none() {
            say(s, "thinking");
        }
        Some(think)
    }

    fn play_engine_move(&mut self, uci: &str, s: &mut Session) {
        match s.follower_mut().play(uci) {
            Ok(played) => {
                s.send(Command::ClockText { text: played.san.clone(), beep: 2 });
                if let (Some(from), Some(to)) = (Square::from_name(&uci[0..2]), Square::from_name(&uci[2..4]))
                {
                    s.send(Command::Lights { from, to });
                }
                self.last_engine_san = Some(played.san);
            }
            Err(e) => say(s, &format!("bad {e}")),
        }
    }

    /// Show the result once, when the game is over. Whether it is.
    fn check_end(&mut self, s: &mut Session) -> bool {
        if self.ended {
            return true;
        }
        let Some(outcome) = s.follower().game().outcome() else { return false };
        if !s.follower().awaiting_board() {
            self.ended = true;
            self.running = None;
            s.send(Command::ClockText { text: result_text(outcome.status, outcome.winner), beep: 4 });
        }
        true
    }

    // ── the clock ──

    /// A move has been made: charge the side that was running, add the
    /// increment. True when its flag had already fallen (the game is over).
    fn charge(&mut self, s: &mut Session, now: Instant) -> bool {
        let (Some(c), Some(r)) = (self.clock.as_mut(), self.running.take()) else { return false };
        if !self.paused {
            c.remaining_ms[r.side.index()] -= elapsed(r.since, now);
        }
        if c.remaining_ms[r.side.index()] <= 0 {
            self.flag(r.side, s);
            return true;
        }
        c.remaining_ms[r.side.index()] += c.increment_ms;
        false
    }

    /// Start a side's clock (clocks only run once the game has a move).
    fn start(&mut self, side: Side, s: &mut Session, now: Instant) {
        if self.clock.is_none() || self.ended || s.follower().game().ply() == 0 {
            return;
        }
        self.running = Some(Running { side, since: now });
        self.show_clock(s, now);
    }

    /// Stop without charging an increment (a takeback, the game ending).
    fn stop(&mut self, now: Instant) {
        if let (Some(c), Some(r)) = (self.clock.as_mut(), self.running.take())
            && !self.paused
        {
            c.remaining_ms[r.side.index()] -= elapsed(r.since, now);
        }
        self.paused = false;
    }

    fn toggle_pause(&mut self, s: &mut Session, now: Instant) {
        let Some(r) = self.running else { return };
        if self.paused {
            self.paused = false;
            self.running = Some(Running { side: r.side, since: now });
        } else {
            if let Some(c) = self.clock.as_mut() {
                c.remaining_ms[r.side.index()] -= elapsed(r.since, now);
            }
            self.paused = true;
        }
        say(s, if self.paused { "paused" } else { "resumed" });
        self.show_clock(s, now);
    }

    fn reset_clock(&mut self, s: &mut Session, now: Instant) {
        self.clock = self.setup.time.map(|t| Clock::new(t.minutes * 60, t.increment_secs));
        self.running = None;
        self.paused = false;
        self.show_clock(s, now);
    }

    fn flag(&mut self, side: Side, s: &mut Session) {
        if let Some(c) = self.clock.as_mut() {
            c.remaining_ms[side.index()] = 0;
        }
        self.running = None;
        self.ended = true;
        self.thinking = None;
        self.flagged = Some(side);
        let text = if side == Side::White { "flag 0-1" } else { "flag 1-0" };
        s.send(Command::ClockText { text: text.into(), beep: 4 });
    }

    /// Set the DGT3000 to the clock's times, running the side whose turn it is.
    fn show_clock(&self, s: &mut Session, now: Instant) {
        let (Some(w), Some(b)) = (self.remaining(Side::White, now), self.remaining(Side::Black, now)) else {
            return;
        };
        let (left, right) = if self.setup.white_on_left { (w, b) } else { (b, w) };
        let mode = match self.running {
            Some(r) if !self.paused => {
                if (r.side == Side::White) == self.setup.white_on_left {
                    RunMode::LeftRunning
                } else {
                    RunMode::RightRunning
                }
            }
            _ => RunMode::Paused,
        };
        s.send(Command::ClockSetAndRun { left: hms(left), right: hms(right), mode, lever_toggles: false });
    }
}

fn elapsed(since: Instant, now: Instant) -> i64 {
    i64::try_from(now.saturating_duration_since(since).as_millis()).unwrap_or(i64::MAX)
}

/// Milliseconds as the clock's (hours, minutes, seconds), rounded up so a
/// side isn't shown 0:00 while it still has time.
fn hms(ms: i64) -> (u8, u8, u8) {
    let secs = u64::try_from((ms.max(0) + 999) / 1000).unwrap_or(0);
    let h = (secs / 3600).min(9);
    let m = (secs / 60) % 60;
    let s = secs % 60;
    (h as u8, m as u8, s as u8)
}

fn result_text(status: Status, winner: Option<Side>) -> String {
    let score = match winner {
        Some(Side::White) => "1-0",
        Some(Side::Black) => "0-1",
        None => "1/2",
    };
    match status {
        Status::Mate => format!("mate {score}"),
        Status::Stalemate => "stalemat".into(),
        _ => format!("draw {score}"),
    }
}

/// Up to eight characters on a DGT3000.
fn say(s: &mut Session, text: &str) {
    s.send(Command::ClockText { text: text.chars().take(8).collect(), beep: 0 });
}

fn splitmix(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The game as the controller sees it (for logging and the app).
pub fn describe(game: &Game) -> String {
    game.sans().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chess_dgt::{Follower, Placement};
    use std::time::Instant;

    fn frame(id: u8, data: &[u8]) -> Vec<u8> {
        let len = data.len() + 3;
        let mut v = vec![id | 0x80, (len >> 7) as u8, (len & 0x7f) as u8];
        v.extend_from_slice(data);
        v
    }

    fn dump(fen: &str) -> Vec<u8> {
        let p = Placement::from_fen(fen).unwrap();
        let mut codes = [0u8; 64];
        for (i, code) in codes.iter_mut().enumerate() {
            let sq = (7 - i / 8) * 8 + i % 8;
            *code = p.0[sq].map_or(0, |piece| {
                let base = match piece.kind {
                    chess_dgt::PieceKind::Pawn => 1,
                    chess_dgt::PieceKind::Rook => 2,
                    chess_dgt::PieceKind::Knight => 3,
                    chess_dgt::PieceKind::Bishop => 4,
                    chess_dgt::PieceKind::King => 5,
                    chess_dgt::PieceKind::Queen => 6,
                };
                if piece.white { base } else { base + 6 }
            });
        }
        frame(0x06, &codes)
    }

    fn fen(moves: &str) -> String {
        Game::from_moves(None, moves).unwrap().fen()
    }

    /// Feed a settled board position; collect any request to think.
    fn show(c: &mut Controller, s: &mut Session, moves: &str) -> Option<Think> {
        let mut think = None;
        for e in s.receive(&dump(&fen(moves)), Instant::now()) {
            if let Some(t) = c.on_event(&e, s, Instant::now()) {
                think = Some(t);
            }
        }
        think
    }

    fn texts(s: &mut Session) -> Vec<String> {
        s.take_commands()
            .into_iter()
            .filter_map(|c| match c {
                Command::ClockText { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn plays_the_engine_and_waits_for_the_board() {
        let mut s = Session::new(Follower::new());
        let mut c = Controller::new(Setup::default());
        assert_eq!(show(&mut c, &mut s, ""), None, "the player has White");

        let t = show(&mut c, &mut s, "e2e4").expect("the engine's turn");
        assert_eq!((t.ply, t.moves.clone()), (1, vec!["e2e4".to_string()]));
        assert!(c.thinking());
        assert_eq!(texts(&mut s), ["e4", "thinking"]);

        c.on_reply(1, Some("e7e5"), &mut s, Instant::now());
        let cmds = s.take_commands();
        assert!(cmds.contains(&Command::ClockText { text: "e5".into(), beep: 2 }), "{cmds:?}");
        assert!(cmds.iter().any(|c| matches!(c, Command::Lights { .. })));
        assert!(s.follower().awaiting_board());

        // The board still shows 1.e4: nothing happens until the move is made.
        assert_eq!(show(&mut c, &mut s, "e2e4"), None);
        assert_eq!(show(&mut c, &mut s, "e2e4 e7e5"), None);
        assert!(s.take_commands().contains(&Command::LightsOff));
        // The player's next move asks again.
        assert!(show(&mut c, &mut s, "e2e4 e7e5 g1f3").is_some());
    }

    #[test]
    fn stale_replies_are_dropped() {
        let mut s = Session::new(Follower::new());
        let mut c = Controller::new(Setup::default());
        show(&mut c, &mut s, "");
        show(&mut c, &mut s, "e2e4");
        // Taken back before the engine answered.
        show(&mut c, &mut s, "");
        c.on_reply(1, Some("e7e5"), &mut s, Instant::now());
        assert_eq!(s.follower().game().ply(), 0);
        assert!(!s.follower().awaiting_board());
    }

    #[test]
    fn the_engine_opens_when_the_player_has_black() {
        let mut s = Session::new(Follower::new());
        let mut c = Controller::new(Setup { player: Side::Black, ..Setup::default() });
        let ev = s.receive(&frame(0x13, &[1, 0]), Instant::now());
        let t = c.on_event(&ev[0], &mut s, Instant::now()).expect("White to move: the engine");
        assert_eq!(t.ply, 0);
    }

    #[test]
    fn book_moves_need_no_thinking() {
        let start = chess_core::Game::new(None).unwrap();
        let mut bytes = chess_analysis::book::key(start.board()).to_be_bytes().to_vec();
        // e2e4 in Polyglot bits: to e4 (4 | 3<<3), from e2 (4<<6 | 1<<9).
        let mv: u16 = 4 | (3 << 3) | (4 << 6) | (1 << 9);
        bytes.extend(mv.to_be_bytes());
        bytes.extend(1u16.to_be_bytes());
        bytes.extend([0; 4]);
        let book = Book::from_bytes(&bytes).unwrap();
        let mut s = Session::new(Follower::new());
        let mut c =
            Controller::new(Setup { player: Side::Black, book: Some(book), seed: 7, ..Setup::default() });
        let ev = s.receive(&frame(0x13, &[1, 0]), Instant::now());
        assert_eq!(c.on_event(&ev[0], &mut s, Instant::now()), None, "played from the book");
        assert_eq!(s.follower().game().uci_moves(), ["e2e4"]);
    }

    #[test]
    fn the_result_is_shown() {
        // Fool's mate: the player (White) walks into it.
        let mut s = Session::new(Follower::new());
        let mut c = Controller::new(Setup::default());
        show(&mut c, &mut s, "");
        show(&mut c, &mut s, "f2f3");
        c.on_reply(1, Some("e7e5"), &mut s, Instant::now());
        show(&mut c, &mut s, "f2f3 e7e5");
        show(&mut c, &mut s, "f2f3 e7e5 g2g4");
        c.on_reply(3, Some("d8h4"), &mut s, Instant::now());
        s.take_commands();
        show(&mut c, &mut s, "f2f3 e7e5 g2g4 d8h4");
        assert!(texts(&mut s).contains(&"mate 0-1".to_string()));
    }

    fn show_at(c: &mut Controller, s: &mut Session, moves: &str, now: Instant) -> Option<Think> {
        let mut think = None;
        for e in s.receive(&dump(&fen(moves)), now) {
            if let Some(t) = c.on_event(&e, s, now) {
                think = Some(t);
            }
        }
        think
    }

    fn timed(minutes: u32, inc: u32) -> Controller {
        Controller::new(Setup {
            time: Some(TimeControl { minutes, increment_secs: inc }),
            ..Setup::default()
        })
    }

    fn set_and_runs(s: &mut Session) -> Vec<Command> {
        s.take_commands().into_iter().filter(|c| matches!(c, Command::ClockSetAndRun { .. })).collect()
    }

    #[test]
    fn time_controls_parse() {
        assert_eq!("5+3".parse(), Ok(TimeControl { minutes: 5, increment_secs: 3 }));
        assert_eq!("10".parse(), Ok(TimeControl { minutes: 10, increment_secs: 0 }));
        assert!("0+1".parse::<TimeControl>().is_err());
        assert!("x".parse::<TimeControl>().is_err());
    }

    #[test]
    fn clocks_start_with_the_first_move_and_charge_each_side() {
        let t0 = Instant::now();
        let ms = std::time::Duration::from_millis;
        let mut s = Session::new(Follower::new());
        let mut c = timed(5, 3);
        show_at(&mut c, &mut s, "", t0);
        assert_eq!(
            c.remaining(Side::White, t0 + ms(9_000)),
            Some(300_000),
            "not running before the first move"
        );

        // White (the player) moves after 2 s: no charge before the first move,
        // Black's (the engine's) clock starts.
        let t1 = t0 + ms(2_000);
        let think = show_at(&mut c, &mut s, "e2e4", t1).expect("engine's turn");
        let clock = think.clock.expect("a clock for the engine");
        assert_eq!((clock.white_ms, clock.black_ms, clock.increment_ms), (300_000, 300_000, 3_000));
        let shown = set_and_runs(&mut s);
        assert_eq!(
            shown.last(),
            Some(&Command::ClockSetAndRun {
                left: (0, 5, 0),
                right: (0, 5, 0),
                mode: RunMode::RightRunning,
                lever_toggles: false
            })
        );

        // The engine answers after 1.5 s: charged, plus the increment.
        let t2 = t1 + ms(1_500);
        c.on_reply(1, Some("e7e5"), &mut s, t2);
        assert_eq!(c.remaining(Side::Black, t2), Some(300_000 - 1_500 + 3_000));
        // White's clock runs while the player makes Black's move and thinks.
        assert_eq!(c.remaining(Side::White, t2 + ms(4_000)), Some(296_000));
        show_at(&mut c, &mut s, "e2e4 e7e5", t2 + ms(1_000));
        show_at(&mut c, &mut s, "e2e4 e7e5 g1f3", t2 + ms(4_000));
        assert_eq!(c.remaining(Side::White, t2 + ms(4_000)), Some(300_000 - 4_000 + 3_000));
    }

    #[test]
    fn a_flag_ends_the_game() {
        let t0 = Instant::now();
        let ms = std::time::Duration::from_millis;
        let mut s = Session::new(Follower::new());
        let mut c = timed(1, 0);
        show_at(&mut c, &mut s, "", t0);
        show_at(&mut c, &mut s, "e2e4", t0);
        c.on_reply(1, Some("e7e5"), &mut s, t0 + ms(100));
        s.take_commands();
        c.tick(&mut s, t0 + ms(30_000));
        assert!(texts(&mut s).is_empty(), "still time");
        c.tick(&mut s, t0 + ms(60_200));
        assert_eq!(texts(&mut s), ["flag 0-1"]);
        // Nothing more is asked of the engine.
        assert_eq!(show_at(&mut c, &mut s, "e2e4 e7e5", t0 + ms(61_000)), None);
        assert_eq!(show_at(&mut c, &mut s, "e2e4 e7e5 g1f3", t0 + ms(62_000)), None);
    }

    fn press(c: &mut Controller, s: &mut Session, button: u8, now: Instant) -> Option<Think> {
        let code = b"14325"[usize::from(button)];
        let ack = chess_dgt::ClockAck { code: chess_dgt::ClockAck::BUTTON, data: [0, code], error: false };
        c.on_event(&Event::Clock(ClockReport::Ack(ack)), s, now)
    }

    #[test]
    fn buttons_change_the_level_and_switch_sides() {
        let now = Instant::now();
        let mut s = Session::new(Follower::new());
        let mut c = Controller::new(Setup { level: 4, ..Setup::default() });
        show_at(&mut c, &mut s, "", now);
        press(&mut c, &mut s, 3, now);
        press(&mut c, &mut s, 3, now);
        press(&mut c, &mut s, 1, now);
        assert_eq!(c.level(), 5);
        assert_eq!(texts(&mut s), ["level 5", "level 6", "level 5"]);
        // Switch sides with White to move: the engine plays White now.
        let t = press(&mut c, &mut s, 2, now).expect("the engine moves");
        assert_eq!((t.ply, t.level), (0, 5));
        assert_eq!(c.player(), Side::Black);
    }

    #[test]
    fn pause_stops_the_clock() {
        let t0 = Instant::now();
        let ms = std::time::Duration::from_millis;
        let mut s = Session::new(Follower::new());
        let mut c = timed(5, 0);
        show_at(&mut c, &mut s, "", t0);
        show_at(&mut c, &mut s, "e2e4", t0);
        press(&mut c, &mut s, 4, t0 + ms(1_000));
        assert_eq!(c.remaining(Side::Black, t0 + ms(60_000)), Some(299_000), "paused");
        press(&mut c, &mut s, 4, t0 + ms(60_000));
        assert_eq!(c.remaining(Side::Black, t0 + ms(61_000)), Some(298_000), "running again");
    }
}
