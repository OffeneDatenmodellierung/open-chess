//! The game on the board against an engine, without I/O.
//!
//! The controller reacts to the board's events: when it's the engine's
//! turn it asks for a move ([`Think`]); when the move comes back it plays
//! it in the game, shows it on the clock and lights its squares
//! (Revelation II), and waits for the player to make it on the board. It
//! uses the opening book first when there is one, and shows the result
//! when the game ends.

use chess_analysis::book::Book;
use chess_core::{Game, Side, Status};
use chess_dgt::{Command, Event, Observation, Session, Square};

/// A request for the engine's move in a position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Think {
    /// The game's ply when asked: a reply for another ply is stale.
    pub ply: usize,
    pub initial_fen: Option<String>,
    pub moves: Vec<String>,
}

/// How the game is set up.
#[derive(Debug, Clone, Default)]
pub struct Setup {
    /// The side the person plays.
    pub player: Option<Side>,
    pub book: Option<Book>,
    /// For choosing among book moves.
    pub seed: u64,
}

/// One board game against an engine.
#[derive(Debug)]
pub struct Controller {
    player: Side,
    book: Option<Book>,
    rng: u64,
    thinking: Option<usize>,
    ended: bool,
}

impl Controller {
    pub fn new(setup: Setup) -> Controller {
        Controller {
            player: setup.player.unwrap_or(Side::White),
            book: setup.book,
            rng: setup.seed | 1,
            thinking: None,
            ended: false,
        }
    }

    pub fn player(&self) -> Side {
        self.player
    }

    /// Whether a move has been asked for and not answered yet.
    pub fn thinking(&self) -> bool {
        self.thinking.is_some()
    }

    /// React to the board. May ask for the engine's move.
    pub fn on_event(&mut self, event: &Event, s: &mut Session) -> Option<Think> {
        match event {
            Event::Connected { .. } => {
                say(s, "ready");
                self.engine_turn(s)
            }
            Event::Game(Observation::NewGame) => {
                self.thinking = None;
                self.ended = false;
                s.send(Command::LightsOff);
                say(s, "new game");
                self.engine_turn(s)
            }
            Event::Game(Observation::Move { san, .. }) => {
                say(s, san);
                self.engine_turn(s)
            }
            Event::Game(Observation::Takeback { .. }) => {
                self.thinking = None;
                self.ended = false;
                s.send(Command::LightsOff);
                say(s, "takeback");
                self.engine_turn(s)
            }
            Event::Game(Observation::Caught) => {
                s.send(Command::LightsOff);
                s.send(Command::ClockEndText);
                self.check_end(s);
                None
            }
            _ => None,
        }
    }

    /// The engine's answer. Stale answers (the game moved on, a takeback,
    /// a new game) are dropped.
    pub fn on_reply(&mut self, ply: usize, uci: Option<&str>, s: &mut Session) -> Option<Think> {
        if self.thinking != Some(ply) || s.follower().game().ply() != ply {
            return None;
        }
        self.thinking = None;
        match uci {
            Some(uci) => Self::play_engine_move(uci, s),
            None => say(s, "no move"),
        }
        None
    }

    fn engine_turn(&mut self, s: &mut Session) -> Option<Think> {
        if self.check_end(s) || self.thinking.is_some() || s.follower().awaiting_board() {
            return None;
        }
        let game = s.follower().game();
        if game.turn() == self.player {
            return None;
        }
        if let Some(book) = &self.book {
            self.rng = splitmix(self.rng);
            if let Some(uci) = book.pick(game.board(), (self.rng >> 32) as u32) {
                Self::play_engine_move(&uci, s);
                return None;
            }
        }
        let think = Think {
            ply: game.ply(),
            initial_fen: game.initial_fen().map(str::to_owned),
            moves: game.uci_moves(),
        };
        self.thinking = Some(think.ply);
        say(s, "thinking");
        Some(think)
    }

    fn play_engine_move(uci: &str, s: &mut Session) {
        match s.follower_mut().play(uci) {
            Ok(played) => {
                s.send(Command::ClockText { text: played.san.clone(), beep: 2 });
                if let (Some(from), Some(to)) = (Square::from_name(&uci[0..2]), Square::from_name(&uci[2..4]))
                {
                    s.send(Command::Lights { from, to });
                }
            }
            Err(e) => say(s, &format!("bad {e}")),
        }
    }

    /// Show the result once, when the game is over. Whether it is.
    fn check_end(&mut self, s: &mut Session) -> bool {
        let Some(outcome) = s.follower().game().outcome() else { return false };
        if !self.ended && !s.follower().awaiting_board() {
            self.ended = true;
            s.send(Command::ClockText { text: result_text(outcome.status, outcome.winner), beep: 4 });
        }
        true
    }
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
            if let Some(t) = c.on_event(&e, s) {
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

        c.on_reply(1, Some("e7e5"), &mut s);
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
        c.on_reply(1, Some("e7e5"), &mut s);
        assert_eq!(s.follower().game().ply(), 0);
        assert!(!s.follower().awaiting_board());
    }

    #[test]
    fn the_engine_opens_when_the_player_has_black() {
        let mut s = Session::new(Follower::new());
        let mut c = Controller::new(Setup { player: Some(Side::Black), ..Setup::default() });
        let ev = s.receive(&frame(0x13, &[1, 0]), Instant::now());
        let t = c.on_event(&ev[0], &mut s).expect("White to move: the engine");
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
        let mut c = Controller::new(Setup { player: Some(Side::Black), book: Some(book), seed: 7 });
        let ev = s.receive(&frame(0x13, &[1, 0]), Instant::now());
        assert_eq!(c.on_event(&ev[0], &mut s), None, "played from the book");
        assert_eq!(s.follower().game().uci_moves(), ["e2e4"]);
    }

    #[test]
    fn the_result_is_shown() {
        // Fool's mate: the player (White) walks into it.
        let mut s = Session::new(Follower::new());
        let mut c = Controller::new(Setup::default());
        show(&mut c, &mut s, "");
        show(&mut c, &mut s, "f2f3");
        c.on_reply(1, Some("e7e5"), &mut s);
        show(&mut c, &mut s, "f2f3 e7e5");
        show(&mut c, &mut s, "f2f3 e7e5 g2g4");
        c.on_reply(3, Some("d8h4"), &mut s);
        s.take_commands();
        show(&mut c, &mut s, "f2f3 e7e5 g2g4 d8h4");
        assert!(texts(&mut s).contains(&"mate 0-1".to_string()));
    }
}
