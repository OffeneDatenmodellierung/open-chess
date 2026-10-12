//! Self-play games as training data for the evaluation network.
//!
//! Lines are `FEN | score | result`, White's point of view, quiet
//! positions only: the format the NNUE trainers read.

use chess_analysis::search::{Limits, Score};
use chess_analysis::{history_hashes, std_uci};
use chess_core::{Game, Side};
use cozy_chess::Color;

use crate::play::Player;
use crate::spsa::splitmix;

/// One game's lines.
///
/// # Panics
///
/// Only on an internal bug: the engine only plays legal moves.
pub fn game(player: &mut Player, nodes: u64, random_plies: u32, seed: u64) -> Vec<String> {
    let mut g = Game::new(None).expect("start");
    player.searcher.clear();
    let mut rng = seed;
    for _ in 0..random_plies {
        let legal = g.legal_moves();
        if legal.is_empty() {
            return Vec::new();
        }
        rng = splitmix(rng);
        let m = legal[(rng % legal.len() as u64) as usize].clone();
        if g.play_uci(&m).is_err() {
            return Vec::new();
        }
    }
    let mut records: Vec<(String, i32)> = Vec::new();
    let mut decisive = 0;
    let mut result: Option<f32> = None;
    while g.outcome().is_none() && g.ply() < 400 {
        let b = g.board().clone();
        let r = player.searcher.search(
            &b,
            &history_hashes(&g),
            Limits { depth: 64, nodes, multipv: 1, time_ms: None },
        );
        let Some(line) = r.lines.first() else { break };
        let mv = line.moves[0];
        let white = b.side_to_move() == Color::White;
        let stm_cp = match line.score {
            Score::Cp { value } => value,
            Score::Mate { moves } => {
                if moves > 0 {
                    20_000
                } else {
                    -20_000
                }
            }
        };
        let white_cp = if white { stm_cp } else { -stm_cp };
        decisive = if white_cp.abs() >= 2000 { decisive + 1 } else { 0 };
        if decisive >= 6 {
            result = Some(if white_cp > 0 { 1.0 } else { 0.0 });
            break;
        }
        let quiet =
            b.checkers().is_empty() && b.color_on(mv.to).is_none() && matches!(line.score, Score::Cp { .. });
        if quiet {
            records.push((format!("{b}"), white_cp));
        }
        if g.play_uci(&std_uci(&b, mv)).is_err() {
            break;
        }
    }
    let result = result.unwrap_or_else(|| match g.outcome().and_then(|o| o.winner) {
        Some(Side::White) => 1.0,
        Some(Side::Black) => 0.0,
        None => 0.5,
    });
    records.into_iter().map(|(fen, cp)| format!("{fen} | {cp} | {result:.1}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chess_analysis::SearchParams;

    #[test]
    fn lines_have_the_trainers_format() {
        let mut p = Player::new(SearchParams::default(), 1);
        let lines = game(&mut p, 300, 8, 42);
        assert!(!lines.is_empty());
        for l in &lines {
            let parts: Vec<&str> = l.split(" | ").collect();
            assert_eq!(parts.len(), 3, "{l}");
            assert!(cozy_chess::Board::from_fen(parts[0], false).is_ok());
            assert!(parts[1].parse::<i32>().is_ok());
            assert!(["0.0", "0.5", "1.0"].contains(&parts[2]));
        }
    }
}
