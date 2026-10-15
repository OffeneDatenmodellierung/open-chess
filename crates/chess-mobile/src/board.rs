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

/// One thing to put right on a real board, so it matches the game.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Correction {
    /// The piece, as a FEN letter (`N` a white knight, `p` a black pawn).
    pub piece: String,
    /// Where it wrongly stands (none: it's missing from the board).
    pub from: Option<String>,
    /// Where it belongs (none: it shouldn't be on the board at all).
    pub to: Option<String>,
}

fn square_name(i: usize) -> String {
    let file = ["a", "b", "c", "d", "e", "f", "g", "h"][i % 8];
    format!("{file}{}", i / 8 + 1)
}

/// What differs between the game's position and what a real board shows
/// (both FEN placements), as moves to put it right: a piece on the wrong
/// square with an arrow to where it belongs, a piece to take off, a piece to
/// put back. For showing an illegal or mistaken move on the board in red,
/// with arrows (and lighting the squares on a board that can), until the
/// board matches again. Pieces of a kind are paired nearest first.
#[uniffi::export]
pub fn board_corrections(expected: String, seen: String) -> Vec<Correction> {
    let (Some(want), Some(have)) =
        (chess_dgt::Placement::from_fen(&expected), chess_dgt::Placement::from_fen(&seen))
    else {
        return Vec::new();
    };
    let letter = |p: chess_dgt::Piece| -> char {
        let c = match p.kind {
            chess_dgt::PieceKind::Pawn => 'p',
            chess_dgt::PieceKind::Knight => 'n',
            chess_dgt::PieceKind::Bishop => 'b',
            chess_dgt::PieceKind::Rook => 'r',
            chess_dgt::PieceKind::Queen => 'q',
            chess_dgt::PieceKind::King => 'k',
        };
        if p.white { c.to_ascii_uppercase() } else { c }
    };
    // Squares holding the wrong thing: what's there that shouldn't be, and
    // what should be there but isn't.
    let mut extra: Vec<(usize, char)> = Vec::new();
    let mut missing: Vec<(usize, char)> = Vec::new();
    for i in 0..64 {
        if want.0[i] == have.0[i] {
            continue;
        }
        if let Some(p) = have.0[i] {
            extra.push((i, letter(p)));
        }
        if let Some(p) = want.0[i] {
            missing.push((i, letter(p)));
        }
    }
    let dist = |a: usize, b: usize| {
        let (fa, ra, fb, rb) = (a % 8, a / 8, b % 8, b / 8);
        fa.abs_diff(fb).max(ra.abs_diff(rb))
    };
    let mut out = Vec::new();
    // Pair each misplaced piece with the nearest square that wants one like it.
    let mut pairs: Vec<(usize, usize, usize)> = Vec::new();
    for (ei, &(from, c)) in extra.iter().enumerate() {
        for (mi, &(to, d)) in missing.iter().enumerate() {
            if c == d {
                pairs.push((dist(from, to), ei, mi));
            }
        }
    }
    pairs.sort_unstable();
    let (mut used_e, mut used_m) = (vec![false; extra.len()], vec![false; missing.len()]);
    for (_, ei, mi) in pairs {
        if used_e[ei] || used_m[mi] {
            continue;
        }
        used_e[ei] = true;
        used_m[mi] = true;
        out.push(Correction {
            piece: extra[ei].1.to_string(),
            from: Some(square_name(extra[ei].0)),
            to: Some(square_name(missing[mi].0)),
        });
    }
    for (k, &(sq, c)) in extra.iter().enumerate() {
        if !used_e[k] {
            out.push(Correction { piece: c.to_string(), from: Some(square_name(sq)), to: None });
        }
    }
    for (k, &(sq, c)) in missing.iter().enumerate() {
        if !used_m[k] {
            out.push(Correction { piece: c.to_string(), from: None, to: Some(square_name(sq)) });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settle(b: &ChessnutBoard) -> Vec<BoardEvent> {
        std::thread::sleep(std::time::Duration::from_millis(400));
        b.poll()
    }

    #[test]
    fn corrections_put_a_board_right() {
        let start = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR".to_owned();
        assert!(board_corrections(start.clone(), start.clone()).is_empty());
        // Nf3 then the knight wrongly on e5 (an illegal move from f3).
        let after = "rnbqkbnr/pppppppp/8/8/8/5N2/PPPPPPPP/RNBQKB1R".to_owned();
        let wrong = "rnbqkbnr/pppppppp/8/4N3/8/8/PPPPPPPP/RNBQKB1R".to_owned();
        assert_eq!(
            board_corrections(after.clone(), wrong),
            [Correction { piece: "N".into(), from: Some("e5".into()), to: Some("f3".into()) }]
        );
        // A pawn knocked off the board, and a stray queen.
        let missing = "rnbqkbnr/pppppppp/8/8/8/5N2/PPPP1PPP/RNBQKBQR".to_owned();
        let c = board_corrections(after, missing);
        assert!(c.contains(&Correction { piece: "Q".into(), from: Some("g1".into()), to: None }), "{c:?}");
        assert!(c.contains(&Correction { piece: "P".into(), from: None, to: Some("e2".into()) }), "{c:?}");
        assert!(board_corrections("nonsense".into(), start).is_empty());
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
