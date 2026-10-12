//! SPSA tuning: improving many parameters at once from game results.
//!
//! Each step perturbs every tuned parameter by ±cₖ at random, plays the
//! "plus" engine against the "minus" engine for a few game pairs, and moves
//! each parameter towards whichever side scored better. With enough game
//! pairs this climbs towards the best values even though each result is
//! noisy. The schedule (aₖ, cₖ shrinking with the iteration) follows
//! fishtest's SPSA, set by each parameter's end-of-run step (`c_end`) and
//! learning rate (`r_end`).
//!
//! Workers on different machines each take a step from the current values
//! and report back; the server applies the reports one after another (the
//! asynchronous SPSA fishtest uses), so contributions merge without
//! coordination.

use chess_analysis::SearchParams;
use serde::{Deserialize, Serialize};

/// One parameter being tuned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tuned {
    pub name: String,
    pub start: f64,
    pub min: f64,
    pub max: f64,
    pub c_end: f64,
    pub r_end: f64,
}

impl Tuned {
    /// From the engine's own metadata for `name`, starting at `start`.
    pub fn from_engine(name: &str, start: i32) -> Option<Tuned> {
        let t = chess_analysis::params::tunable(name)?;
        Some(Tuned {
            name: name.into(),
            start: f64::from(start),
            min: f64::from(t.min),
            max: f64::from(t.max),
            c_end: t.c_end,
            r_end: t.r_end,
        })
    }
}

/// SPSA schedule constants.
pub const ALPHA: f64 = 0.602;
pub const GAMMA: f64 = 0.101;

/// One step's perturbation, sent to a worker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    /// +1 or −1 per tuned parameter.
    pub flips: Vec<i8>,
    /// cₖ per parameter.
    pub c: Vec<f64>,
    /// aₖ / cₖ² per parameter.
    pub r: Vec<f64>,
}

/// The step for iteration `k` (counted in game pairs) of a run of `n`
/// pairs, with flips drawn from `seed`.
pub fn step(params: &[Tuned], k: u64, n: u64, seed: u64) -> Step {
    let big_a = 0.1 * n as f64;
    let mut rng = seed;
    let mut flips = Vec::with_capacity(params.len());
    let mut c = Vec::with_capacity(params.len());
    let mut r = Vec::with_capacity(params.len());
    for p in params {
        rng = splitmix(rng);
        flips.push(if rng & 1 == 0 { 1 } else { -1 });
        let c0 = p.c_end * (n as f64).powf(GAMMA);
        let a0 = p.r_end * p.c_end * p.c_end * (big_a + n as f64).powf(ALPHA);
        let ck = c0 / (k as f64 + 1.0).powf(GAMMA);
        let ak = a0 / (big_a + k as f64 + 1.0).powf(ALPHA);
        c.push(ck);
        r.push(ak / (ck * ck));
    }
    Step { flips, c, r }
}

/// The plus and minus engines for a step from `theta` on top of `base`.
pub fn engines(
    base: &SearchParams,
    params: &[Tuned],
    theta: &[f64],
    s: &Step,
) -> (SearchParams, SearchParams) {
    let mut plus = *base;
    let mut minus = *base;
    for (i, p) in params.iter().enumerate() {
        let d = s.c[i] * f64::from(s.flips[i]);
        plus.set(&p.name, (theta[i] + d).clamp(p.min, p.max).round() as i32);
        minus.set(&p.name, (theta[i] - d).clamp(p.min, p.max).round() as i32);
    }
    (plus, minus)
}

/// Apply a step's result: `result` is the plus engine's score minus the
/// minus engine's, in games (wins count 1, draws ½).
pub fn apply(params: &[Tuned], theta: &mut [f64], s: &Step, result: f64) {
    for (i, p) in params.iter().enumerate() {
        theta[i] = (theta[i] + s.r[i] * s.c[i] * result * f64::from(s.flips[i])).clamp(p.min, p.max);
    }
}

/// The current values as engine parameters (rounded).
pub fn params_of(base: &SearchParams, params: &[Tuned], theta: &[f64]) -> SearchParams {
    let mut p = *base;
    for (i, t) in params.iter().enumerate() {
        p.set(&t.name, theta[i].round() as i32);
    }
    p
}

pub(crate) fn splitmix(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_shrink_and_climb_towards_the_better_side() {
        let p = vec![Tuned::from_engine("rfp_margin", 80).unwrap()];
        let early = step(&p, 0, 10_000, 1);
        let late = step(&p, 9_999, 10_000, 1);
        assert!(early.c[0] > late.c[0]);
        assert!((late.c[0] - p[0].c_end).abs() < 0.1, "{}", late.c[0]);
        // A noiseless toy: the plus side scores better whenever it's nearer 120.
        let mut theta = vec![80.0];
        for k in 0..400u64 {
            let s = step(&p, k * 10, 4_000, k);
            let (plus, minus) = engines(&SearchParams::default(), &p, &theta, &s);
            let d = (f64::from(minus.rfp_margin) - 120.0).abs() - (f64::from(plus.rfp_margin) - 120.0).abs();
            apply(&p, &mut theta, &s, d.signum() * 4.0);
        }
        assert!((theta[0] - 120.0).abs() < 15.0, "{}", theta[0]);
        assert_eq!(params_of(&SearchParams::default(), &p, &theta).rfp_margin, theta[0].round() as i32);
    }

    #[test]
    fn values_stay_in_range() {
        let p = vec![Tuned::from_engine("nmp_base", 3).unwrap()];
        let mut theta = vec![3.0];
        let s = step(&p, 0, 100, 3);
        apply(&p, &mut theta, &s, 1e9);
        assert!(theta[0] <= 5.0 && theta[0] >= 2.0);
    }
}
