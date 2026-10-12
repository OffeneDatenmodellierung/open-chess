//! Playing the engine against itself in-process.

use chess_analysis::search::{Limits, Searcher};
use chess_analysis::{SearchParams, history_hashes, std_uci};
use chess_core::{Game, Side};
use serde::{Deserialize, Serialize};

/// How long each move may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Tc {
    /// A fixed node budget per move: the same on every machine, so results
    /// from a laptop and a server are comparable.
    Nodes { nodes: u64 },
    /// A clock: base and increment in milliseconds (machine speed matters).
    Clock { base_ms: u64, inc_ms: u64 },
}

impl std::fmt::Display for Tc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tc::Nodes { nodes } => write!(f, "{nodes} nodes/move"),
            Tc::Clock { base_ms, inc_ms } => {
                write!(f, "{}+{}s", *base_ms as f64 / 1000.0, *inc_ms as f64 / 1000.0)
            }
        }
    }
}

impl std::str::FromStr for Tc {
    type Err = String;
    /// `nodes=20000`, or `10+0.1` (seconds).
    fn from_str(s: &str) -> Result<Tc, String> {
        if let Some(n) = s.strip_prefix("nodes=") {
            return n.parse().map(|nodes| Tc::Nodes { nodes }).map_err(|_| format!("bad node count {n:?}"));
        }
        let (b, i) = s.split_once('+').unwrap_or((s, "0"));
        let secs = |v: &str| v.parse::<f64>().map_err(|_| format!("bad time {v:?}"));
        Ok(Tc::Clock { base_ms: (secs(b)? * 1000.0) as u64, inc_ms: (secs(i)? * 1000.0) as u64 })
    }
}

/// One side of a game: a searcher with its parameters.
pub struct Player {
    pub searcher: Searcher,
}

impl Player {
    /// A player with `params` and a hash table of about `hash_mb` MB.
    pub fn new(params: SearchParams, hash_mb: usize) -> Player {
        let entries = (hash_mb.max(1) << 20) / 32;
        let mut searcher = Searcher::new(entries);
        searcher.set_params(params);
        Player { searcher }
    }
}

/// The opening for game pair `index` of a job with `seed`: a book line's
/// first moves, plus up to two random legal moves for variety.
///
/// # Panics
///
/// Only on an internal bug: the book lines are legal.
pub fn opening(seed: u64, index: u64) -> Vec<String> {
    let book: Vec<_> = chess_analysis::openings::all().into_iter().filter(|o| o.moves.len() >= 4).collect();
    let mut rng = crate::spsa::splitmix(seed ^ index.wrapping_mul(0x2545_f491_4f6c_dd1d));
    let line = &book[(rng % book.len() as u64) as usize];
    let mut moves: Vec<String> = line.moves.iter().take(8).map(|m| (*m).to_string()).collect();
    let mut g = Game::from_moves(None, &moves.join(" ")).unwrap_or_else(|_| Game::new(None).expect("start"));
    rng = crate::spsa::splitmix(rng);
    for _ in 0..(rng % 3) {
        let legal = g.legal_moves();
        if legal.is_empty() {
            break;
        }
        rng = crate::spsa::splitmix(rng);
        let m = legal[(rng % legal.len() as u64) as usize].clone();
        if g.play_uci(&m).is_err() {
            break;
        }
        moves.push(m);
    }
    moves
}

/// Play one game; the score is White's (1, ½ or 0).
///
/// # Panics
///
/// Only on an internal bug: the engine only plays legal moves.
pub fn play_game(white: &mut Player, black: &mut Player, opening: &[String], tc: Tc) -> f64 {
    white.searcher.clear();
    black.searcher.clear();
    let mut g =
        Game::from_moves(None, &opening.join(" ")).unwrap_or_else(|_| Game::new(None).expect("start"));
    let (mut wt, mut bt) = match tc {
        Tc::Clock { base_ms, .. } => (base_ms as i64, base_ms as i64),
        Tc::Nodes { .. } => (0, 0),
    };
    // Adjudicate long, clearly won or dead-drawn games to save time.
    let (mut decisive, mut drawish) = (0, 0);
    while g.outcome().is_none() && g.ply() < 400 {
        let white_to_move = g.turn() == Side::White;
        let p = if white_to_move { &mut *white } else { &mut *black };
        let limits = match tc {
            Tc::Nodes { nodes } => Limits { depth: 64, nodes, multipv: 1, time_ms: None },
            Tc::Clock { inc_ms, .. } => {
                let left = if white_to_move { wt } else { bt }.max(1) as u64;
                let budget = (left / 25 + inc_ms / 2).min(left.saturating_sub(20)).max(5);
                Limits { depth: 64, nodes: u64::MAX / 2, multipv: 1, time_ms: Some(budget) }
            }
        };
        let board = g.board().clone();
        let started = std::time::Instant::now();
        let r = p.searcher.search(&board, &history_hashes(&g), limits);
        if let Tc::Clock { inc_ms, .. } = tc {
            let clock = if white_to_move { &mut wt } else { &mut bt };
            *clock -= started.elapsed().as_millis() as i64;
            if *clock < 0 {
                return if white_to_move { 0.0 } else { 1.0 };
            }
            *clock += inc_ms as i64;
        }
        let Some(line) = r.lines.first() else { break };
        let cp = line.score.as_cp();
        let white_cp = if white_to_move { cp } else { -cp };
        decisive = if white_cp.abs() >= 1000 { decisive + 1 } else { 0 };
        drawish = if white_cp.abs() <= 10 && g.ply() >= 80 { drawish + 1 } else { 0 };
        if decisive >= 8 {
            return if white_cp > 0 { 1.0 } else { 0.0 };
        }
        if drawish >= 16 {
            return 0.5;
        }
        if g.play_uci(&std_uci(&board, line.moves[0])).is_err() {
            break;
        }
    }
    match g.outcome().and_then(|o| o.winner) {
        Some(Side::White) => 1.0,
        Some(Side::Black) => 0.0,
        None => 0.5,
    }
}

/// Play a pair of games from one opening with colours swapped. Returns the
/// first player's points (0 to 2).
pub fn play_pair(a: &mut Player, b: &mut Player, opening: &[String], tc: Tc) -> f64 {
    let g1 = play_game(a, b, opening, tc);
    let g2 = 1.0 - play_game(b, a, opening, tc);
    g1 + g2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tc_parses() {
        assert_eq!("nodes=5000".parse::<Tc>().unwrap(), Tc::Nodes { nodes: 5000 });
        assert_eq!("10+0.1".parse::<Tc>().unwrap(), Tc::Clock { base_ms: 10_000, inc_ms: 100 });
        assert!("x".parse::<Tc>().is_err());
    }

    #[test]
    fn openings_are_legal_and_reproducible() {
        for i in 0..20 {
            let o = opening(7, i);
            assert_eq!(o, opening(7, i));
            assert!(Game::from_moves(None, &o.join(" ")).is_ok(), "{o:?}");
        }
        assert_ne!(opening(7, 0), opening(8, 0));
    }

    #[test]
    fn a_pair_finishes_with_a_valid_score() {
        let mut a = Player::new(SearchParams::default(), 1);
        let mut b = Player::new(SearchParams::default(), 1);
        let pts = play_pair(&mut a, &mut b, &opening(1, 0), Tc::Nodes { nodes: 300 });
        assert!((0.0..=2.0).contains(&pts));
    }
}
