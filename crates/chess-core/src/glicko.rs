//! Glicko-2 ratings, updated after every rated game as Lichess does (each
//! game is its own rating period). Lichess's settings: start at 1500 with
//! a deviation of 500 and volatility 0.09, τ = 0.75, deviation kept
//! between 45 and 500, and "provisional" while the deviation is over 110.

use serde::{Deserialize, Serialize};

const SCALE: f64 = 173.7178;
const TAU: f64 = 0.75;
const EPSILON: f64 = 0.000_001;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rating {
    pub rating: f64,
    pub deviation: f64,
    pub volatility: f64,
}

/// Result of one game for the player being rated.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GameScore {
    Win,
    Draw,
    Loss,
}

impl GameScore {
    fn value(self) -> f64 {
        match self {
            GameScore::Win => 1.0,
            GameScore::Draw => 0.5,
            GameScore::Loss => 0.0,
        }
    }
}

impl Default for Rating {
    fn default() -> Self {
        Rating { rating: 1500.0, deviation: 500.0, volatility: 0.09 }
    }
}

impl Rating {
    pub const MIN_DEVIATION: f64 = 45.0;
    pub const MAX_DEVIATION: f64 = 500.0;
    pub const PROVISIONAL_DEVIATION: f64 = 110.0;

    pub fn provisional(&self) -> bool {
        self.deviation > Self::PROVISIONAL_DEVIATION
    }

    pub fn int(&self) -> i32 {
        self.rating.round() as i32
    }

    /// This player's rating after one game against `opponent`.
    #[must_use]
    pub fn after_game(&self, opponent: &Rating, score: GameScore) -> Rating {
        let mu = (self.rating - 1500.0) / SCALE;
        let phi = self.deviation / SCALE;
        let mu_j = (opponent.rating - 1500.0) / SCALE;
        let phi_j = opponent.deviation / SCALE;

        let g = 1.0 / (1.0 + 3.0 * phi_j * phi_j / (std::f64::consts::PI * std::f64::consts::PI)).sqrt();
        let e = 1.0 / (1.0 + (-g * (mu - mu_j)).exp());
        let v = 1.0 / (g * g * e * (1.0 - e));
        let delta = v * g * (score.value() - e);

        let sigma = new_volatility(phi, self.volatility, v, delta);
        let phi_star = (phi * phi + sigma * sigma).sqrt();
        let phi_new = 1.0 / (1.0 / (phi_star * phi_star) + 1.0 / v).sqrt();
        let mu_new = mu + phi_new * phi_new * g * (score.value() - e);

        Rating {
            rating: (mu_new * SCALE + 1500.0).clamp(400.0, 4000.0),
            deviation: (phi_new * SCALE).clamp(Self::MIN_DEVIATION, Self::MAX_DEVIATION),
            volatility: sigma,
        }
    }
}

/// Step 5 of Glickman's paper (the Illinois algorithm).
fn new_volatility(phi: f64, sigma: f64, v: f64, delta: f64) -> f64 {
    let a = (sigma * sigma).ln();
    let f = |x: f64| {
        let ex = x.exp();
        let d = phi * phi + v + ex;
        ex * (delta * delta - phi * phi - v - ex) / (2.0 * d * d) - (x - a) / (TAU * TAU)
    };
    let mut big_a = a;
    let mut big_b = if delta * delta > phi * phi + v {
        (delta * delta - phi * phi - v).ln()
    } else {
        let mut k = 1.0;
        while f(a - k * TAU) < 0.0 {
            k += 1.0;
        }
        a - k * TAU
    };
    let mut fa = f(big_a);
    let mut fb = f(big_b);
    let mut guard = 0;
    while (big_b - big_a).abs() > EPSILON && guard < 100 {
        let c = big_a + (big_a - big_b) * fa / (fb - fa);
        let fc = f(c);
        if fc * fb <= 0.0 {
            big_a = big_b;
            fa = fb;
        } else {
            fa /= 2.0;
        }
        big_b = c;
        fb = fc;
        guard += 1;
    }
    (big_a / 2.0).exp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glickman_paper_example_single_game() {
        // Glickman's worked example, first opponent only (1400, RD 30),
        // with the paper's τ of 0.5 the numbers differ slightly; check the
        // direction and size instead.
        let p = Rating { rating: 1500.0, deviation: 200.0, volatility: 0.06 };
        let o = Rating { rating: 1400.0, deviation: 30.0, volatility: 0.06 };
        let w = p.after_game(&o, GameScore::Win);
        assert!(w.rating > 1500.0 && w.rating < 1600.0, "{w:?}");
        assert!(w.deviation < 200.0);
        let l = p.after_game(&o, GameScore::Loss);
        assert!(l.rating < 1500.0 && l.rating > 1300.0, "{l:?}");
    }

    #[test]
    fn new_players_move_a_lot_and_are_provisional() {
        let a = Rating::default();
        let b = Rating::default();
        assert!(a.provisional());
        let a2 = a.after_game(&b, GameScore::Win);
        let b2 = b.after_game(&a, GameScore::Loss);
        assert!(a2.rating > 1650.0, "{a2:?}");
        assert!((a2.rating - 1500.0 + (b2.rating - 1500.0)).abs() < 1.0, "symmetric between equals");
        let d = a.after_game(&b, GameScore::Draw);
        assert!((d.rating - 1500.0).abs() < 1e-6);
        assert!(d.deviation < 500.0);
    }

    #[test]
    fn established_ratings_move_little() {
        let a = Rating { rating: 2000.0, deviation: 50.0, volatility: 0.06 };
        let b = Rating { rating: 2000.0, deviation: 50.0, volatility: 0.06 };
        let w = a.after_game(&b, GameScore::Win);
        assert!(w.rating - 2000.0 < 10.0 && w.rating > 2000.0, "{w:?}");
        assert!(!w.provisional());
        assert!(w.deviation >= Rating::MIN_DEVIATION);
    }
}
