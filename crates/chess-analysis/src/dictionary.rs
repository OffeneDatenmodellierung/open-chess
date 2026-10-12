//! Keys for the position dictionary (the chess server's `chess.positions`):
//! one key per position, however it was reached.

use chess_core::Game;
use cozy_chess::{Piece, Square};

use crate::photo::{Grid, complete_fen};

/// A position's key: the FEN without the move counters, with an en-passant
/// square only when an en-passant capture is actually possible, so the
/// same position is always one entry.
pub fn position_key(game: &Game) -> String {
    let fen = game.fen();
    let mut f: Vec<&str> = fen.split_whitespace().take(4).collect();
    if let Some(ep) = f.get(3).copied().filter(|e| *e != "-") {
        let board = game.board();
        let possible = game.legal_moves().iter().any(|m| {
            m.get(2..4) == Some(ep)
                && m.get(0..2).and_then(|s| s.parse::<Square>().ok()).and_then(|s| board.piece_on(s))
                    == Some(Piece::Pawn)
        });
        if !possible {
            f[3] = "-";
        }
    }
    f.join(" ")
}

/// The positions a board photo can be: a placement with either side to
/// move (or the one given), castling where kings and rooks stand at home.
/// Only legal positions (no side to move with the other king in check).
pub fn photo_positions(grid: &Grid, white_to_move: Option<bool>) -> Vec<Game> {
    let sides: &[bool] = match white_to_move {
        Some(true) => &[true],
        Some(false) => &[false],
        None => &[true, false],
    };
    sides.iter().filter_map(|&w| Game::new(Some(&complete_fen(grid, w))).ok()).collect()
}

/// A full FEN (counters added) for a key.
pub fn fen_of_key(key: &str) -> String {
    format!("{key} 0 1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::photo::parse_placement;

    #[test]
    fn key_drops_move_counters_and_idle_en_passant() {
        let typed = Game::new(Some("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1")).unwrap();
        let played = Game::from_moves(None, "e2e4").unwrap();
        assert_eq!(position_key(&typed), "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq -");
        assert_eq!(position_key(&played), position_key(&typed));
        // A real en-passant chance is part of the position.
        let ep = Game::from_moves(None, "e2e4 a7a6 e4e5 d7d5").unwrap();
        assert!(position_key(&ep).ends_with(" d6"), "{}", position_key(&ep));
        assert_eq!(
            position_key(&Game::new(Some(&fen_of_key(&position_key(&ep)))).unwrap()),
            position_key(&ep)
        );
    }

    #[test]
    fn a_photo_is_both_sides_to_move_unless_told() {
        let g = parse_placement(chess_core::STARTPOS_FEN).unwrap();
        let both: Vec<String> = photo_positions(&g, None).iter().map(position_key).collect();
        assert_eq!(
            both,
            vec![
                "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq -",
                "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR b KQkq -"
            ]
        );
        assert_eq!(photo_positions(&g, Some(false)).len(), 1);
        // White to move with Black in check can't be: only Black to move.
        let check = parse_placement("4k3/8/8/8/8/8/4R3/4K3").unwrap();
        let keys: Vec<String> = photo_positions(&check, None).iter().map(position_key).collect();
        assert_eq!(keys, vec!["4k3/8/8/8/8/8/4R3/4K3 b - -"]);
    }
}
