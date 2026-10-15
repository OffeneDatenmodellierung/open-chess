//! The Chessnut boards (Air, Air+, Pro, Go). The app connects with
//! CoreBluetooth, subscribes to [`chessnut_read_characteristic`], hands
//! every notification to [`ChessnutBoard::receive`], calls
//! [`ChessnutBoard::poll`] a few times a second, and writes whatever
//! [`ChessnutBoard::take_writes`] returns to
//! [`chessnut_write_characteristic`].

use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use chess_chessnut::{Event, Session, protocol};
use chess_core::Game;
use chess_dgt::{Follower, Observation, Orientation};

use crate::MobileError;

/// What the board said.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum BoardEvent {
    /// A legal move was made on the board and is now in the game.
    Move {
        uci: String,
        san: String,
    },
    /// The pieces went back this many plies; the game was taken back.
    Takeback {
        plies: u32,
    },
    /// The starting position was set up: a new game.
    NewGame,
    /// The board now shows the move the other side made.
    Caught,
    /// Not a position one step from the game: a move in progress, a
    /// knocked-over piece, or a position being set up.
    Waiting,
    Battery {
        percent: u8,
        charging: bool,
    },
}

impl BoardEvent {
    fn from_event(e: Event) -> Option<Self> {
        Some(match e {
            Event::Battery { percent, charging } => BoardEvent::Battery { percent, charging },
            Event::Game(o) => match o {
                Observation::Unchanged => return None,
                Observation::Move { uci, san } => BoardEvent::Move { uci, san },
                Observation::Takeback { plies } => {
                    BoardEvent::Takeback { plies: u32::try_from(plies).unwrap_or(u32::MAX) }
                }
                Observation::NewGame => BoardEvent::NewGame,
                Observation::Caught => BoardEvent::Caught,
                Observation::Waiting => BoardEvent::Waiting,
            },
        })
    }
}

/// What a connected board shows right now.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct BoardNow {
    /// FEN placement, White's side.
    pub placement: String,
    /// It has been still long enough to count (not a move in progress).
    pub settled: bool,
}

/// The characteristic the board notifies on.
#[uniffi::export]
pub fn chessnut_read_characteristic() -> String {
    protocol::READ.into()
}

/// The characteristic commands are written to.
#[uniffi::export]
pub fn chessnut_write_characteristic() -> String {
    protocol::WRITE.into()
}

/// One connected Chessnut board following one game.
#[derive(uniffi::Object)]
pub struct ChessnutBoard {
    session: Mutex<Session>,
}

fn square(name: &str) -> Option<u8> {
    let b = name.as_bytes();
    let (file, rank) = (b.first()?.checked_sub(b'a')?, b.get(1)?.checked_sub(b'1')?);
    (file < 8 && rank < 8).then_some(rank * 8 + file)
}

#[uniffi::export]
impl ChessnutBoard {
    /// Follow the game from its moves so far (space-separated UCI) from
    /// a FEN or the start. `rotated`: the board is turned so Black sits
    /// at the near side.
    #[uniffi::constructor]
    pub fn new(fen: Option<String>, moves: String, rotated: bool) -> Result<Self, MobileError> {
        let fen = fen.filter(|f| !f.trim().is_empty());
        let game = Game::from_moves(fen.as_deref(), &moves)
            .map_err(|e| MobileError::InvalidPosition { message: format!("{e:?}") })?;
        let mut follower = Follower::from_game(game);
        follower.set_orientation(if rotated { Orientation::Rotated } else { Orientation::Normal });
        Ok(ChessnutBoard { session: Mutex::new(Session::new(follower)) })
    }

    /// Just connected: ask for the position as it changes, and the battery.
    pub fn start(&self) {
        self.lock().start(Instant::now());
    }

    /// A notification's bytes.
    pub fn receive(&self, bytes: Vec<u8>) -> Vec<BoardEvent> {
        self.lock().receive(&bytes, Instant::now()).into_iter().filter_map(BoardEvent::from_event).collect()
    }

    /// Time passing: a position that has stayed still becomes a move. Call
    /// a few times a second while connected.
    pub fn poll(&self) -> Vec<BoardEvent> {
        self.lock().poll(Instant::now()).into_iter().filter_map(BoardEvent::from_event).collect()
    }

    /// Commands to write to the board now, in order.
    pub fn take_writes(&self) -> Vec<Vec<u8>> {
        self.lock().take_writes()
    }

    /// The other side's move (the bot's, or an online opponent's): it goes
    /// into the game and its squares light up until the player makes it
    /// on the board.
    pub fn opponent_move(&self, uci: String) -> Result<String, MobileError> {
        let mut s = self.lock();
        let played =
            s.play(&uci).map_err(|e| MobileError::IllegalMove { message: format!("{uci}: {e:?}") })?;
        let squares: Vec<u8> =
            [&uci.get(0..2), &uci.get(2..4)].into_iter().flatten().filter_map(|n| square(n)).collect();
        s.lights(&squares);
        Ok(played.san)
    }

    /// Light squares by name ("e2", "e4"): a hint, or the last move.
    pub fn light(&self, squares: Vec<String>) {
        let squares: Vec<u8> = squares.iter().filter_map(|n| square(n)).collect();
        self.lock().lights(&squares);
    }

    pub fn lights_off(&self) {
        self.lock().lights_off();
    }

    /// What the board shows now, for labelling photos and video frames
    /// ([`crate::Capture::board_state`]).
    pub fn board_now(&self) -> Option<BoardNow> {
        self.lock().board_now().map(|(p, settled)| BoardNow { placement: p.fen(), settled })
    }

    /// The game as the board has followed it: its FEN.
    pub fn fen(&self) -> String {
        self.lock().game().fen()
    }
}

impl ChessnutBoard {
    fn lock(&self) -> MutexGuard<'_, Session> {
        self.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settle(b: &ChessnutBoard) -> Vec<BoardEvent> {
        std::thread::sleep(std::time::Duration::from_millis(400));
        b.poll()
    }

    #[test]
    fn follows_moves_and_lights_replies() {
        let b = ChessnutBoard::new(None, String::new(), false).unwrap();
        b.start();
        assert_eq!(b.take_writes().len(), 2, "real-time mode and the battery");

        let after_e4 =
            chess_dgt::Placement::from_fen(&Game::from_moves(None, "e2e4").unwrap().fen()).unwrap();
        let mut events = b.receive(protocol::position_bytes(&after_e4));
        events.extend(settle(&b));
        assert!(events.contains(&BoardEvent::Move { uci: "e2e4".into(), san: "e4".into() }), "{events:?}");

        assert_eq!(b.opponent_move("e7e5".into()).unwrap(), "e5");
        let writes = b.take_writes();
        assert_eq!(writes.last().unwrap()[..2], [0x0a, 0x08], "the reply's squares lit");
        assert!(b.opponent_move("e7e5".into()).is_err());
        assert_eq!(square("e2"), Some(12));
        assert_eq!(square("z9"), None);
        assert_eq!(chessnut_write_characteristic().len(), 36);
        let now = b.board_now().unwrap();
        assert!(now.settled && now.placement.starts_with("rnbqkbnr/pppppppp/8/8/4P3"), "{now:?}");
    }
}
