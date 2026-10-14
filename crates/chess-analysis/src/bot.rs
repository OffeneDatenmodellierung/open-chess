//! A playing bot with strength levels.
//!
//! Each level is a search budget plus a margin: the bot looks at several
//! candidate lines and picks at random among those within the margin of
//! the best, so weaker levels make plausible human-looking slips rather
//! than random blunders. Randomness comes from a caller-supplied seed, so
//! a game can be replayed exactly (useful when bot games feed training).

use chess_core::Game;
use serde::Serialize;

use crate::search::{Limits, Searcher};
use crate::util::{history_hashes, std_uci};

/// Bot strength, 1 (beginner) to 8 (strongest this search offers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level(u8);

impl Level {
    pub fn new(n: u8) -> Level {
        Level(n.clamp(1, 8))
    }

    pub fn get(self) -> u8 {
        self.0
    }

    /// The level's fixed rating, roughly what it plays like. The server's
    /// `ai-level-N` accounts and the iOS app's offline games both use it.
    pub fn rating(self) -> i32 {
        [800, 1000, 1200, 1400, 1600, 1800, 2000, 2200][usize::from(self.0 - 1)]
    }

    fn settings(self) -> (Limits, i32) {
        let (depth, nodes, lines, margin) = match self.0 {
            1 => (1, 2_000, 6, 300),
            2 => (2, 8_000, 5, 200),
            3 => (3, 20_000, 4, 120),
            4 => (4, 60_000, 3, 70),
            5 => (5, 150_000, 3, 40),
            6 => (6, 400_000, 2, 20),
            7 => (8, 1_000_000, 1, 0),
            _ => (12, 3_000_000, 1, 0),
        };
        (Limits { depth, nodes, multipv: lines, time_ms: None }, margin)
    }
}

/// The bot's choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BotMove {
    pub uci: String,
    pub san: String,
}

/// Full strength: the best move found in `time_ms` milliseconds (or to
/// `depth`, whichever comes first), with no deliberate slips. This is the
/// "max" level; give it more time to make it stronger.
pub fn best_move(searcher: &mut Searcher, game: &Game, time_ms: u64, depth: u8) -> Option<BotMove> {
    if game.outcome().is_some() {
        return None;
    }
    let board = game.board();
    let limits =
        Limits { depth: depth.max(1), nodes: u64::MAX / 2, multipv: 1, time_ms: Some(time_ms.max(10)) };
    let result = searcher.search(board, &history_hashes(game), limits);
    let mv = *result.lines.first()?.moves.first()?;
    Some(BotMove { uci: std_uci(board, mv), san: chess_core::san(board, mv) })
}

/// Choose a move for the side to move, or None if the game is over.
pub fn bot_move(searcher: &mut Searcher, game: &Game, level: Level, seed: u64) -> Option<BotMove> {
    if game.outcome().is_some() {
        return None;
    }
    let board = game.board();
    let (limits, margin) = level.settings();
    let result = searcher.search(board, &history_hashes(game), limits);
    let best = result.lines.first()?.score.as_cp();
    let pool: Vec<_> = result.lines.iter().filter(|l| l.score.as_cp() >= best - margin).collect();
    let pick = pool[(splitmix(seed) % pool.len() as u64) as usize];
    let mv = *pick.moves.first()?;
    Some(BotMove { uci: std_uci(board, mv), san: chess_core::san(board, mv) })
}

fn splitmix(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_level_plays_a_legal_move() {
        let g = Game::new(None).unwrap();
        for n in 1..=6 {
            let m = bot_move(&mut Searcher::new(1 << 14), &g, Level::new(n), u64::from(n)).unwrap();
            assert!(g.legal_moves().contains(&m.uci), "{m:?}");
        }
    }

    #[test]
    fn strong_level_takes_mate() {
        let g = Game::new(Some("6k1/5ppp/8/8/8/8/8/R5K1 w - - 0 1")).unwrap();
        let m = bot_move(&mut Searcher::new(1 << 14), &g, Level::new(5), 7).unwrap();
        assert_eq!(m.san, "Ra8#");
    }

    #[test]
    fn max_level_uses_its_time() {
        let g = Game::new(None).unwrap();
        let mut s = Searcher::new(1 << 16);
        let t = std::time::Instant::now();
        let m = best_move(&mut s, &g, 300, 64).unwrap();
        let ms = t.elapsed().as_millis();
        assert!(g.legal_moves().contains(&m.uci));
        assert!((100..=1500).contains(&ms), "{ms} ms");
    }

    #[test]
    fn same_seed_same_move() {
        let g = Game::new(None).unwrap();
        let a = bot_move(&mut Searcher::new(1 << 14), &g, Level::new(2), 42);
        let b = bot_move(&mut Searcher::new(1 << 14), &g, Level::new(2), 42);
        assert_eq!(a, b);
        assert!(
            bot_move(
                &mut Searcher::default(),
                &Game::from_moves(None, "f2f3 e7e5 g2g4 d8h4").unwrap(),
                Level::new(3),
                1
            )
            .is_none()
        );
    }
}
