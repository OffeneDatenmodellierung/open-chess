//! Following a game on a board that senses only whether each square has a
//! piece, not which piece (the DGT Centaur).
//!
//! The board reports each piece lifted and put down. The follower keeps
//! the squares' occupancy and, at each lift or put-down, looks for the
//! legal move that leaves exactly that occupancy *and* fits what was just
//! done: a move is complete when its piece is put down on its square (a
//! capture: the captured piece lifted, then the capturing piece put down
//! in its place), castling when the second of king and rook is put down,
//! and en passant when the captured pawn is lifted, if that comes last.
//! So lifting a piece that could capture doesn't count as capturing.
//!
//! Positions earlier in the game, set up again, take moves back; the
//! starting position starts a new game. A move played elsewhere (an
//! engine) waits until the board shows it ([`Follower::play`]).
//!
//! Promotions are to a queen: the board can't tell what was put down.

use chess_core::{Game, GameError, Played};
use chess_dgt::Observation;

use crate::protocol::Field;

/// The starting position's occupancy: ranks 1, 2, 7 and 8.
pub const START: u64 = 0xffff_0000_0000_ffff;

/// How far back a takeback is looked for.
const TAKEBACK_PLIES: usize = 12;

/// The squares with a piece in a game's position (bit 0 = a1).
pub fn occupancy(game: &Game) -> u64 {
    game.board().occupied().0
}

fn square(name: &str) -> Option<u8> {
    let b = name.as_bytes();
    let (f, r) = (b.first()?.wrapping_sub(b'a'), b.get(1)?.wrapping_sub(b'1'));
    (f < 8 && r < 8).then_some(r * 8 + f)
}

/// A move's squares, from UCI with standard castling notation.
#[derive(Debug, Clone, Copy)]
struct Shape {
    from: u8,
    to: u8,
    /// The rook's squares, when castling.
    rook: Option<(u8, u8)>,
    /// The pawn taken en passant.
    en_passant: Option<u8>,
}

/// The piece letters on each square (FEN letters; bit 0 = a1).
fn pieces(game: &Game) -> [Option<char>; 64] {
    let mut out = [None; 64];
    let fen = game.fen();
    let placement = fen.split(' ').next().unwrap_or("");
    for (row, rank) in placement.split('/').enumerate() {
        let mut file = 0usize;
        for c in rank.chars() {
            if let Some(n) = c.to_digit(10) {
                file += n as usize;
            } else if file < 8 && row < 8 {
                out[(7 - row) * 8 + file] = Some(c);
                file += 1;
            }
        }
    }
    out
}

fn shape(game: &Game, uci: &str) -> Option<Shape> {
    let (from, to) = (square(uci.get(0..2)?)?, square(uci.get(2..4)?)?);
    let board = pieces(game);
    let kind = board[usize::from(from)].map(|c| c.to_ascii_lowercase());
    let (ff, tf) = (from % 8, to % 8);
    let rook = (kind == Some('k') && ff.abs_diff(tf) == 2).then(|| {
        let rank = from - ff;
        if tf > ff { (rank + 7, rank + 5) } else { (rank, rank + 3) }
    });
    let en_passant =
        (kind == Some('p') && ff != tf && board[usize::from(to)].is_none()).then_some(from - ff + tf);
    Some(Shape { from, to, rook, en_passant })
}

/// Follows a game on an occupancy-sensing board.
#[derive(Debug, Clone)]
pub struct Follower {
    game: Game,
    occupied: u64,
    awaiting_board: bool,
}

impl Default for Follower {
    fn default() -> Self {
        Follower::new()
    }
}

impl Follower {
    /// A new game, the board assumed set up.
    pub fn new() -> Follower {
        Follower {
            game: Game::new(None).expect("the starting position"),
            occupied: START,
            awaiting_board: false,
        }
    }

    pub fn game(&self) -> &Game {
        &self.game
    }

    /// The occupancy as the board last reported it.
    pub fn occupied(&self) -> u64 {
        self.occupied
    }

    /// A move was played elsewhere and still has to be made on the board.
    pub fn awaiting_board(&self) -> bool {
        self.awaiting_board
    }

    /// Play a move made elsewhere (an engine, an online opponent); the
    /// board then waits for it to be made.
    pub fn play(&mut self, uci: &str) -> Result<Played, GameError> {
        let played = self.game.play_uci(uci)?;
        self.awaiting_board = true;
        Ok(played)
    }

    /// Start again from a position (the board is then expected to show
    /// it; until it does, nothing it shows is a move).
    pub fn set_position(&mut self, fen: &str) -> Result<(), GameError> {
        self.game = Game::new(Some(fen))?;
        self.awaiting_board = false;
        Ok(())
    }

    /// A piece lifted or put down.
    pub fn field(&mut self, f: Field) -> Observation {
        match f {
            Field::Lifted(s) => self.occupied &= !(1 << s),
            Field::Placed(s) => self.occupied |= 1 << s,
        }
        self.observe(Some(f))
    }

    /// The whole board's occupancy, read afresh (at start, or to catch up
    /// with changes the events missed). Only takebacks, new games and a
    /// move made elsewhere are recognised this way; a move needs the
    /// events, to tell which piece went where.
    pub fn sync(&mut self, occupied: u64) -> Observation {
        self.occupied = occupied;
        self.observe(None)
    }

    fn observe(&mut self, last: Option<Field>) -> Observation {
        let o = self.occupied;
        if o == START {
            let fresh = self.game.ply() == 0 && self.game.initial_fen().is_none();
            if fresh && !self.awaiting_board {
                return Observation::Unchanged;
            }
            self.game = Game::new(None).expect("the starting position");
            self.awaiting_board = false;
            return Observation::NewGame;
        }
        if o == occupancy(&self.game) {
            if self.awaiting_board {
                self.awaiting_board = false;
                return Observation::Caught;
            }
            return Observation::Unchanged;
        }
        if !self.awaiting_board
            && let Some(last) = last
            && let Some(uci) = self.find_move(o, last)
        {
            let played = self.game.play_uci(&uci).expect("a legal move");
            return Observation::Move { uci, san: played.san };
        }
        // An earlier position: moves taken back. While a move made
        // elsewhere waits, the board still shows the position before it,
        // which is no takeback.
        let ply = self.game.ply();
        let first = if self.awaiting_board { 2 } else { 1 };
        for plies in first..=ply.min(TAKEBACK_PLIES) {
            let mut earlier = self.game.clone();
            earlier.undo(plies);
            if occupancy(&earlier) == o {
                self.game = earlier;
                self.awaiting_board = false;
                return Observation::Takeback { plies };
            }
        }
        Observation::Waiting
    }

    /// The legal move that leaves occupancy `o` and that `last` completes.
    fn find_move(&self, o: u64, last: Field) -> Option<String> {
        let mut found: Option<(String, Shape)> = None;
        for uci in self.game.legal_moves() {
            let mut g = self.game.clone();
            if g.play_uci(&uci).is_err() || occupancy(&g) != o {
                continue;
            }
            let Some(s) = shape(&self.game, &uci) else { continue };
            let completes = match last {
                Field::Placed(sq) => sq == s.to || s.rook.is_some_and(|(_, rook_to)| sq == rook_to),
                Field::Lifted(sq) => s.en_passant == Some(sq),
            };
            if !completes {
                continue;
            }
            match &found {
                // Promotions to each piece look the same: take the queen.
                Some((_, f)) if f.from == s.from && f.to == s.to => {
                    if uci.ends_with('q') {
                        found = Some((uci, s));
                    }
                }
                Some(_) => return None,
                None => found = Some((uci, s)),
            }
        }
        found.map(|(uci, _)| uci)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Field::{Lifted, Placed};

    fn sq(name: &str) -> u8 {
        square(name).unwrap()
    }

    fn up(n: &str) -> Field {
        Lifted(sq(n))
    }

    fn down(n: &str) -> Field {
        Placed(sq(n))
    }

    /// Feed events; the observations that weren't Unchanged/Waiting.
    fn feed(f: &mut Follower, events: &[Field]) -> Vec<Observation> {
        events
            .iter()
            .map(|&e| f.field(e))
            .filter(|o| !matches!(o, Observation::Unchanged | Observation::Waiting))
            .collect()
    }

    fn mv(uci: &str, san: &str) -> Observation {
        Observation::Move { uci: uci.into(), san: san.into() }
    }

    fn after(moves: &str) -> Follower {
        let mut f = Follower::new();
        for m in moves.split_whitespace() {
            f.game.play_uci(m).unwrap();
        }
        f.occupied = occupancy(&f.game);
        f
    }

    #[test]
    fn a_move_is_a_lift_and_a_put_down() {
        let mut f = Follower::new();
        assert_eq!(feed(&mut f, &[up("e2"), down("e4")]), [mv("e2e4", "e4")]);
        // Picking a piece up and putting it back changes nothing.
        assert_eq!(feed(&mut f, &[up("g8"), down("g8")]), []);
        assert_eq!(feed(&mut f, &[up("g8"), down("f6")]), [mv("g8f6", "Nf6")]);
    }

    #[test]
    fn a_capture_is_decided_by_where_the_piece_lands() {
        // Black pawns on d5 and e4: the knight on c3 can take either.
        let mut f = after("d2d4 e7e5 b1c3 d7d5 g1f3 e5e4");
        // Lifting the knight doesn't capture anything yet.
        assert_eq!(f.field(up("c3")), Observation::Waiting);
        assert_eq!(f.field(up("e4")), Observation::Waiting);
        assert_eq!(f.field(down("e4")), mv("c3e4", "Nxe4"));
        // Lifting the captured piece first works too.
        let mut f = after("d2d4 e7e5 b1c3 d7d5 g1f3 e5e4");
        assert_eq!(feed(&mut f, &[up("d5"), up("c3"), down("d5")]), [mv("c3d5", "Nxd5")]);
    }

    #[test]
    fn castling_completes_with_both_pieces() {
        let mut f = after("e2e4 e7e5 g1f3 b8c6 f1c4 g8f6");
        assert_eq!(f.field(up("e1")), Observation::Waiting);
        assert_eq!(f.field(down("g1")), Observation::Waiting);
        assert_eq!(f.field(up("h1")), Observation::Waiting);
        assert_eq!(f.field(down("f1")), mv("e1g1", "O-O"));
    }

    #[test]
    fn en_passant_with_the_captured_pawn_lifted_last() {
        let mut f = after("e2e4 a7a6 e4e5 d7d5");
        assert_eq!(feed(&mut f, &[up("e5"), down("d6"), up("d5")]), [mv("e5d6", "exd6")]);
        // Or lifted first.
        let mut f = after("e2e4 a7a6 e4e5 d7d5");
        assert_eq!(feed(&mut f, &[up("d5"), up("e5"), down("d6")]), [mv("e5d6", "exd6")]);
    }

    #[test]
    fn promotions_are_to_a_queen() {
        // White's pawn on h7, Black's knight on g8 (and rook on h8).
        let mut f = after("h2h4 g7g5 h4g5 h7h6 g5h6 g8f6 h6h7 f6g8");
        assert_eq!(feed(&mut f, &[up("h7"), up("g8"), down("g8")]), [mv("h7g8q", "hxg8=Q")]);
    }

    #[test]
    fn setting_back_an_earlier_position_takes_moves_back() {
        let mut f = Follower::new();
        feed(&mut f, &[up("e2"), down("e4"), up("e7"), down("e5")]);
        assert_eq!(f.game().ply(), 2);
        assert_eq!(feed(&mut f, &[up("e5"), down("e7")]), [Observation::Takeback { plies: 1 }]);
        assert_eq!(f.game().ply(), 1);
    }

    #[test]
    fn the_starting_position_starts_a_new_game() {
        let mut f = Follower::new();
        feed(&mut f, &[up("e2"), down("e4"), up("e7"), down("e5"), up("g1"), down("f3")]);
        assert_eq!(f.sync(START), Observation::NewGame);
        assert_eq!(f.game().ply(), 0);
    }

    #[test]
    fn an_engine_move_waits_for_the_board() {
        let mut f = Follower::new();
        feed(&mut f, &[up("e2"), down("e4")]);
        f.play("e7e5").unwrap();
        assert!(f.awaiting_board());
        // Not the player's move to make: other moves aren't taken.
        assert_eq!(f.field(up("d2")), Observation::Waiting);
        assert_eq!(f.field(down("d2")), Observation::Waiting);
        assert_eq!(feed(&mut f, &[up("e7"), down("e5")]), [Observation::Caught]);
        assert!(!f.awaiting_board());
    }
}
