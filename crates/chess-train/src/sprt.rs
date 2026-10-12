//! Sequential probability ratio tests on game pairs.
//!
//! Engine changes are tested the way fishtest and OpenBench do it: games
//! are played in pairs (the same opening with colours swapped) and counted
//! by the pair's total score, 0, ½, 1, 1½ or 2 (the "pentanomial" counts),
//! which removes most of the noise from unbalanced openings. The test
//! asks whether the change is at least `elo1` better (pass) or no better
//! than `elo0` (fail), stopping as soon as the evidence is strong enough
//! either way. The log-likelihood ratio uses the generalised SPRT
//! approximation on the pair scores' mean and variance (Michel Van den
//! Bergh's formula).
//!
//! Counts from any number of machines simply add up, so merging results
//! is addition.

use serde::{Deserialize, Serialize};

/// Game-pair counts by pair score: index 0 = both lost … 4 = both won.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Penta(pub [u64; 5]);

impl Penta {
    pub fn pairs(&self) -> u64 {
        self.0.iter().sum()
    }

    /// Add another machine's counts.
    pub fn add(&mut self, other: &Penta) {
        for (a, b) in self.0.iter_mut().zip(other.0) {
            *a += b;
        }
    }

    /// Record one pair worth `points` (0.0 to 2.0) to the tested side.
    pub fn record(&mut self, points: f64) {
        let i = (points * 2.0).round().clamp(0.0, 4.0) as usize;
        self.0[i] += 1;
    }

    /// Mean score per game (0–1) and the variance of the per-game score of
    /// a pair (pair score / 2), with a little regularisation so a handful of
    /// identical results don't give zero variance.
    fn stats(&self) -> Option<(f64, f64)> {
        let n = self.pairs() as f64;
        if n < 1.0 {
            return None;
        }
        // Half a pair of prior in every bucket.
        let counts: Vec<f64> = self.0.iter().map(|&c| c as f64 + 0.1).collect();
        let total: f64 = counts.iter().sum();
        let score = |i: usize| i as f64 / 4.0;
        let mean = counts.iter().enumerate().map(|(i, c)| c * score(i)).sum::<f64>() / total;
        let var = counts.iter().enumerate().map(|(i, c)| c * (score(i) - mean).powi(2)).sum::<f64>() / total;
        Some((mean, var))
    }

    /// The log-likelihood ratio for H1 (Elo ≥ `elo1`) against H0 (Elo ≤
    /// `elo0`), logistic Elo.
    pub fn llr(&self, elo0: f64, elo1: f64) -> f64 {
        let Some((mean, var)) = self.stats() else { return 0.0 };
        if var <= 0.0 {
            return 0.0;
        }
        let s0 = expected(elo0);
        let s1 = expected(elo1);
        let n = self.pairs() as f64;
        n * (s1 - s0) * (2.0 * mean - s0 - s1) / (2.0 * var)
    }

    /// The Elo difference these counts suggest, with a 95% interval.
    pub fn elo(&self) -> Option<(f64, f64, f64)> {
        let (mean, var) = self.stats()?;
        let n = self.pairs() as f64;
        let margin = 1.96 * (var / n).sqrt();
        Some((elo_of(mean), elo_of(mean - margin), elo_of(mean + margin)))
    }
}

/// Expected score for a logistic Elo difference.
pub fn expected(elo: f64) -> f64 {
    1.0 / (1.0 + 10f64.powf(-elo / 400.0))
}

/// Logistic Elo for an expected score.
pub fn elo_of(score: f64) -> f64 {
    let s = score.clamp(1e-6, 1.0 - 1e-6);
    -400.0 * (1.0 / s - 1.0).log10()
}

/// The decision bounds for error rates `alpha` (false pass) and `beta`
/// (false fail): fail at or below the first, pass at or above the second.
pub fn bounds(alpha: f64, beta: f64) -> (f64, f64) {
    ((beta / (1.0 - alpha)).ln(), ((1.0 - beta) / alpha).ln())
}

/// Where a test stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Running,
    Passed,
    Failed,
}

/// The verdict for counts under a test's settings.
pub fn verdict(p: &Penta, elo0: f64, elo1: f64, alpha: f64, beta: f64) -> Verdict {
    let llr = p.llr(elo0, elo1);
    let (lo, hi) = bounds(alpha, beta);
    if llr >= hi {
        Verdict::Passed
    } else if llr <= lo {
        Verdict::Failed
    } else {
        Verdict::Running
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_and_elo() {
        let (lo, hi) = bounds(0.05, 0.05);
        assert!((lo + 2.944).abs() < 0.01 && (hi - 2.944).abs() < 0.01);
        assert!((elo_of(expected(35.0)) - 35.0).abs() < 1e-6);
        assert!((expected(0.0) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn a_clearly_better_engine_passes_and_a_worse_one_fails() {
        // About +40 Elo: more wins than losses.
        let better = Penta([20, 180, 500, 260, 40]);
        assert_eq!(verdict(&better, 0.0, 5.0, 0.05, 0.05), Verdict::Passed);
        let worse = Penta([40, 260, 500, 180, 20]);
        assert_eq!(verdict(&worse, 0.0, 5.0, 0.05, 0.05), Verdict::Failed);
        let early = Penta([1, 3, 6, 4, 1]);
        assert_eq!(verdict(&early, 0.0, 5.0, 0.05, 0.05), Verdict::Running);
        let (e, lo, hi) = better.elo().unwrap();
        assert!(e > 20.0 && lo < e && e < hi, "{e} {lo} {hi}");
    }

    #[test]
    fn merging_is_addition() {
        let mut a = Penta([1, 2, 3, 4, 5]);
        a.add(&Penta([5, 4, 3, 2, 1]));
        assert_eq!(a, Penta([6, 6, 6, 6, 6]));
        let mut r = Penta::default();
        r.record(2.0);
        r.record(0.5);
        r.record(1.0);
        assert_eq!(r, Penta([0, 1, 1, 0, 1]));
    }
}
