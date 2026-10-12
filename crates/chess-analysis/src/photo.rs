//! Turning a board read from a photo into a position.
//!
//! A vision model (or a person) reads the squares of a photographed board
//! as a FEN piece placement. Reading 64 squares from a photo is error
//! prone, so two things make it usable:
//!
//! - **Following a game**: when the previous position is known, the new
//!   photo can only show one of the positions a legal move leads to (about
//!   thirty), or the same position again. [`match_after_move`] scores every
//!   candidate by how many squares differ from what was read and returns
//!   them best first, so a misread square or two still finds the right
//!   move.
//! - **A first photo**: [`complete_fen`] turns the placement into a full
//!   FEN (side to move, castling rights inferred from kings and rooks on
//!   their home squares), and the app shows it in an editor to confirm.

use cozy_chess::{Board, Color, Piece, Square};
use serde::Serialize;

use crate::util::{after, legal, std_uci};

/// What is on each square (index = `Square as usize`, a1 = 0).
pub type Grid = [Option<(Piece, Color)>; 64];

/// Parse the piece-placement field of a FEN (the first field; anything
/// after a space is ignored). `None` unless it has exactly eight ranks of
/// eight squares.
pub fn parse_placement(fen: &str) -> Option<Grid> {
    let placement = fen.split_whitespace().next()?;
    let ranks: Vec<&str> = placement.split('/').collect();
    if ranks.len() != 8 {
        return None;
    }
    let mut grid: Grid = [None; 64];
    for (i, rank) in ranks.iter().enumerate() {
        let r = 7 - i;
        let mut f = 0;
        for c in rank.chars() {
            if let Some(d) = c.to_digit(10) {
                f += d as usize;
            } else {
                let piece = match c.to_ascii_lowercase() {
                    'p' => Piece::Pawn,
                    'n' => Piece::Knight,
                    'b' => Piece::Bishop,
                    'r' => Piece::Rook,
                    'q' => Piece::Queen,
                    'k' => Piece::King,
                    _ => return None,
                };
                let color = if c.is_ascii_uppercase() { Color::White } else { Color::Black };
                if f >= 8 {
                    return None;
                }
                grid[r * 8 + f] = Some((piece, color));
                f += 1;
            }
            if f > 8 {
                return None;
            }
        }
        if f != 8 {
            return None;
        }
    }
    Some(grid)
}

/// The grid of a board.
pub fn grid_of(board: &Board) -> Grid {
    let mut g: Grid = [None; 64];
    for sq in board.occupied() {
        if let (Some(p), Some(c)) = (board.piece_on(sq), board.color_on(sq)) {
            g[sq as usize] = Some((p, c));
        }
    }
    g
}

/// The placement field for a grid.
pub fn placement(grid: &Grid) -> String {
    let mut out = String::new();
    for r in (0..8).rev() {
        let mut empty = 0;
        for f in 0..8 {
            match grid[r * 8 + f] {
                None => empty += 1,
                Some((p, c)) => {
                    if empty > 0 {
                        out.push_str(&empty.to_string());
                        empty = 0;
                    }
                    let ch = match p {
                        Piece::Pawn => 'p',
                        Piece::Knight => 'n',
                        Piece::Bishop => 'b',
                        Piece::Rook => 'r',
                        Piece::Queen => 'q',
                        Piece::King => 'k',
                    };
                    out.push(if c == Color::White { ch.to_ascii_uppercase() } else { ch });
                }
            }
        }
        if empty > 0 {
            out.push_str(&empty.to_string());
        }
        if r > 0 {
            out.push('/');
        }
    }
    out
}

/// Squares that differ between two grids.
pub fn distance(a: &Grid, b: &Grid) -> usize {
    a.iter().zip(b).filter(|(x, y)| x != y).count()
}

/// The grid seen from the other side (a photo taken from Black's side,
/// read as if from White's).
pub fn rotated(g: &Grid) -> Grid {
    let mut out: Grid = [None; 64];
    for (i, v) in g.iter().enumerate() {
        out[63 - i] = *v;
    }
    out
}

/// A position the photo might show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    /// The move that leads to it (standard UCI); empty for "no move yet".
    pub uci: String,
    pub san: String,
    pub fen: String,
    /// Squares that differ from what was read (0 = exact).
    pub distance: usize,
}

/// Every position reachable from `board` by one legal move, plus the
/// position itself, scored against what was read from the photo, best
/// first. The photo may have been taken from either side; the better
/// orientation is used for each candidate.
pub fn match_after_move(board: &Board, seen: &Grid) -> Vec<Candidate> {
    let flipped = rotated(seen);
    let score = |g: &Grid| distance(g, seen).min(distance(g, &flipped));
    let mut out = vec![Candidate {
        uci: String::new(),
        san: String::new(),
        fen: format!("{board}"),
        distance: score(&grid_of(board)),
    }];
    for mv in legal(board) {
        let next = after(board, mv);
        out.push(Candidate {
            uci: std_uci(board, mv),
            san: chess_core::san(board, mv),
            fen: format!("{next}"),
            distance: score(&grid_of(&next)),
        });
    }
    out.sort_by_key(|c| c.distance);
    out
}

/// The classes a board-reading network gives each square, in output order:
/// empty, then White's pawn, knight, bishop, rook, queen, king, then
/// Black's in the same order. FEN letters, `'.'` for empty.
pub const CLASSES: [char; 13] = ['.', 'P', 'N', 'B', 'R', 'Q', 'K', 'p', 'n', 'b', 'r', 'q', 'k'];

/// For each square (a1 = 0, White's point of view), the probability of
/// each of the [`CLASSES`].
pub type SquareProbs = [[f32; 13]; 64];

const PIECES: [Piece; 6] =
    [Piece::Pawn, Piece::Knight, Piece::Bishop, Piece::Rook, Piece::Queen, Piece::King];

/// The class index of what stands on a square.
pub fn class_of(cell: Option<(Piece, Color)>) -> usize {
    match cell {
        None => 0,
        Some((p, c)) => {
            let i = PIECES.iter().position(|&x| x == p).unwrap_or(0) + 1;
            if c == Color::White { i } else { i + 6 }
        }
    }
}

/// What a class index stands for (`None` for empty or out of range).
pub fn cell_of(class: usize) -> Option<(Piece, Color)> {
    match class {
        1..=6 => Some((PIECES[class - 1], Color::White)),
        7..=12 => Some((PIECES[class - 7], Color::Black)),
        _ => None,
    }
}

/// The most likely thing on each square.
pub fn argmax_grid(probs: &SquareProbs) -> Grid {
    let mut g: Grid = [None; 64];
    for (sq, p) in probs.iter().enumerate() {
        let best = p.iter().enumerate().fold(0, |b, (i, &v)| if v > p[b] { i } else { b });
        g[sq] = cell_of(best);
    }
    g
}

/// No square's probability counts as lower than this, so one confident
/// misread costs a bounded amount rather than ruling a position out.
const FLOOR: f64 = 1e-4;

/// How likely the network's reading makes a grid: the sum over squares of
/// the log probability it gave what the grid has there.
pub fn log_likelihood(grid: &Grid, probs: &SquareProbs) -> f64 {
    grid.iter().zip(probs).map(|(cell, p)| f64::from(p[class_of(*cell)]).max(FLOOR).ln()).sum()
}

/// The probabilities seen from the other side of the board.
pub fn rotated_probs(probs: &SquareProbs) -> SquareProbs {
    let mut out = [[0.0; 13]; 64];
    for (i, p) in probs.iter().enumerate() {
        out[63 - i] = *p;
    }
    out
}

/// A position the photo might show, scored with the network's
/// per-square probabilities.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScoredCandidate {
    /// The move that leads to it (standard UCI); empty for "no move yet".
    pub uci: String,
    pub san: String,
    pub fen: String,
    /// Squares where the network's most likely reading differs.
    pub distance: usize,
    /// Log likelihood of this position under the reading.
    pub log_likelihood: f64,
    /// Its share of the probability across all the candidates (they
    /// add up to 1): how sure the reading is that this is the move.
    pub probability: f64,
}

/// [`match_after_move`] for a network that gives probabilities: every
/// position one legal move away (and the position itself), most likely
/// first. Uses whichever board orientation explains the reading better,
/// in case the network read the board upside down.
pub fn match_after_move_probs(board: &Board, probs: &SquareProbs) -> Vec<ScoredCandidate> {
    let flipped = rotated_probs(probs);
    let seen = argmax_grid(probs);
    let seen_flipped = rotated(&seen);
    let use_flipped = log_likelihood(&grid_of(board), &flipped) > log_likelihood(&grid_of(board), probs);
    let (p, s) = if use_flipped { (&flipped, &seen_flipped) } else { (probs, &seen) };
    let mut out = Vec::new();
    let mut push = |uci: String, san: String, next: &Board| {
        let g = grid_of(next);
        out.push(ScoredCandidate {
            uci,
            san,
            fen: format!("{next}"),
            distance: distance(&g, s),
            log_likelihood: log_likelihood(&g, p),
            probability: 0.0,
        });
    };
    push(String::new(), String::new(), board);
    for mv in legal(board) {
        let next = after(board, mv);
        push(std_uci(board, mv), chess_core::san(board, mv), &next);
    }
    out.sort_by(|a, b| b.log_likelihood.total_cmp(&a.log_likelihood));
    let top = out.first().map_or(0.0, |c| c.log_likelihood);
    let total: f64 = out.iter().map(|c| (c.log_likelihood - top).exp()).sum();
    for c in &mut out {
        c.probability = (c.log_likelihood - top).exp() / total;
    }
    out
}

/// A full FEN for a placement read from a photo: the given side to move,
/// castling rights wherever king and rook stand on their home squares (but
/// none in endings of seven pieces or fewer, where castling rights are
/// almost never still there and would keep the tablebases from answering),
/// no en-passant square.
pub fn complete_fen(grid: &Grid, white_to_move: bool) -> String {
    let at = |sq: Square| grid[sq as usize];
    let mut castling = String::new();
    let ending = grid.iter().flatten().count() <= 7;
    let wk = at(Square::E1) == Some((Piece::King, Color::White));
    let bk = at(Square::E8) == Some((Piece::King, Color::Black));
    if wk && at(Square::H1) == Some((Piece::Rook, Color::White)) {
        castling.push('K');
    }
    if wk && at(Square::A1) == Some((Piece::Rook, Color::White)) {
        castling.push('Q');
    }
    if bk && at(Square::H8) == Some((Piece::Rook, Color::Black)) {
        castling.push('k');
    }
    if bk && at(Square::A8) == Some((Piece::Rook, Color::Black)) {
        castling.push('q');
    }
    if castling.is_empty() || ending {
        castling = "-".into();
    }
    format!("{} {} {castling} - 0 1", placement(grid), if white_to_move { 'w' } else { 'b' })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_round_trip() {
        let g = parse_placement(chess_core::STARTPOS_FEN).unwrap();
        assert_eq!(placement(&g), "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR");
        assert!(parse_placement("8/8/8").is_none());
        assert!(parse_placement("9/8/8/8/8/8/8/8").is_none());
        assert!(parse_placement("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNX").is_none());
    }

    #[test]
    fn finds_the_move_despite_misreads() {
        let board = Board::default();
        // After 1.Nf3, read with two squares wrong (a pawn missed, a
        // phantom piece on h5).
        let mut seen = parse_placement("rnbqkbnr/pppppppp/8/7p/8/5N2/PPPPPPP1/RNBQKB1R").unwrap();
        seen[Square::A2 as usize] = None;
        let c = match_after_move(&board, &seen);
        assert_eq!(c[0].uci, "g1f3", "{:?}", &c[..3]);
        assert_eq!(c[0].san, "Nf3");
        assert!(c[0].distance <= 4);
        assert!(c[1].distance > c[0].distance);
    }

    #[test]
    fn photo_from_blacks_side_and_no_move() {
        let board = Board::default();
        let after_e4 = parse_placement("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR").unwrap();
        assert_eq!(match_after_move(&board, &rotated(&after_e4))[0].uci, "e2e4");
        let same = grid_of(&board);
        let c = match_after_move(&board, &same);
        assert_eq!((c[0].uci.as_str(), c[0].distance), ("", 0));
    }

    /// A network reading that is `sure` of `grid` on every square.
    fn reading(grid: &Grid, sure: f32) -> SquareProbs {
        let mut p = [[(1.0 - sure) / 12.0; 13]; 64];
        for (sq, cell) in grid.iter().enumerate() {
            p[sq][class_of(*cell)] = sure;
        }
        p
    }

    #[test]
    fn classes_round_trip() {
        for (i, &want) in CLASSES.iter().enumerate() {
            assert_eq!(class_of(cell_of(i)), i);
            let letter = cell_of(i).map_or('.', |(p, c)| {
                let ch = p.to_string().chars().next().unwrap();
                if c == Color::White { ch.to_ascii_uppercase() } else { ch }
            });
            assert_eq!(letter, want);
        }
        let g = parse_placement(chess_core::STARTPOS_FEN).unwrap();
        assert_eq!(argmax_grid(&reading(&g, 0.9)), g);
    }

    #[test]
    fn probabilities_find_the_move_and_how_sure() {
        let board = Board::default();
        let after_nf3 = parse_placement("rnbqkbnr/pppppppp/8/8/8/5N2/PPPPPPPP/RNBQKB1R").unwrap();
        let mut p = reading(&after_nf3, 0.95);
        // The network wasn't sure about f3: it thought it might be empty.
        p[Square::F3 as usize] = [0.45, 0.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.05];
        let c = match_after_move_probs(&board, &p);
        assert_eq!(c[0].san, "Nf3");
        assert!(c[0].probability > 0.9, "{:?}", &c[..2]);
        let sum: f64 = c.iter().map(|x| x.probability).sum();
        assert!((sum - 1.0).abs() < 1e-9);
        // Read upside down, the same move still comes out on top.
        let c = match_after_move_probs(&board, &rotated_probs(&p));
        assert_eq!(c[0].san, "Nf3");
    }

    #[test]
    fn complete_fen_infers_castling() {
        let g = parse_placement("r3k3/pppp4/8/8/8/8/PPPP4/4K2R").unwrap();
        assert_eq!(complete_fen(&g, false), "r3k3/pppp4/8/8/8/8/PPPP4/4K2R b Kq - 0 1");
        assert!(chess_core::Game::new(Some(&complete_fen(&g, true))).is_ok());
        // An ending: no castling rights, so the tablebases can answer.
        let e = parse_placement("8/8/8/4k3/8/8/8/4K2R").unwrap();
        assert_eq!(complete_fen(&e, true), "8/8/8/4k3/8/8/8/4K2R w - - 0 1");
    }
}
