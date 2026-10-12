//! Following a game through a stream of readings: frames of a video, or
//! photos taken after each move.
//!
//! Each reading is weighed against every position one legal move away
//! (two, if a frame was missed), so a frame only has to make one move more
//! likely than the others, not read all 64 squares right. A move is taken
//! once the same one has been the clear favourite for a few frames in a
//! row, so a hand over the board, a camera cut or a replay doesn't play a
//! move that didn't happen.

use chess_analysis::photo::{ScoredCandidate, SquareProbs, match_after_move_probs};
use cozy_chess::Board;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackerConfig {
    /// A candidate counts as the favourite at this share of the
    /// probability across all candidates.
    pub accept: f64,
    /// Frames in a row the same move must be the favourite.
    pub stable_frames: usize,
    /// Frames in a row that no position near the game explains (the
    /// favourite has more than `far` squares wrong) before the tracker
    /// says it has lost the game.
    pub lost_after: usize,
    pub far: usize,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self { accept: 0.9, stable_frames: 3, lost_after: 30, far: 10 }
    }
}

/// A move the tracker took.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrackedMove {
    pub uci: String,
    pub san: String,
    /// The position after it.
    pub fen: String,
}

/// What one reading changed.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Update {
    /// The board still shows the current position.
    Unchanged,
    /// A move (or two, if a frame between them was missed) is the favourite
    /// but hasn't been for long enough yet.
    Pending { moves: Vec<String>, frames: usize },
    /// Moves were played.
    Moved { moves: Vec<TrackedMove> },
    /// No position stands out (the board is covered, out of shot, or
    /// blurred). `best` is the likeliest anyway.
    Unsure { best: Option<String>, probability: f64 },
    /// For `lost_after` frames nothing near the game explains what's seen:
    /// a different game, or the tracker missed moves. Start again from a
    /// reading of the whole board.
    Lost,
}

/// Follows one game.
#[derive(Debug, Clone)]
pub struct GameTracker {
    board: Board,
    cfg: TrackerConfig,
    pending: Option<(Vec<TrackedMove>, usize)>,
    unexplained: usize,
}

impl GameTracker {
    pub fn new(board: Board, cfg: TrackerConfig) -> Self {
        Self { board, cfg, pending: None, unexplained: 0 }
    }

    /// The position the tracker believes the board is in.
    pub fn board(&self) -> &Board {
        &self.board
    }

    /// Take one reading.
    pub fn observe(&mut self, probs: &SquareProbs) -> Update {
        let one = match_after_move_probs(&self.board, probs);
        let Some(top) = one.first() else { return Update::Lost };
        let mut best: (Vec<TrackedMove>, f64, usize) = (Self::moves_of(top), top.probability, top.distance);
        // Nothing one move away fits well (a misread square is common; two
        // aren't): maybe a frame between two moves was missed.
        if (top.probability < self.cfg.accept || top.distance >= 2)
            && let Some(two) = Self::two_plies(&one, probs)
        {
            best = two;
        }
        let (moves, probability, distance) = best;
        if distance > self.cfg.far {
            self.pending = None;
            self.unexplained += 1;
            if self.unexplained >= self.cfg.lost_after {
                self.unexplained = 0;
                return Update::Lost;
            }
            return Update::Unsure { best: moves.first().map(|m| m.uci.clone()), probability };
        }
        self.unexplained = 0;
        if probability < self.cfg.accept {
            self.pending = None;
            return Update::Unsure { best: moves.first().map(|m| m.uci.clone()), probability };
        }
        if moves.is_empty() {
            self.pending = None;
            return Update::Unchanged;
        }
        let frames = match &mut self.pending {
            Some((p, n)) if *p == moves => {
                *n += 1;
                *n
            }
            _ => {
                self.pending = Some((moves.clone(), 1));
                1
            }
        };
        if frames < self.cfg.stable_frames {
            return Update::Pending { moves: moves.iter().map(|m| m.uci.clone()).collect(), frames };
        }
        self.pending = None;
        if let Some(b) = moves.last().and_then(|m| m.fen.parse::<Board>().ok()) {
            self.board = b;
        }
        Update::Moved { moves }
    }

    fn moves_of(c: &ScoredCandidate) -> Vec<TrackedMove> {
        if c.uci.is_empty() {
            Vec::new()
        } else {
            vec![TrackedMove { uci: c.uci.clone(), san: c.san.clone(), fen: c.fen.clone() }]
        }
    }

    /// The likeliest position exactly two moves on, with its share of the
    /// probability across all two-move positions and its distance.
    fn two_plies(first: &[ScoredCandidate], probs: &SquareProbs) -> Option<(Vec<TrackedMove>, f64, usize)> {
        let mut all: Vec<(Vec<TrackedMove>, f64, usize)> = Vec::new();
        for c1 in first.iter().filter(|c| !c.uci.is_empty()) {
            let Ok(b1) = c1.fen.parse::<Board>() else { continue };
            for c2 in match_after_move_probs(&b1, probs).into_iter().filter(|c| !c.uci.is_empty()) {
                let mut m = Self::moves_of(c1);
                m.extend(Self::moves_of(&c2));
                all.push((m, c2.log_likelihood, c2.distance));
            }
        }
        // Keep the one-move and no-move candidates in the running, so two
        // moves only win when they explain the reading better.
        for c in first {
            all.push((Self::moves_of(c), c.log_likelihood, c.distance));
        }
        let top = all.iter().map(|a| a.1).fold(f64::NEG_INFINITY, f64::max);
        let total: f64 = all.iter().map(|a| (a.1 - top).exp()).sum();
        let (moves, ll, distance) = all.into_iter().max_by(|a, b| a.1.total_cmp(&b.1))?;
        Some((moves, (ll - top).exp() / total, distance))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chess_analysis::photo::{Grid, class_of, grid_of, parse_placement};

    fn sure(grid: &Grid) -> SquareProbs {
        let mut p = [[0.005; 13]; 64];
        for (sq, cell) in grid.iter().enumerate() {
            p[sq][class_of(*cell)] = 0.94;
        }
        p
    }

    fn after(board: &Board, moves: &str) -> Board {
        let g = chess_core::Game::from_moves(Some(&format!("{board}")), moves).unwrap();
        g.board().clone()
    }

    #[test]
    fn takes_a_move_once_it_holds() {
        let mut t = GameTracker::new(Board::default(), TrackerConfig::default());
        assert_eq!(t.observe(&sure(&grid_of(&Board::default()))), Update::Unchanged);
        let e4 = sure(&grid_of(&after(&Board::default(), "e2e4")));
        assert!(matches!(t.observe(&e4), Update::Pending { frames: 1, .. }));
        assert!(matches!(t.observe(&e4), Update::Pending { frames: 2, .. }));
        match t.observe(&e4) {
            Update::Moved { moves } => assert_eq!(moves[0].san, "e4"),
            u => panic!("{u:?}"),
        }
        assert_eq!(t.observe(&e4), Update::Unchanged);
    }

    #[test]
    fn a_covered_board_plays_nothing() {
        let mut t = GameTracker::new(Board::default(), TrackerConfig::default());
        // A hand over the centre: those squares could be anything.
        let mut p = sure(&grid_of(&Board::default()));
        for sq in [27, 28, 35, 36, 11, 12] {
            p[sq] = [1.0 / 13.0; 13];
        }
        for _ in 0..5 {
            assert!(matches!(t.observe(&p), Update::Unsure { .. } | Update::Unchanged));
        }
        assert_eq!(t.board(), &Board::default());
    }

    #[test]
    fn a_missed_frame_finds_both_moves() {
        let mut t = GameTracker::new(
            Board::default(),
            TrackerConfig { stable_frames: 1, ..TrackerConfig::default() },
        );
        let p = sure(&grid_of(&after(&Board::default(), "e2e4 e7e5")));
        match t.observe(&p) {
            Update::Moved { moves } => {
                assert_eq!(moves.iter().map(|m| m.san.as_str()).collect::<Vec<_>>(), ["e4", "e5"]);
            }
            u => panic!("{u:?}"),
        }
    }

    #[test]
    fn another_game_is_lost() {
        let cfg = TrackerConfig { lost_after: 3, ..TrackerConfig::default() };
        let mut t = GameTracker::new(Board::default(), cfg);
        let other = sure(&parse_placement("8/8/3k4/8/8/2K5/8/8").unwrap());
        assert!(matches!(t.observe(&other), Update::Unsure { .. }));
        assert!(matches!(t.observe(&other), Update::Unsure { .. }));
        assert_eq!(t.observe(&other), Update::Lost);
    }
}
