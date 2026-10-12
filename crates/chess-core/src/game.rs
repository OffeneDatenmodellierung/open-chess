//! A game: the starting position, the moves played and the rules that end
//! it on the board (mate, stalemate, insufficient material, threefold
//! repetition, the fifty-move rule).
//!
//! Moves go in and come out as UCI in standard notation (castling is the
//! king's two-square move, `e1g1`), as Lichess's Board and Bot APIs use
//! for standard chess. cozy-chess represents castling as king-takes-rook
//! (`e1h1`); both spellings are accepted on input.

use std::fmt;

use cozy_chess::{Board, Color, File, Move, Piece, Square};
use serde::{Deserialize, Serialize};

use crate::san::san;
use crate::status::Status;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    White,
    Black,
}

impl Side {
    pub fn index(self) -> usize {
        match self {
            Side::White => 0,
            Side::Black => 1,
        }
    }

    #[must_use]
    pub fn opposite(self) -> Side {
        match self {
            Side::White => Side::Black,
            Side::Black => Side::White,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Side::White => "white",
            Side::Black => "black",
        }
    }

    pub fn from_name(s: &str) -> Option<Side> {
        match s {
            "white" => Some(Side::White),
            "black" => Some(Side::Black),
            _ => None,
        }
    }

    /// The side to move at a given ply (0-based, from a position where
    /// `first` was to move).
    pub fn at_ply(first: Side, ply: usize) -> Side {
        if ply.is_multiple_of(2) { first } else { first.opposite() }
    }

    fn color(self) -> Color {
        match self {
            Side::White => Color::White,
            Side::Black => Color::Black,
        }
    }

    fn from_color(c: Color) -> Side {
        match c {
            Color::White => Side::White,
            Color::Black => Side::Black,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameError {
    /// The starting FEN doesn't describe a legal, playable position.
    BadFen(String),
    /// Not a move in UCI notation.
    NotUci(String),
    /// A well-formed move that isn't legal here.
    IllegalMove(String),
    /// The game is over on the board already.
    Over,
}

impl fmt::Display for GameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GameError::BadFen(e) => write!(f, "invalid FEN: {e}"),
            GameError::NotUci(m) => write!(f, "{m} isn't a UCI move"),
            GameError::IllegalMove(m) => write!(f, "Piece on {} cannot move to {}", uci_from(m), uci_to(m)),
            GameError::Over => f.write_str("the game is over"),
        }
    }
}

fn uci_from(m: &str) -> &str {
    m.get(..2).unwrap_or(m)
}

fn uci_to(m: &str) -> &str {
    m.get(2..4).unwrap_or("")
}

impl std::error::Error for GameError {}

/// How a game ended on the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub status: Status,
    pub winner: Option<Side>,
}

/// A move that was played.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Played {
    /// UCI, standard castling notation.
    pub uci: String,
    pub san: String,
    pub capture: bool,
    pub check: bool,
}

#[derive(Debug, Clone)]
pub struct Game {
    initial: Board,
    /// `None` for the standard starting position.
    initial_fen: Option<String>,
    board: Board,
    moves: Vec<Move>,
    sans: Vec<String>,
    /// Positions since the last capture or pawn move (repetition).
    positions: Vec<Board>,
}

impl Game {
    /// A new game from the start, or from a FEN (`"startpos"` counts as the
    /// start). A FEN position must be legal and have a move to play.
    pub fn new(initial_fen: Option<&str>) -> Result<Game, GameError> {
        let fen = initial_fen.map(str::trim).filter(|f| !f.is_empty() && *f != "startpos");
        let (board, initial_fen) = match fen {
            None => (Board::default(), None),
            Some(f) => {
                let b = Board::from_fen(f, false).map_err(|e| GameError::BadFen(format!("{e:?}")))?;
                let standard = b == Board::default();
                (b, if standard { None } else { Some(f.to_owned()) })
            }
        };
        if !board.generate_moves(|_| true) {
            return Err(GameError::BadFen("the position is already over".into()));
        }
        Ok(Game {
            initial: board.clone(),
            initial_fen,
            board: board.clone(),
            moves: Vec::new(),
            sans: Vec::new(),
            positions: vec![board],
        })
    }

    /// Rebuild a game from its starting FEN and space-separated UCI moves.
    pub fn from_moves(initial_fen: Option<&str>, moves: &str) -> Result<Game, GameError> {
        let mut g = Game::new(initial_fen)?;
        for m in moves.split_whitespace() {
            g.play_uci(m)?;
        }
        Ok(g)
    }

    /// Play a move given in UCI. The game must not be over on the board.
    pub fn play_uci(&mut self, uci: &str) -> Result<Played, GameError> {
        if self.outcome().is_some() {
            return Err(GameError::Over);
        }
        let mv = self.parse_uci(uci)?;
        let capture = self.is_capture(mv);
        let san = san(&self.board, mv);
        let out = self.standard_uci(mv);
        self.board.play_unchecked(mv);
        if self.board.halfmove_clock() == 0 {
            self.positions.clear();
        }
        self.positions.push(self.board.clone());
        self.moves.push(mv);
        self.sans.push(san.clone());
        let check = !self.board.checkers().is_empty();
        Ok(Played { uci: out, san, capture, check })
    }

    fn parse_uci(&self, uci: &str) -> Result<Move, GameError> {
        let s = uci.trim();
        let mut mv: Move = s.parse().map_err(|_| GameError::NotUci(s.to_owned()))?;
        // Standard castling (e1g1) → cozy's king-takes-rook (e1h1).
        if self.board.piece_on(mv.from) == Some(Piece::King)
            && mv.from.rank() == mv.to.rank()
            && (mv.from.file() as i8 - mv.to.file() as i8).abs() == 2
        {
            let rook_file = if mv.to.file() > mv.from.file() { File::H } else { File::A };
            mv.to = Square::new(rook_file, mv.from.rank());
        }
        if self.board.is_legal(mv) { Ok(mv) } else { Err(GameError::IllegalMove(s.to_owned())) }
    }

    fn is_capture(&self, mv: Move) -> bool {
        let us = self.board.side_to_move();
        match self.board.color_on(mv.to) {
            Some(c) => c != us,
            // En passant: a pawn changing file onto an empty square.
            None => self.board.piece_on(mv.from) == Some(Piece::Pawn) && mv.from.file() != mv.to.file(),
        }
    }

    fn standard_uci(&self, mv: Move) -> String {
        standard_uci(&self.board, mv)
    }

    pub fn turn(&self) -> Side {
        Side::from_color(self.board.side_to_move())
    }

    /// Who moved first (white, unless a FEN says otherwise).
    pub fn first_to_move(&self) -> Side {
        Side::from_color(self.initial.side_to_move())
    }

    pub fn ply(&self) -> usize {
        self.moves.len()
    }

    pub fn fen(&self) -> String {
        self.board.to_string()
    }

    pub fn initial_fen(&self) -> Option<&str> {
        self.initial_fen.as_deref()
    }

    /// The starting FEN, always spelled out.
    pub fn initial_fen_or_start(&self) -> String {
        self.initial.to_string()
    }

    pub fn board(&self) -> &Board {
        &self.board
    }

    /// Every move so far, UCI with standard castling.
    pub fn uci_moves(&self) -> Vec<String> {
        let mut b = self.initial.clone();
        let mut out = Vec::with_capacity(self.moves.len());
        for &mv in &self.moves {
            out.push(standard_uci(&b, mv));
            b.play_unchecked(mv);
        }
        out
    }

    pub fn sans(&self) -> &[String] {
        &self.sans
    }

    /// Legal moves here, UCI with standard castling.
    pub fn legal_moves(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.board.generate_moves(|pm| {
            for mv in pm {
                out.push(self.standard_uci(mv));
            }
            false
        });
        out
    }

    /// The position before each move and the move, for analysis: FEN and
    /// UCI pairs.
    pub fn positions_and_moves(&self) -> Vec<(String, String)> {
        let mut b = self.initial.clone();
        let ucis = self.uci_moves();
        let mut out = Vec::with_capacity(ucis.len());
        for (&mv, u) in self.moves.iter().zip(ucis) {
            out.push((b.to_string(), u));
            b.play_unchecked(mv);
        }
        out
    }

    /// Take back the last `n` moves.
    pub fn undo(&mut self, n: usize) {
        let keep = self.moves.len().saturating_sub(n);
        let moves: Vec<Move> = self.moves[..keep].to_vec();
        let mut g = Game {
            initial: self.initial.clone(),
            initial_fen: self.initial_fen.clone(),
            board: self.initial.clone(),
            moves: Vec::new(),
            sans: Vec::new(),
            positions: vec![self.initial.clone()],
        };
        for mv in moves {
            let san = san(&g.board, mv);
            g.board.play_unchecked(mv);
            if g.board.halfmove_clock() == 0 {
                g.positions.clear();
            }
            g.positions.push(g.board.clone());
            g.moves.push(mv);
            g.sans.push(san);
        }
        *self = g;
    }

    /// How the game ended on the board, if it has.
    pub fn outcome(&self) -> Option<Outcome> {
        let has_moves = self.board.generate_moves(|_| true);
        if !has_moves {
            return Some(if self.board.checkers().is_empty() {
                Outcome { status: Status::Stalemate, winner: None }
            } else {
                Outcome { status: Status::Mate, winner: Some(self.turn().opposite()) }
            });
        }
        if self.insufficient_material() || self.repetitions() >= 3 || self.board.halfmove_clock() >= 100 {
            return Some(Outcome { status: Status::Draw, winner: None });
        }
        None
    }

    /// How many times the current position has occurred (FIDE identity:
    /// side to move, castling rights and a usable en passant square).
    pub fn repetitions(&self) -> usize {
        self.positions.iter().filter(|b| b.same_position(&self.board)).count()
    }

    /// Neither side can ever mate: kings only, a single minor piece, or
    /// bishops all on one colour of square.
    pub fn insufficient_material(&self) -> bool {
        let b = &self.board;
        if !(b.pieces(Piece::Pawn) | b.pieces(Piece::Rook) | b.pieces(Piece::Queen)).is_empty() {
            return false;
        }
        let knights = b.pieces(Piece::Knight).len();
        let bishops = b.pieces(Piece::Bishop);
        if knights == 0 {
            let light = bishops.into_iter().filter(|sq| is_light(*sq)).count();
            return light == 0 || light == bishops.len() as usize;
        }
        knights == 1 && bishops.is_empty()
    }

    /// `side` has no way to mate even with help: a bare king, or a single
    /// minor piece against a bare king. Used when the other side flags (a
    /// timeout against a side that can't win is a draw).
    pub fn cannot_win(&self, side: Side) -> bool {
        let b = &self.board;
        let ours = b.colors(side.color());
        let theirs = b.colors(side.opposite().color());
        let king_only = |bb: cozy_chess::BitBoard| bb.len() == 1;
        if king_only(ours) {
            return true;
        }
        let minors = (b.pieces(Piece::Knight) | b.pieces(Piece::Bishop)) & ours;
        ours.len() == 2 && minors.len() == 1 && king_only(theirs)
    }
}

/// cozy's move → UCI with standard castling (king-takes-rook → e1g1).
fn standard_uci(board: &Board, mv: Move) -> String {
    let us = board.side_to_move();
    if board.piece_on(mv.from) == Some(Piece::King) && board.color_on(mv.to) == Some(us) {
        let file = if mv.to.file() > mv.from.file() { File::G } else { File::C };
        return Move { from: mv.from, to: Square::new(file, mv.from.rank()), promotion: None }.to_string();
    }
    mv.to_string()
}

fn is_light(sq: Square) -> bool {
    (sq.file() as u8 + sq.rank() as u8) % 2 == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn play(moves: &str) -> Game {
        Game::from_moves(None, moves).unwrap()
    }

    #[test]
    fn fools_mate() {
        let g = play("f2f3 e7e5 g2g4 d8h4");
        assert_eq!(g.outcome(), Some(Outcome { status: Status::Mate, winner: Some(Side::Black) }));
        assert_eq!(g.sans().last().unwrap(), "Qh4#");
        assert_eq!(g.play_uci_clone("a2a3"), Err(GameError::Over));
    }

    impl Game {
        fn play_uci_clone(&self, m: &str) -> Result<Played, GameError> {
            self.clone().play_uci(m)
        }
    }

    #[test]
    fn castling_both_spellings_come_out_standard() {
        let mut g = play("e2e4 e7e5 g1f3 b8c6 f1c4 g8f6");
        let p = g.play_uci("e1g1").unwrap();
        assert_eq!((p.uci.as_str(), p.san.as_str()), ("e1g1", "O-O"));
        let mut g2 = play("e2e4 e7e5 g1f3 b8c6 f1c4 g8f6");
        let p2 = g2.play_uci("e1h1").unwrap();
        assert_eq!(p2.uci, "e1g1", "king-takes-rook is accepted and normalised");
        assert_eq!(g.uci_moves().last().unwrap(), "e1g1");
        assert!(g.legal_moves().iter().all(|m| m != "e8h8"));
    }

    #[test]
    fn queenside_castle() {
        let mut g = play("d2d4 d7d5 b1c3 b8c6 c1f4 c8f5 d1d2 d8d7");
        let p = g.play_uci("e1c1").unwrap();
        assert_eq!((p.uci.as_str(), p.san.as_str()), ("e1c1", "O-O-O"));
    }

    #[test]
    fn illegal_and_malformed_moves() {
        let mut g = Game::new(None).unwrap();
        assert_eq!(g.play_uci("e2e5"), Err(GameError::IllegalMove("e2e5".into())));
        assert_eq!(g.play_uci("zz"), Err(GameError::NotUci("zz".into())));
        assert_eq!(g.ply(), 0);
        assert_eq!(GameError::IllegalMove("e2e5".into()).to_string(), "Piece on e2 cannot move to e5");
    }

    #[test]
    fn en_passant_and_promotion() {
        let mut g = play("e2e4 a7a6 e4e5 d7d5");
        let p = g.play_uci("e5d6").unwrap();
        assert!(p.capture);
        assert_eq!(p.san, "exd6");
        let mut g = Game::new(Some("8/P6k/8/8/8/8/8/K7 w - - 0 1")).unwrap();
        let p = g.play_uci("a7a8q").unwrap();
        assert_eq!((p.uci.as_str(), p.san.as_str()), ("a7a8q", "a8=Q"));
    }

    #[test]
    fn stalemate() {
        let mut g = Game::new(Some("k7/8/1Q6/8/8/8/8/7K w - - 0 1")).unwrap();
        g.play_uci("b6b5").unwrap();
        assert!(g.outcome().is_none(), "the king still has a7");
        let mut g = Game::new(Some("k7/8/8/2Q5/8/8/8/7K w - - 0 1")).unwrap();
        g.play_uci("c5b6").unwrap();
        assert_eq!(g.outcome(), Some(Outcome { status: Status::Stalemate, winner: None }));
    }

    #[test]
    fn threefold_repetition() {
        let mut g = play("g1f3 g8f6 f3g1 f6g8 g1f3 g8f6 f3g1");
        assert!(g.outcome().is_none());
        assert_eq!(g.repetitions(), 2);
        g.play_uci("f6g8").unwrap();
        assert_eq!(g.repetitions(), 3, "the starting position, a third time");
        assert_eq!(g.outcome(), Some(Outcome { status: Status::Draw, winner: None }));
    }

    #[test]
    fn fifty_move_rule() {
        let mut g = Game::new(Some("4k3/8/8/8/8/8/8/R3K3 w - - 98 80")).unwrap();
        g.play_uci("a1a2").unwrap();
        assert!(g.outcome().is_none());
        g.play_uci("e8d8").unwrap();
        assert_eq!(g.outcome().unwrap().status, Status::Draw);
    }

    #[test]
    fn insufficient_material() {
        let cases = [
            ("4k3/8/8/8/8/8/8/4K3 w - - 0 1", true),
            ("4k3/8/8/8/8/8/8/3NK3 w - - 0 1", true),
            ("4k3/8/8/8/8/8/8/3BK3 w - - 0 1", true),
            ("2b1k3/8/8/8/8/8/8/3BK3 w - - 0 1", true), // both bishops on light squares
            ("3bk3/8/8/8/8/8/8/3BK3 w - - 0 1", false), // opposite colours: a mate exists
            ("4k3/8/8/8/8/8/8/2NNK3 w - - 0 1", false),
            ("4k3/8/8/8/8/8/P7/4K3 w - - 0 1", false),
        ];
        for (fen, want) in cases {
            let g = Game::new(Some(fen)).unwrap();
            assert_eq!(g.insufficient_material(), want, "{fen}");
        }
    }

    #[test]
    fn cannot_win_after_a_flag() {
        let g = Game::new(Some("4k3/8/8/8/8/8/8/3NK3 w - - 0 1")).unwrap();
        assert!(g.cannot_win(Side::Black), "a bare king can't win");
        assert!(g.cannot_win(Side::White), "a knight against a bare king can't mate");
        let g = Game::new(Some("4k3/4p3/8/8/8/8/8/3NK3 w - - 0 1")).unwrap();
        assert!(!g.cannot_win(Side::White), "a knight against king and pawn can (helpmate)");
    }

    #[test]
    fn fen_positions() {
        assert!(Game::new(Some("not a fen")).is_err());
        let g = Game::new(Some("startpos")).unwrap();
        assert_eq!(g.initial_fen(), None);
        let g = Game::new(Some(crate::STARTPOS_FEN)).unwrap();
        assert_eq!(g.initial_fen(), None, "the standard FEN counts as the start");
        let g = Game::new(Some("4k3/8/8/8/8/8/8/R3K3 b - - 0 1")).unwrap();
        assert_eq!(g.first_to_move(), Side::Black);
        assert_eq!(g.turn(), Side::Black);
        // Mated already: refused.
        assert!(Game::new(Some("R3k3/8/4K3/8/8/8/8/8 b - - 0 1")).is_err());
    }

    #[test]
    fn undo_restores_history() {
        let mut g = play("e2e4 e7e5 g1f3");
        let fen_after_two = play("e2e4 e7e5").fen();
        g.undo(1);
        assert_eq!(g.ply(), 2);
        assert_eq!(g.fen(), fen_after_two);
        assert_eq!(g.sans(), ["e4", "e5"]);
        g.undo(5);
        assert_eq!(g.ply(), 0);
    }

    #[test]
    fn positions_and_moves_for_analysis() {
        let g = play("e2e4 e7e5");
        let pm = g.positions_and_moves();
        assert_eq!(pm.len(), 2);
        assert_eq!(pm[0], (crate::STARTPOS_FEN.to_owned(), "e2e4".to_owned()));
        assert_eq!(pm[1].1, "e7e5");
    }

    #[test]
    fn side_helpers() {
        assert_eq!(Side::at_ply(Side::White, 0), Side::White);
        assert_eq!(Side::at_ply(Side::White, 3), Side::Black);
        assert_eq!(Side::at_ply(Side::Black, 1), Side::White);
        assert_eq!(Side::from_name("black"), Some(Side::Black));
        assert_eq!(Side::from_name("x"), None);
        assert_eq!(serde_json::to_string(&Side::White).unwrap(), "\"white\"");
    }
}
