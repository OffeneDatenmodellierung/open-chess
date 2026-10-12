//! Static evaluation, in centipawns from the side to move's point of view.
//!
//! Material plus piece-square tables (Tomasz Michniewski's "Simplified
//! Evaluation Function", public domain), with the king's table tapered
//! from middlegame to endgame by remaining material, a bishop-pair bonus
//! and simple pawn-structure terms. It is deliberately small and
//! readable: a starting point to tune against real games, not a rival to
//! Stockfish's network.

use cozy_chess::{BitBoard, Board, Color, File, Piece, Rank, Square};

use crate::util::value;

/// Tables are written as you'd print a board for White: rank 8 first.
#[rustfmt::skip]
const PAWN: [i32; 64] = [
     0,  0,  0,  0,  0,  0,  0,  0,
    50, 50, 50, 50, 50, 50, 50, 50,
    10, 10, 20, 30, 30, 20, 10, 10,
     5,  5, 10, 25, 25, 10,  5,  5,
     0,  0,  0, 20, 20,  0,  0,  0,
     5, -5,-10,  0,  0,-10, -5,  5,
     5, 10, 10,-20,-20, 10, 10,  5,
     0,  0,  0,  0,  0,  0,  0,  0,
];
#[rustfmt::skip]
const KNIGHT: [i32; 64] = [
    -50,-40,-30,-30,-30,-30,-40,-50,
    -40,-20,  0,  0,  0,  0,-20,-40,
    -30,  0, 10, 15, 15, 10,  0,-30,
    -30,  5, 15, 20, 20, 15,  5,-30,
    -30,  0, 15, 20, 20, 15,  0,-30,
    -30,  5, 10, 15, 15, 10,  5,-30,
    -40,-20,  0,  5,  5,  0,-20,-40,
    -50,-40,-30,-30,-30,-30,-40,-50,
];
#[rustfmt::skip]
const BISHOP: [i32; 64] = [
    -20,-10,-10,-10,-10,-10,-10,-20,
    -10,  0,  0,  0,  0,  0,  0,-10,
    -10,  0,  5, 10, 10,  5,  0,-10,
    -10,  5,  5, 10, 10,  5,  5,-10,
    -10,  0, 10, 10, 10, 10,  0,-10,
    -10, 10, 10, 10, 10, 10, 10,-10,
    -10,  5,  0,  0,  0,  0,  5,-10,
    -20,-10,-10,-10,-10,-10,-10,-20,
];
#[rustfmt::skip]
const ROOK: [i32; 64] = [
     0,  0,  0,  0,  0,  0,  0,  0,
     5, 10, 10, 10, 10, 10, 10,  5,
    -5,  0,  0,  0,  0,  0,  0, -5,
    -5,  0,  0,  0,  0,  0,  0, -5,
    -5,  0,  0,  0,  0,  0,  0, -5,
    -5,  0,  0,  0,  0,  0,  0, -5,
    -5,  0,  0,  0,  0,  0,  0, -5,
     0,  0,  0,  5,  5,  0,  0,  0,
];
#[rustfmt::skip]
const QUEEN: [i32; 64] = [
    -20,-10,-10, -5, -5,-10,-10,-20,
    -10,  0,  0,  0,  0,  0,  0,-10,
    -10,  0,  5,  5,  5,  5,  0,-10,
     -5,  0,  5,  5,  5,  5,  0, -5,
      0,  0,  5,  5,  5,  5,  0, -5,
    -10,  5,  5,  5,  5,  5,  0,-10,
    -10,  0,  5,  0,  0,  0,  0,-10,
    -20,-10,-10, -5, -5,-10,-10,-20,
];
#[rustfmt::skip]
const KING_MID: [i32; 64] = [
    -30,-40,-40,-50,-50,-40,-40,-30,
    -30,-40,-40,-50,-50,-40,-40,-30,
    -30,-40,-40,-50,-50,-40,-40,-30,
    -30,-40,-40,-50,-50,-40,-40,-30,
    -20,-30,-30,-40,-40,-30,-30,-20,
    -10,-20,-20,-20,-20,-20,-20,-10,
     20, 20,  0,  0,  0,  0, 20, 20,
     20, 30, 10,  0,  0, 10, 30, 20,
];
#[rustfmt::skip]
const KING_END: [i32; 64] = [
    -50,-40,-30,-20,-20,-30,-40,-50,
    -30,-20,-10,  0,  0,-10,-20,-30,
    -30,-10, 20, 30, 30, 20,-10,-30,
    -30,-10, 30, 40, 40, 30,-10,-30,
    -30,-10, 30, 40, 40, 30,-10,-30,
    -30,-10, 20, 30, 30, 20,-10,-30,
    -30,-30,  0,  0,  0,  0,-30,-30,
    -50,-30,-30,-30,-30,-30,-30,-50,
];

/// Passed-pawn bonus by rank from the pawn's own side (rank 1 … 8).
const PASSED: [i32; 8] = [0, 5, 10, 20, 35, 60, 100, 0];

/// Game phase weights; 24 is a full set of minor and major pieces.
const PHASE_MAX: i32 = 24;

/// Index into a table for a piece of `color` on `sq`.
fn table_index(sq: Square, color: Color) -> usize {
    let file = sq.file() as usize;
    let rank = sq.rank() as usize;
    match color {
        Color::White => (7 - rank) * 8 + file,
        Color::Black => rank * 8 + file,
    }
}

/// 24 with every piece on, 0 with only kings and pawns.
pub fn phase(board: &Board) -> i32 {
    let n = |p| board.pieces(p).len() as i32;
    (n(Piece::Knight) + n(Piece::Bishop) + 2 * n(Piece::Rook) + 4 * n(Piece::Queen)).min(PHASE_MAX)
}

fn file_mask(file: File) -> BitBoard {
    file.bitboard()
}

/// The squares in front of a pawn on its file and the neighbouring files.
fn passed_mask(sq: Square, color: Color) -> BitBoard {
    let mut files = file_mask(sq.file());
    if let Some(f) = File::try_index(sq.file() as usize + 1) {
        files |= file_mask(f);
    }
    if (sq.file() as usize) > 0 {
        files |= file_mask(File::index(sq.file() as usize - 1));
    }
    let rank = sq.rank() as usize;
    let mut ahead = BitBoard::EMPTY;
    for r in Rank::ALL {
        let r_i = r as usize;
        let in_front = match color {
            Color::White => r_i > rank,
            Color::Black => r_i < rank,
        };
        if in_front {
            ahead |= r.bitboard();
        }
    }
    files & ahead
}

fn side_score(board: &Board, color: Color, phase: i32) -> i32 {
    let mut mid = 0;
    let mut end = 0;
    for piece in Piece::ALL {
        let table = match piece {
            Piece::Pawn => &PAWN,
            Piece::Knight => &KNIGHT,
            Piece::Bishop => &BISHOP,
            Piece::Rook => &ROOK,
            Piece::Queen => &QUEEN,
            Piece::King => &KING_MID,
        };
        for sq in board.colored_pieces(color, piece) {
            let i = table_index(sq, color);
            if piece == Piece::King {
                mid += KING_MID[i];
                end += KING_END[i];
            } else {
                let v = value(piece) + table[i];
                mid += v;
                end += v;
            }
        }
    }
    if board.colored_pieces(color, Piece::Bishop).len() >= 2 {
        mid += 30;
        end += 40;
    }
    let ours = board.colored_pieces(color, Piece::Pawn);
    let theirs = board.colored_pieces(!color, Piece::Pawn);
    for sq in ours {
        if (passed_mask(sq, color) & theirs).is_empty() {
            let rel = sq.relative_to(color).rank() as usize;
            mid += PASSED[rel] / 2;
            end += PASSED[rel];
        }
    }
    for f in File::ALL {
        let on_file = ours & file_mask(f);
        let n = on_file.len() as i32;
        if n > 1 {
            mid -= 10 * (n - 1);
            end -= 20 * (n - 1);
        }
        // Isolated: no friendly pawn on either neighbouring file.
        if n > 0 && (ours & adjacent_files(f)).is_empty() {
            mid -= 10 * n;
            end -= 15 * n;
        }
    }
    let (m, e) = activity(board, color);
    mid += m;
    end += e;
    (mid * phase + end * (PHASE_MAX - phase)) / PHASE_MAX
}

fn adjacent_files(f: File) -> BitBoard {
    let i = f as usize;
    let mut b = BitBoard::EMPTY;
    if i > 0 {
        b |= file_mask(File::index(i - 1));
    }
    if i < 7 {
        b |= file_mask(File::index(i + 1));
    }
    b
}

/// Squares attacked by `color`'s pawns.
fn pawn_attacks(board: &Board, color: Color) -> BitBoard {
    let mut a = BitBoard::EMPTY;
    for sq in board.colored_pieces(color, Piece::Pawn) {
        a |= cozy_chess::get_pawn_attacks(sq, color);
    }
    a
}

/// Mobility, rooks on open files, king shelter and king-zone pressure,
/// as (middlegame, endgame) centipawns for `color`.
fn activity(board: &Board, color: Color) -> (i32, i32) {
    let occ = board.occupied();
    let own = board.colors(color);
    let enemy_pawn_attacks = pawn_attacks(board, !color);
    let safe = !own & !enemy_pawn_attacks;
    let their_king = board.king(!color);
    let zone = cozy_chess::get_king_moves(their_king) | their_king.bitboard();
    let (mut mid, mut end) = (0, 0);
    let (mut attackers, mut attack_weight) = (0, 0);
    for piece in [Piece::Knight, Piece::Bishop, Piece::Rook, Piece::Queen] {
        for sq in board.colored_pieces(color, piece) {
            let att = match piece {
                Piece::Knight => cozy_chess::get_knight_moves(sq),
                Piece::Bishop => cozy_chess::get_bishop_moves(sq, occ),
                Piece::Rook => cozy_chess::get_rook_moves(sq, occ),
                _ => cozy_chess::get_rook_moves(sq, occ) | cozy_chess::get_bishop_moves(sq, occ),
            };
            let m = (att & safe).len() as i32;
            let (centre, wm, we) = match piece {
                Piece::Knight => (4, 4, 4),
                Piece::Bishop => (6, 5, 5),
                Piece::Rook => (7, 2, 4),
                _ => (13, 1, 2),
            };
            mid += wm * (m - centre);
            end += we * (m - centre);
            let hits = (att & zone).len() as i32;
            if hits > 0 {
                attackers += 1;
                attack_weight += hits
                    * match piece {
                        Piece::Knight | Piece::Bishop => 2,
                        Piece::Rook => 3,
                        _ => 5,
                    };
            }
            if piece == Piece::Rook {
                let f = file_mask(sq.file());
                if (board.pieces(Piece::Pawn) & f).is_empty() {
                    mid += 40;
                    end += 15;
                } else if (board.colored_pieces(color, Piece::Pawn) & f).is_empty() {
                    mid += 20;
                    end += 8;
                }
            }
        }
    }
    // Pressure on the enemy king grows faster than linearly with the
    // number of attackers.
    if attackers >= 2 {
        mid += attack_weight * attack_weight * attackers / 12;
    }
    // Our own king's pawn shelter (middlegame only).
    let king = board.king(color);
    let shelter_files = file_mask(king.file()) | adjacent_files(king.file());
    let ahead = passed_mask(king, color) & shelter_files;
    let near = ahead
        & (0..2)
            .filter_map(|d| {
                let r = king.rank() as i32 + if color == Color::White { d + 1 } else { -(d + 1) };
                (0..8).contains(&r).then(|| Rank::index(r as usize).bitboard())
            })
            .fold(BitBoard::EMPTY, |a, b| a | b);
    mid += 12 * (board.colored_pieces(color, Piece::Pawn) & near).len() as i32;
    (mid, end)
}

/// Centipawns from White's point of view.
pub fn evaluate_white(board: &Board) -> i32 {
    let p = phase(board);
    side_score(board, Color::White, p) - side_score(board, Color::Black, p)
}

/// Centipawns from the side to move's point of view (what search wants).
pub fn evaluate(board: &Board) -> i32 {
    let w = evaluate_white(board);
    // A small bonus for having the move.
    match board.side_to_move() {
        Color::White => w + TEMPO,
        Color::Black => -w + TEMPO,
    }
}

/// The side to move's bonus, in centipawns.
pub const TEMPO: i32 = 15;

#[cfg(test)]
mod tests {
    use super::*;

    fn b(fen: &str) -> Board {
        Board::from_fen(fen, false).unwrap()
    }

    #[test]
    fn start_is_level() {
        assert_eq!(evaluate_white(&Board::default()), 0);
        assert_eq!(evaluate(&Board::default()), TEMPO);
    }

    #[test]
    fn symmetric_positions_score_zero_for_both_sides() {
        let fen = "r1bqkbnr/pppp1ppp/2n5/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R w KQkq - 2 3";
        // Not symmetric, but mirrored colours must give the negated score.
        let mirrored = "rnbqkb1r/pppp1ppp/5n2/4p3/4P3/2N5/PPPP1PPP/R1BQKBNR b KQkq - 2 3";
        assert_eq!(evaluate(&b(fen)), evaluate(&b(mirrored)));
    }

    #[test]
    fn extra_queen_is_winning() {
        assert!(evaluate_white(&b("4k3/8/8/8/8/8/8/3QK3 w - - 0 1")) > 800);
        assert!(evaluate(&b("4k3/8/8/8/8/8/8/3QK3 b - - 0 1")) < -800);
    }

    #[test]
    fn advanced_passed_pawn_is_worth_more() {
        let far = evaluate_white(&b("4k3/P7/8/8/8/8/8/4K3 w - - 0 1"));
        let near = evaluate_white(&b("4k3/8/8/8/8/8/P7/4K3 w - - 0 1"));
        assert!(far > near + 50, "{far} vs {near}");
    }
}
