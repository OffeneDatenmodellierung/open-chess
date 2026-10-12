//! Standard Algebraic Notation for a legal move.

use cozy_chess::{Board, Move, Piece};

fn letter(p: Piece) -> &'static str {
    match p {
        Piece::Pawn => "",
        Piece::Knight => "N",
        Piece::Bishop => "B",
        Piece::Rook => "R",
        Piece::Queen => "Q",
        Piece::King => "K",
    }
}

/// SAN for `mv`, which must be legal on `board` (cozy-chess encoding:
/// castling is king-takes-rook).
pub fn san(board: &Board, mv: Move) -> String {
    let us = board.side_to_move();
    let Some(piece) = board.piece_on(mv.from) else {
        return mv.to_string();
    };
    let mut out = String::new();
    if piece == Piece::King && board.color_on(mv.to) == Some(us) {
        out.push_str(if mv.to.file() > mv.from.file() { "O-O" } else { "O-O-O" });
    } else {
        let capture = board.color_on(mv.to).is_some_and(|c| c != us)
            || (piece == Piece::Pawn && mv.from.file() != mv.to.file());
        if piece == Piece::Pawn {
            if capture {
                out.push_str(&mv.from.file().to_string());
            }
        } else {
            out.push_str(letter(piece));
            out.push_str(&disambiguation(board, mv, piece));
        }
        if capture {
            out.push('x');
        }
        out.push_str(&mv.to.to_string());
        if let Some(p) = mv.promotion {
            out.push('=');
            out.push_str(letter(p));
        }
    }
    let mut after = board.clone();
    after.play_unchecked(mv);
    if !after.checkers().is_empty() {
        out.push(if after.generate_moves(|_| true) { '+' } else { '#' });
    }
    out
}

/// File, rank or both, when another piece of the same kind could also
/// reach the target square.
fn disambiguation(board: &Board, mv: Move, piece: Piece) -> String {
    let mut others = Vec::new();
    board.generate_moves(|pm| {
        if pm.piece == piece {
            for m in pm {
                if m.to == mv.to && m.from != mv.from {
                    others.push(m.from);
                }
            }
        }
        false
    });
    if others.is_empty() {
        return String::new();
    }
    let same_file = others.iter().any(|s| s.file() == mv.from.file());
    let same_rank = others.iter().any(|s| s.rank() == mv.from.rank());
    match (same_file, same_rank) {
        (false, _) => mv.from.file().to_string(),
        (true, false) => mv.from.rank().to_string(),
        (true, true) => mv.from.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn san_of(fen: &str, uci: &str) -> String {
        let b = Board::from_fen(fen, false).unwrap();
        san(&b, uci.parse().unwrap())
    }

    #[test]
    fn basic_moves() {
        let start = crate::STARTPOS_FEN;
        assert_eq!(san_of(start, "e2e4"), "e4");
        assert_eq!(san_of(start, "g1f3"), "Nf3");
    }

    #[test]
    fn disambiguation_by_file_rank_and_square() {
        // Rooks on a1 and h1, both reaching d1: file.
        assert_eq!(san_of("4k3/8/8/8/8/8/4K3/R6R w - - 0 1", "a1d1"), "Rad1");
        // Rooks on a1 and a5 to a3: same file → rank.
        assert_eq!(san_of("4k3/8/8/R7/8/8/8/R3K3 w - - 0 1", "a1a3"), "R1a3");
        // Queens on a4, a2 and c4 all reach b3: from a4 needs file and rank.
        assert_eq!(san_of("7k/8/8/8/Q1Q5/8/Q7/4K3 w - - 0 1", "a4b3"), "Qa4b3");
    }

    #[test]
    fn captures_checks_and_mate() {
        assert_eq!(san_of("4k3/8/8/3p4/4P3/8/8/4K3 w - - 0 1", "e4d5"), "exd5");
        assert_eq!(san_of("4k3/8/8/8/8/8/8/R3K3 w - - 0 1", "a1a8"), "Ra8+");
        assert_eq!(san_of("6k1/5ppp/8/8/8/8/8/R5K1 w - - 0 1", "a1a8"), "Ra8#");
        assert_eq!(san_of("4k3/8/8/8/8/8/8/R3K3 w - - 0 1", "a1a7"), "Ra7");
    }

    #[test]
    fn castling_and_promotion() {
        assert_eq!(san_of("4k3/8/8/8/8/8/8/R3K2R w KQ - 0 1", "e1h1"), "O-O");
        assert_eq!(san_of("4k3/8/8/8/8/8/8/R3K2R w KQ - 0 1", "e1a1"), "O-O-O");
        assert_eq!(san_of("3r3k/2P5/8/8/8/8/8/4K3 w - - 0 1", "c7d8n"), "cxd8=N");
        assert_eq!(san_of("7k/P7/8/8/8/8/8/4K3 w - - 0 1", "a7a8q"), "a8=Q+");
    }
}
