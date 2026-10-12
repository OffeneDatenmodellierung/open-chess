//! Win / draw / loss odds from a search score.
//!
//! The model is the shape engines use (Stockfish's "WDL model"): two
//! logistic curves in the evaluation, one for the stronger side winning
//! and one for it losing, with whatever is left over being the draw.
//!
//! ```text
//! P(white wins) = 1 / (1 + e^((a − x) / b))
//! P(black wins) = 1 / (1 + e^((a + x) / b))
//! P(draw)       = 1 − the two
//! ```
//!
//! where `x` is the evaluation in centipawns from White's side. `a` sets
//! how big an advantage must be before a win becomes more likely than not
//! and `b` how sharply the odds change. Both depend on the material left
//! ([`Params::for_phase`]): with lots of material an extra pawn decides
//! less often than in a pawn ending.
//!
//! The defaults are pitched at club-level human play, where draws are much
//! rarer than between engines. They are a starting point to fit from real
//! results (the same data the fraud work collects), not measured values.

use serde::Serialize;

use crate::search::Score;

/// Probabilities from White's side; they sum to 1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Wdl {
    pub white: f64,
    pub draw: f64,
    pub black: f64,
}

/// The model's parameters, in centipawns.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    pub a: f64,
    pub b: f64,
}

impl Params {
    /// Parameters for a game phase (24 = all pieces, 0 = pawns and kings).
    pub fn for_phase(phase: i32) -> Params {
        let t = f64::from(phase.clamp(0, 24)) / 24.0;
        // Endgames: equal positions are drawn more often, but a given
        // advantage converts more reliably (smaller b).
        Params { a: 60.0 + 70.0 * (1.0 - t), b: 70.0 + 50.0 * t }
    }
}

impl Wdl {
    /// A decided result.
    pub fn certain(winner: Option<bool>) -> Wdl {
        match winner {
            Some(true) => Wdl { white: 1.0, draw: 0.0, black: 0.0 },
            Some(false) => Wdl { white: 0.0, draw: 0.0, black: 1.0 },
            None => Wdl { white: 0.0, draw: 1.0, black: 0.0 },
        }
    }

    /// Odds for a score from White's side.
    pub fn from_score(score_white: Score, params: Params) -> Wdl {
        match score_white {
            Score::Mate { moves } => Wdl::certain(Some(moves > 0)),
            Score::Cp { value } => Wdl::from_cp(value, params),
        }
    }

    /// Odds for a centipawn evaluation from White's side.
    pub fn from_cp(cp_white: i32, p: Params) -> Wdl {
        let x = f64::from(cp_white.clamp(-4000, 4000));
        let white = 1.0 / (1.0 + ((p.a - x) / p.b).exp());
        let black = 1.0 / (1.0 + ((p.a + x) / p.b).exp());
        let draw = (1.0 - white - black).max(0.0);
        let total = white + draw + black;
        Wdl { white: white / total, draw: draw / total, black: black / total }
    }

    /// White's expected score (win = 1, draw = ½).
    pub fn expected_white(&self) -> f64 {
        self.white + self.draw / 2.0
    }
}

/// Lichess's win percentage for a centipawn score (the scale
/// `chess-fairplay` uses for accuracy), 0–100 for the side the score is
/// from. Kept identical so training numbers match fair-play reports.
pub fn lichess_win_percent(cp: i32) -> f64 {
    let cp = f64::from(cp.clamp(-1000, 1000));
    50.0 + 50.0 * (2.0 / (1.0 + (-0.003_682_08 * cp).exp()) - 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn sums_to_one_and_is_symmetric() {
        for phase in [0, 12, 24] {
            let p = Params::for_phase(phase);
            for cp in [-900, -150, 0, 35, 300, 2500] {
                let w = Wdl::from_cp(cp, p);
                assert!(close(w.white + w.draw + w.black, 1.0));
                let m = Wdl::from_cp(-cp, p);
                assert!(close(w.white, m.black) && close(w.black, m.white));
            }
        }
    }

    #[test]
    fn monotonic_in_the_evaluation() {
        let p = Params::for_phase(24);
        let mut last = 0.0;
        for cp in (-1000..=1000).step_by(50) {
            let w = Wdl::from_cp(cp, p).white;
            assert!(w >= last);
            last = w;
        }
    }

    #[test]
    fn sensible_levels() {
        let p = Params::for_phase(24);
        let level = Wdl::from_cp(0, p);
        assert!(level.white > 0.25 && level.white < 0.45, "{level:?}");
        assert!(Wdl::from_cp(300, p).white > 0.85);
        assert!(Wdl::from_cp(900, p).white > 0.99);
    }

    #[test]
    fn endgames_are_drawier_when_level() {
        assert!(Wdl::from_cp(0, Params::for_phase(0)).draw > Wdl::from_cp(0, Params::for_phase(24)).draw);
    }

    #[test]
    fn mate_is_certain() {
        let w = Wdl::from_score(Score::Mate { moves: -3 }, Params::for_phase(10));
        assert_eq!(w, Wdl::certain(Some(false)));
    }

    #[test]
    fn matches_fairplay_scale() {
        assert!(close(lichess_win_percent(0), 50.0));
        assert!(lichess_win_percent(200) > 65.0 && lichess_win_percent(200) < 70.0);
    }
}
