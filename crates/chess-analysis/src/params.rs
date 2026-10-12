//! The search's tunable numbers.
//!
//! Every margin, reduction and bonus the search uses lives here, with a
//! range and step sizes for automatic tuning (SPSA), so the distributed
//! training in `chess-train` can tune them across many machines and test
//! the results (SPRT) without rebuilding the engine. New search features
//! are switched on by a parameter too (0 = off), so whether one helps is
//! settled by games, not by guessing.
//!
//! [`SearchParams::default`] is the current best set; the training
//! server's champion replaces it once a tuned set has passed a test.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One tunable: its default, bounds, and SPSA step sizes (`c_end`: the
/// perturbation at the end of a tuning run; `r_end`: the learning rate
/// there, as in fishtest's SPSA).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Tunable {
    pub name: &'static str,
    pub default: i32,
    pub min: i32,
    pub max: i32,
    pub c_end: f64,
    pub r_end: f64,
    pub about: &'static str,
}

macro_rules! params {
    ($( $field:ident = $default:expr, $min:expr, $max:expr, $c:expr, $r:expr, $about:expr; )*) => {
        /// The search's tunable numbers (see the module docs).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(default)]
        pub struct SearchParams {
            $( #[doc = $about] pub $field: i32, )*
        }

        impl Default for SearchParams {
            fn default() -> Self {
                SearchParams { $( $field: $default, )* }
            }
        }

        /// Every tunable, with its metadata.
        pub const TUNABLES: &[Tunable] = &[
            $( Tunable { name: stringify!($field), default: $default, min: $min, max: $max, c_end: $c, r_end: $r, about: $about }, )*
        ];

        impl SearchParams {
            /// A parameter by name.
            pub fn get(&self, name: &str) -> Option<i32> {
                match name {
                    $( stringify!($field) => Some(self.$field), )*
                    _ => None,
                }
            }

            /// Set a parameter by name, clamped to its range. False if
            /// there is no such parameter.
            pub fn set(&mut self, name: &str, value: i32) -> bool {
                match name {
                    $( stringify!($field) => { self.$field = value.clamp($min, $max); true } )*
                    _ => false,
                }
            }
        }
    };
}

params! {
    // Aspiration windows.
    asp_delta = 25, 8, 80, 3.0, 0.002, "First aspiration half-window (cp)";
    // Reverse futility pruning.
    rfp_depth = 8, 3, 12, 0.5, 0.002, "Deepest node for reverse futility pruning";
    rfp_margin = 80, 30, 160, 6.0, 0.002, "Reverse futility margin per ply (cp)";
    // Null move.
    nmp_depth = 3, 2, 6, 0.5, 0.002, "Shallowest node for null-move pruning";
    nmp_base = 3, 2, 5, 0.5, 0.002, "Null-move reduction, constant part";
    nmp_div = 3, 2, 6, 0.5, 0.002, "Null-move reduction: one more ply per this many plies of depth";
    nmp_eval_div = 200, 80, 400, 15.0, 0.002, "Null-move reduction: one more ply per this many cp above beta";
    // Internal iterative reduction.
    iir_depth = 4, 2, 8, 0.5, 0.002, "Shallowest node reduced when there's no hash move";
    // Late-move pruning.
    lmp_base = 3, 1, 8, 0.5, 0.002, "Late-move pruning: quiet moves allowed at depth 0";
    // Futility pruning.
    fp_depth = 8, 3, 12, 0.5, 0.002, "Deepest (reduced) node for futility pruning";
    fp_base = 100, 30, 250, 8.0, 0.002, "Futility margin, constant part (cp)";
    fp_margin = 90, 30, 200, 6.0, 0.002, "Futility margin per ply (cp)";
    // Static exchange pruning.
    see_quiet = 25, 5, 80, 3.0, 0.002, "Quiet moves losing more than this × depth² by SEE are pruned (cp)";
    see_noisy = 100, 30, 200, 8.0, 0.002, "Captures losing more than this × depth by SEE are pruned (cp)";
    // Late-move reductions (×100).
    lmr_base = 75, 20, 150, 6.0, 0.002, "Late-move reduction, constant part (hundredths of a ply)";
    lmr_div = 225, 150, 350, 10.0, 0.002, "Late-move reduction divisor (hundredths)";
    lmr_hist = 6000, 2000, 16000, 400.0, 0.002, "History score worth one ply less reduction";
    // History.
    hist_bonus_max = 1200, 400, 2400, 80.0, 0.002, "Largest history bonus for a cut-off move";
    // Quiescence delta pruning.
    q_delta = 200, 50, 400, 15.0, 0.002, "Quiescence delta-pruning margin (cp)";
    // Singular extensions (0 = off).
    se_depth = 0, 0, 12, 0.5, 0.002, "Shallowest node for singular extensions (0: off)";
    se_margin = 2, 1, 6, 0.3, 0.002, "Singular margin per ply (cp)";
    // Continuation history (0 = off; otherwise its weight in move ordering).
    cont_hist = 0, 0, 4, 0.3, 0.002, "Weight of continuation history in quiet-move ordering (0: off)";
    // Static-evaluation correction from pawn structure (0 = off).
    corr_hist = 0, 0, 2, 0.2, 0.002, "Static evaluation correction history (0: off, 1: on, 2: double weight)";
    // Time management (percent).
    time_soft = 50, 25, 90, 4.0, 0.002, "Stop starting new iterations after this % of the move's time";
    time_stable = 0, 0, 60, 4.0, 0.002, "Stop this % earlier still when the best move hasn't changed for several iterations (0: off)";
}

impl SearchParams {
    /// Every parameter by name.
    pub fn to_map(&self) -> BTreeMap<String, i32> {
        TUNABLES.iter().filter_map(|t| self.get(t.name).map(|v| (t.name.to_string(), v))).collect()
    }

    /// From a map; unknown names are ignored, missing ones keep their
    /// default.
    pub fn from_map(map: &BTreeMap<String, i32>) -> SearchParams {
        let mut p = SearchParams::default();
        for (k, v) in map {
            p.set(k, *v);
        }
        p
    }

    /// The names that differ from `other`.
    pub fn diff(&self, other: &SearchParams) -> Vec<(&'static str, i32, i32)> {
        TUNABLES
            .iter()
            .filter_map(|t| {
                let (a, b) = (self.get(t.name)?, other.get(t.name)?);
                (a != b).then_some((t.name, b, a))
            })
            .collect()
    }
}

/// Look up a tunable's metadata.
pub fn tunable(name: &str) -> Option<&'static Tunable> {
    TUNABLES.iter().find(|t| t.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_in_range_and_round_trip() {
        for t in TUNABLES {
            assert!(t.min <= t.default && t.default <= t.max, "{}", t.name);
        }
        let p = SearchParams::default();
        assert_eq!(SearchParams::from_map(&p.to_map()), p);
        let mut q = p;
        assert!(q.set("rfp_margin", 1000));
        assert_eq!(q.rfp_margin, 160, "clamped");
        assert!(!q.set("nope", 1));
        assert_eq!(q.diff(&p), vec![("rfp_margin", 80, 160)]);
        let json = serde_json::to_string(&p).unwrap();
        let back: SearchParams = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
        let partial: SearchParams = serde_json::from_str(r#"{"rfp_margin": 90}"#).unwrap();
        assert_eq!(partial.rfp_margin, 90);
        assert_eq!(partial.asp_delta, 25);
    }
}
