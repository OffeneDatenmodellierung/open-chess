//! Training runs, the jobs they hand out, and how results merge.
//!
//! A run is one of:
//!
//! - **SPRT**: is parameter set B better than A? Pentanomial counts add up
//!   across machines until the test passes or fails.
//! - **SPSA**: tune some parameters. Each job is one step from the current
//!   values; reports are applied in arrival order.
//! - **Datagen**: self-play training data. Each job's lines go up as a
//!   shard; counts add up.
//!
//! All of it is plain data with deterministic merging, so the server can
//! keep a run in one database row and apply each result in a transaction,
//! and any number of machines can contribute at once.

use chess_analysis::SearchParams;
use serde::{Deserialize, Serialize};

use crate::play::Tc;
use crate::sprt::{self, Penta, Verdict};
use crate::spsa::{self, Step, Tuned};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SprtSpec {
    /// The reference parameters.
    pub base: SearchParams,
    /// The parameters under test.
    pub test: SearchParams,
    pub elo0: f64,
    pub elo1: f64,
    pub alpha: f64,
    pub beta: f64,
    pub tc: Tc,
    /// Give up (inconclusive) after this many pairs.
    pub max_pairs: u64,
    /// Make `test` the champion if it passes.
    pub promote: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpsaSpec {
    /// Everything not being tuned stays at these values.
    pub base: SearchParams,
    pub params: Vec<Tuned>,
    /// Length of the run in game pairs.
    pub pairs: u64,
    pub tc: Tc,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatagenSpec {
    pub params: SearchParams,
    pub nodes: u64,
    pub random_plies: u32,
    pub target_positions: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunSpec {
    Sprt(SprtSpec),
    Spsa(SpsaSpec),
    Datagen(DatagenSpec),
}

impl RunSpec {
    pub fn kind(&self) -> Kind {
        match self {
            RunSpec::Sprt(_) => Kind::Sprt,
            RunSpec::Spsa(_) => Kind::Spsa,
            RunSpec::Datagen(_) => Kind::Datagen,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Sprt,
    Spsa,
    Datagen,
}

impl std::str::FromStr for Kind {
    type Err = String;
    fn from_str(s: &str) -> Result<Kind, String> {
        match s {
            "sprt" => Ok(Kind::Sprt),
            "spsa" => Ok(Kind::Spsa),
            "datagen" => Ok(Kind::Datagen),
            _ => Err(format!("unknown job kind {s:?} (sprt, spsa or datagen)")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RunState {
    #[serde(default)]
    pub penta: Penta,
    #[serde(default)]
    pub theta: Vec<f64>,
    /// SPSA: game pairs applied so far.
    #[serde(default)]
    pub iter: u64,
    #[serde(default)]
    pub games: u64,
    #[serde(default)]
    pub positions: u64,
    #[serde(default)]
    pub shards: Vec<String>,
    /// Jobs handed out (to spread work across runs).
    #[serde(default)]
    pub assigned: u64,
    /// Contributions by worker name.
    #[serde(default)]
    pub workers: std::collections::BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Running,
    Passed,
    Failed,
    /// Ran its course without a verdict (SPRT at max pairs) or done (SPSA,
    /// datagen).
    Finished,
    Stopped,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Passed => "passed",
            Status::Failed => "failed",
            Status::Finished => "finished",
            Status::Stopped => "stopped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id: i64,
    pub name: String,
    /// The engine build this run is for (see `chess_analysis::engine_id`).
    pub engine: String,
    pub spec: RunSpec,
    pub state: RunState,
    pub status: Status,
    pub priority: i32,
}

/// Work for one machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub run_id: i64,
    pub kind: Kind,
    pub engine: String,
    pub seed: u64,
    /// SPRT/SPSA: game pairs to play. Datagen: games.
    pub count: u64,
    pub tc: Tc,
    /// SPRT: the tested parameters; SPSA: the plus engine; datagen: the
    /// engine.
    pub a: SearchParams,
    /// SPRT: the base; SPSA: the minus engine.
    pub b: SearchParams,
    #[serde(default)]
    pub step: Option<Step>,
    #[serde(default)]
    pub random_plies: u32,
}

/// What a machine sends back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobResult {
    pub run_id: i64,
    pub engine: String,
    pub worker: String,
    /// SPRT: pair counts for `a` (the tested side).
    #[serde(default)]
    pub penta: Penta,
    /// SPSA: the step that was played, and plus's score minus minus's.
    #[serde(default)]
    pub step: Option<Step>,
    #[serde(default)]
    pub spsa_result: f64,
    pub games: u64,
    #[serde(default)]
    pub positions: u64,
    /// Datagen: the uploaded shard's name.
    #[serde(default)]
    pub shard: Option<String>,
    pub secs: f64,
}

/// What applying a result did.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Applied {
    pub accepted: bool,
    /// The run just ended with this status.
    pub ended: Option<Status>,
    /// A tested set to make champion.
    pub promote: Option<SearchParams>,
    /// A run to start next (an SPSA run's result, to be tested).
    pub follow_up: Option<(String, RunSpec)>,
}

impl Run {
    pub fn new(id: i64, name: String, engine: String, spec: RunSpec, priority: i32) -> Run {
        let mut state = RunState::default();
        if let RunSpec::Spsa(s) = &spec {
            state.theta = s.params.iter().map(|p| p.start).collect();
        }
        Run { id, name, engine, spec, state, status: Status::Running, priority }
    }

    /// A job sized for `threads` threads (`per_thread` pairs or games each).
    pub fn job(&self, threads: u64, per_thread: u64, seed: u64) -> Option<Job> {
        if self.status != Status::Running {
            return None;
        }
        let count = threads.max(1) * per_thread.max(1);
        let base = Job {
            run_id: self.id,
            kind: self.spec.kind(),
            engine: self.engine.clone(),
            seed,
            count,
            tc: Tc::Nodes { nodes: 10_000 },
            a: SearchParams::default(),
            b: SearchParams::default(),
            step: None,
            random_plies: 0,
        };
        Some(match &self.spec {
            RunSpec::Sprt(s) => Job { tc: s.tc, a: s.test, b: s.base, ..base },
            RunSpec::Spsa(s) => {
                let step = spsa::step(&s.params, self.state.iter, s.pairs, seed);
                let (plus, minus) = spsa::engines(&s.base, &s.params, &self.state.theta, &step);
                Job { tc: s.tc, a: plus, b: minus, step: Some(step), ..base }
            }
            RunSpec::Datagen(d) => Job {
                tc: Tc::Nodes { nodes: d.nodes },
                a: d.params,
                b: d.params,
                random_plies: d.random_plies,
                count: threads.max(1) * per_thread.max(1),
                ..base
            },
        })
    }

    /// Merge a result in. Results for another engine, or arriving after the
    /// run ended, are refused (datagen shards are still kept by the caller).
    pub fn apply(&mut self, r: &JobResult) -> Applied {
        let mut out = Applied::default();
        if r.run_id != self.id || r.engine != self.engine || self.status != Status::Running {
            return out;
        }
        out.accepted = true;
        self.state.games += r.games;
        self.state.positions += r.positions;
        *self.state.workers.entry(r.worker.clone()).or_default() += r.games;
        match &self.spec {
            RunSpec::Sprt(s) => {
                self.state.penta.add(&r.penta);
                match sprt::verdict(&self.state.penta, s.elo0, s.elo1, s.alpha, s.beta) {
                    Verdict::Passed => {
                        self.status = Status::Passed;
                        if s.promote {
                            out.promote = Some(s.test);
                        }
                    }
                    Verdict::Failed => self.status = Status::Failed,
                    Verdict::Running if self.state.penta.pairs() >= s.max_pairs => {
                        self.status = Status::Finished;
                    }
                    Verdict::Running => {}
                }
            }
            RunSpec::Spsa(s) => {
                if let Some(step) = &r.step {
                    spsa::apply(&s.params, &mut self.state.theta, step, r.spsa_result);
                    self.state.iter += r.games / 2;
                }
                if self.state.iter >= s.pairs {
                    self.status = Status::Finished;
                    let tuned = spsa::params_of(&s.base, &s.params, &self.state.theta);
                    // Nothing moved far enough to change a value: nothing to test.
                    if tuned == s.base {
                        return Applied { ended: Some(self.status), ..out };
                    }
                    out.follow_up = Some((
                        format!("{} (tuned) vs base", self.name),
                        RunSpec::Sprt(SprtSpec {
                            base: s.base,
                            test: tuned,
                            elo0: 0.0,
                            elo1: 5.0,
                            alpha: 0.05,
                            beta: 0.05,
                            tc: s.tc,
                            max_pairs: 50_000,
                            promote: true,
                        }),
                    ));
                }
            }
            RunSpec::Datagen(d) => {
                if let Some(sh) = &r.shard {
                    self.state.shards.push(sh.clone());
                }
                if self.state.positions >= d.target_positions {
                    self.status = Status::Finished;
                }
            }
        }
        if self.status != Status::Running {
            out.ended = Some(self.status);
        }
        out
    }

    /// A one-line progress report.
    pub fn progress(&self) -> String {
        match &self.spec {
            RunSpec::Sprt(s) => {
                let p = &self.state.penta;
                let (lo, hi) = sprt::bounds(s.alpha, s.beta);
                let elo =
                    p.elo().map_or_else(String::new, |(e, a, b)| format!(", Elo {e:+.1} [{a:+.1}, {b:+.1}]"));
                format!(
                    "{} pairs, LLR {:.2} ({lo:.2}, {hi:.2}) for [{}, {}]{elo}",
                    p.pairs(),
                    p.llr(s.elo0, s.elo1),
                    s.elo0,
                    s.elo1
                )
            }
            RunSpec::Spsa(s) => {
                let vals: Vec<String> = s
                    .params
                    .iter()
                    .zip(&self.state.theta)
                    .map(|(p, v)| format!("{}={v:.1}", p.name))
                    .collect();
                format!("{}/{} pairs; {}", self.state.iter, s.pairs, vals.join(" "))
            }
            RunSpec::Datagen(d) => format!(
                "{}/{} positions from {} games in {} shards",
                self.state.positions,
                d.target_positions,
                self.state.games,
                self.state.shards.len()
            ),
        }
    }
}

/// Pick the run to work on: highest priority first, then the one that has
/// had the fewest jobs, among running runs for this engine and these kinds.
pub fn choose<'a>(runs: &'a [Run], engine: &str, kinds: &[Kind]) -> Option<&'a Run> {
    runs.iter()
        .filter(|r| r.status == Status::Running && r.engine == engine && kinds.contains(&r.spec.kind()))
        .max_by_key(|r| (r.priority, std::cmp::Reverse(r.state.assigned)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sprt_run() -> Run {
        let test = SearchParams { rfp_margin: 90, ..SearchParams::default() };
        Run::new(
            1,
            "rfp".into(),
            "e1".into(),
            RunSpec::Sprt(SprtSpec {
                base: SearchParams::default(),
                test,
                elo0: 0.0,
                elo1: 5.0,
                alpha: 0.05,
                beta: 0.05,
                tc: Tc::Nodes { nodes: 1000 },
                max_pairs: 100_000,
                promote: true,
            }),
            0,
        )
    }

    fn result(run: &Run, penta: Penta, worker: &str) -> JobResult {
        JobResult {
            run_id: run.id,
            engine: run.engine.clone(),
            worker: worker.into(),
            penta,
            step: None,
            spsa_result: 0.0,
            games: penta.pairs() * 2,
            positions: 0,
            shard: None,
            secs: 1.0,
        }
    }

    #[test]
    fn sprt_results_from_two_machines_add_up_and_promote() {
        let mut run = sprt_run();
        let job = run.job(4, 2, 9).unwrap();
        assert_eq!((job.count, job.a.rfp_margin, job.b.rfp_margin), (8, 90, 80));
        let a = run.apply(&result(&run.clone(), Penta([10, 90, 250, 130, 20]), "mac"));
        assert!(a.accepted && a.ended.is_none());
        let b = run.apply(&result(&run.clone(), Penta([10, 90, 250, 130, 20]), "server"));
        assert_eq!(b.ended, Some(Status::Passed));
        assert_eq!(b.promote.unwrap().rfp_margin, 90);
        assert_eq!(run.state.workers.len(), 2);
        // Late results are refused; so are another engine's.
        assert!(!run.apply(&result(&run.clone(), Penta([1, 0, 0, 0, 0]), "late")).accepted);
        let mut other = sprt_run();
        let mut r = result(&other.clone(), Penta([0, 0, 1, 0, 0]), "x");
        r.engine = "e0".into();
        assert!(!other.apply(&r).accepted);
    }

    #[test]
    fn spsa_finishes_into_a_test() {
        // Started away from the base value, so the tuned set differs from
        // the base however little 16 pairs move it.
        let p = vec![Tuned::from_engine("rfp_margin", 100).unwrap()];
        let mut run = Run::new(
            2,
            "tune rfp".into(),
            "e1".into(),
            RunSpec::Spsa(SpsaSpec {
                base: SearchParams::default(),
                params: p,
                pairs: 16,
                tc: Tc::Nodes { nodes: 500 },
            }),
            0,
        );
        for i in 0..2 {
            let job = run.job(2, 4, i).unwrap();
            let step = job.step.clone().unwrap();
            assert_ne!(job.a.rfp_margin, job.b.rfp_margin);
            let r = JobResult {
                run_id: 2,
                engine: "e1".into(),
                worker: "w".into(),
                penta: Penta::default(),
                step: Some(step),
                spsa_result: 2.0,
                games: job.count * 2,
                positions: 0,
                shard: None,
                secs: 1.0,
            };
            let a = run.apply(&r);
            if i == 1 {
                assert_eq!(a.ended, Some(Status::Finished));
                let (_, RunSpec::Sprt(t)) = a.follow_up.unwrap() else { panic!() };
                assert!(t.promote);
            }
        }
        assert_eq!(run.state.iter, 16);
    }

    #[test]
    fn spsa_that_moves_nothing_ends_without_a_test() {
        let p = vec![Tuned::from_engine("rfp_margin", 80).unwrap()];
        let spec =
            SpsaSpec { base: SearchParams::default(), params: p, pairs: 4, tc: Tc::Nodes { nodes: 500 } };
        let mut run = Run::new(3, "flat".into(), "e1".into(), RunSpec::Spsa(spec), 0);
        let job = run.job(2, 4, 0).unwrap();
        let r = JobResult {
            run_id: 3,
            engine: "e1".into(),
            worker: "w".into(),
            penta: Penta::default(),
            step: job.step.clone(),
            spsa_result: 0.0,
            games: 8,
            positions: 0,
            shard: None,
            secs: 1.0,
        };
        let a = run.apply(&r);
        assert_eq!(a.ended, Some(Status::Finished));
        assert!(a.follow_up.is_none());
    }

    #[test]
    fn choose_prefers_priority_then_least_worked() {
        let mut a = sprt_run();
        a.state.assigned = 5;
        let mut b = sprt_run();
        b.id = 2;
        let mut c = sprt_run();
        c.id = 3;
        c.engine = "old".into();
        let runs = vec![a.clone(), b.clone(), c];
        assert_eq!(choose(&runs, "e1", &[Kind::Sprt]).unwrap().id, 2);
        assert!(choose(&runs, "e1", &[Kind::Datagen]).is_none());
        let mut runs = vec![a, b];
        runs[0].priority = 1;
        assert_eq!(choose(&runs, "e1", &[Kind::Sprt]).unwrap().id, 1);
    }
}
