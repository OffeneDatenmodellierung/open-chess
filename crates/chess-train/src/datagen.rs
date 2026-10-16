//! Self-play games as training data for the evaluation network.
//!
//! A game starts with a few random plies for variety, then the engine
//! plays itself at a fixed node count. Quiet positions (not in check, best
//! move not a capture, no mate score) are recorded with the search score
//! and, once the game is over, its result. Records are written either as
//! text lines `FEN | score | result` (White's point of view) or in the
//! trainer's binary [`crate::bulletformat`].

use chess_analysis::search::{Limits, Score};
use chess_analysis::{history_hashes, std_uci};
use chess_core::{Game, Side};
use cozy_chess::{Board, Color};

use crate::bulletformat;
use crate::play::Player;
use crate::spsa::splitmix;

/// Settings for one self-play game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// Nodes per move.
    pub nodes: u64,
    /// Random plies at the start.
    pub random_plies: u32,
    /// Skip the first `n` plies after the random ones (they are mostly
    /// the opening book's job, and their labels are noisy).
    pub skip_plies: u32,
    /// Resign once one side has been at least this far ahead (White's
    /// point of view, centipawns) for `resign_moves` consecutive moves.
    pub resign_cp: i32,
    pub resign_moves: u32,
    /// Give up a game after this many plies, scoring it a draw.
    pub max_plies: usize,
    /// A random opening that leaves one side more than this far ahead
    /// (centipawns, by a quick search) is thrown away and another drawn,
    /// so the data isn't full of positions already decided by a blunder.
    pub max_start_cp: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            nodes: 5_000,
            random_plies: 8,
            skip_plies: 0,
            resign_cp: 2000,
            resign_moves: 6,
            max_plies: 400,
            max_start_cp: 300,
        }
    }
}

/// One recorded position: the board, the search score for White, and the
/// game's result for White (1.0, 0.5, 0.0).
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub board: Board,
    pub white_cp: i32,
    pub white_result: f32,
}

impl Record {
    /// The text line the trainers read.
    pub fn text(&self) -> String {
        bulletformat::text_line(&self.board, self.white_cp, self.white_result)
    }

    /// The binary record bullet reads directly.
    pub fn bullet(&self) -> [u8; bulletformat::SIZE] {
        bulletformat::encode(&self.board, self.white_cp, self.white_result)
    }
}

/// Play one game and return its records (empty if the random opening
/// ended the game).
///
/// # Panics
///
/// Only on an internal bug: the engine only plays legal moves.
pub fn play(player: &mut Player, settings: Settings, seed: u64) -> Vec<Record> {
    let mut rng = seed;
    let Some(mut g) = (0..8).find_map(|_| {
        rng = splitmix(rng);
        opening(player, settings, rng)
    }) else {
        return Vec::new();
    };
    let first_recorded = g.ply() + settings.skip_plies as usize;
    let mut records: Vec<(Board, i32)> = Vec::new();
    let mut decisive = 0;
    let mut result: Option<f32> = None;
    while g.outcome().is_none() && g.ply() < settings.max_plies {
        let b = g.board().clone();
        let r = player.searcher.search(
            &b,
            &history_hashes(&g),
            Limits { depth: 64, nodes: settings.nodes, multipv: 1, time_ms: None },
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
        decisive = if white_cp.abs() >= settings.resign_cp { decisive + 1 } else { 0 };
        if decisive >= settings.resign_moves {
            result = Some(if white_cp > 0 { 1.0 } else { 0.0 });
            break;
        }
        let quiet = b.checkers().is_empty()
            && b.color_on(mv.to).is_none()
            && matches!(line.score, Score::Cp { .. })
            && g.ply() >= first_recorded;
        if quiet {
            records.push((b.clone(), white_cp));
        }
        if g.play_uci(&std_uci(&b, mv)).is_err() {
            break;
        }
    }
    let white_result = result.unwrap_or_else(|| match g.outcome().and_then(|o| o.winner) {
        Some(Side::White) => 1.0,
        Some(Side::Black) => 0.0,
        None => 0.5,
    });
    records.into_iter().map(|(board, white_cp)| Record { board, white_cp, white_result }).collect()
}

/// A random opening that is still balanced, or `None`.
fn opening(player: &mut Player, settings: Settings, seed: u64) -> Option<Game> {
    let mut g = Game::new(None).expect("start");
    player.searcher.clear();
    let mut rng = seed;
    for _ in 0..settings.random_plies {
        let legal = g.legal_moves();
        if legal.is_empty() {
            return None;
        }
        rng = splitmix(rng);
        let m = legal[(rng % legal.len() as u64) as usize].clone();
        g.play_uci(&m).ok()?;
    }
    if g.outcome().is_some() {
        return None;
    }
    let b = g.board().clone();
    let r = player.searcher.search(
        &b,
        &history_hashes(&g),
        Limits { depth: 64, nodes: settings.nodes, multipv: 1, time_ms: None },
    );
    match r.lines.first().map(|l| l.score) {
        Some(Score::Cp { value }) if value.abs() <= settings.max_start_cp => Some(g),
        _ => None,
    }
}

/// One game's text lines (the format the training server's datagen jobs
/// upload).
pub fn game(player: &mut Player, nodes: u64, random_plies: u32, seed: u64) -> Vec<String> {
    let settings = Settings { nodes, random_plies, ..Settings::default() };
    play(player, settings, seed).iter().map(Record::text).collect()
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

    #[test]
    fn hopeless_openings_are_rejected() {
        let mut p = Player::new(SearchParams::default(), 1);
        // Nothing within 0 cp will be found in 8 tries, so no game is played.
        let none = play(&mut p, Settings { nodes: 200, max_start_cp: -1, ..Settings::default() }, 1);
        assert!(none.is_empty());
    }

    #[test]
    fn records_are_quiet_and_binary_is_32_bytes() {
        let mut p = Player::new(SearchParams::default(), 1);
        let records = play(&mut p, Settings { nodes: 300, skip_plies: 4, ..Settings::default() }, 7);
        assert!(!records.is_empty());
        for r in &records {
            assert!(r.board.checkers().is_empty());
            assert_eq!(r.bullet().len(), 32);
            assert!(r.white_cp.abs() < 20_000);
        }
    }
}
