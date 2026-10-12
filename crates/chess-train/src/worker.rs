//! Running a job on this machine, within resource limits.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use crate::play::{Player, opening, play_pair};
use crate::run::{Job, JobResult, Kind};
use crate::sprt::Penta;

/// How much of the machine a job may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Games played at once (one thread each).
    pub threads: usize,
    /// Hash-table memory in total, MB (split across the engines in play).
    pub memory_mb: usize,
}

impl Limits {
    /// Hash per engine: two engines per thread.
    pub fn hash_mb(&self) -> usize {
        (self.memory_mb / (2 * self.threads.max(1))).clamp(1, 1024)
    }
}

/// Run `job`. Stops early (with what's done) when `stop` is set. Returns
/// the result and, for datagen, the lines to upload.
///
/// # Panics
///
/// If a game thread panicked (an internal bug).
pub fn run(job: &Job, worker: &str, limits: Limits, stop: &AtomicBool) -> (JobResult, Vec<String>) {
    let started = Instant::now();
    let next = AtomicU64::new(0);
    let penta = Mutex::new(Penta::default());
    let spsa = Mutex::new(0.0f64);
    let lines = Mutex::new(Vec::<String>::new());
    let games = AtomicU64::new(0);
    let hash = limits.hash_mb();
    std::thread::scope(|s| {
        for _ in 0..limits.threads.max(1) {
            s.spawn(|| {
                let mut a = Player::new(job.a, hash);
                let mut b = Player::new(job.b, hash);
                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= job.count {
                        break;
                    }
                    match job.kind {
                        Kind::Sprt | Kind::Spsa => {
                            let pts = play_pair(&mut a, &mut b, &opening(job.seed, i), job.tc);
                            games.fetch_add(2, Ordering::Relaxed);
                            if job.kind == Kind::Sprt {
                                penta.lock().expect("lock").record(pts);
                            } else {
                                // Plus's points minus minus's over the pair.
                                *spsa.lock().expect("lock") += pts - (2.0 - pts);
                            }
                        }
                        Kind::Datagen => {
                            let nodes = match job.tc {
                                crate::play::Tc::Nodes { nodes } => nodes,
                                crate::play::Tc::Clock { .. } => 5_000,
                            };
                            let g = crate::datagen::game(
                                &mut a,
                                nodes,
                                job.random_plies,
                                job.seed ^ i.wrapping_mul(0x9e37),
                            );
                            games.fetch_add(1, Ordering::Relaxed);
                            lines.lock().expect("lock").extend(g);
                        }
                    }
                }
            });
        }
    });
    let lines = lines.into_inner().expect("lock");
    let result = JobResult {
        run_id: job.run_id,
        engine: job.engine.clone(),
        worker: worker.into(),
        penta: penta.into_inner().expect("lock"),
        step: job.step.clone(),
        spsa_result: spsa.into_inner().expect("lock"),
        games: games.load(Ordering::Relaxed),
        positions: lines.len() as u64,
        shard: None,
        secs: started.elapsed().as_secs_f64(),
    };
    (result, lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::play::Tc;
    use crate::run::{Run, RunSpec, SprtSpec};
    use chess_analysis::SearchParams;

    #[test]
    fn a_small_sprt_job_runs_on_two_threads() {
        let run = Run::new(
            1,
            "t".into(),
            "e".into(),
            RunSpec::Sprt(SprtSpec {
                base: SearchParams::default(),
                test: SearchParams::default(),
                elo0: 0.0,
                elo1: 5.0,
                alpha: 0.05,
                beta: 0.05,
                tc: Tc::Nodes { nodes: 300 },
                max_pairs: 1000,
                promote: false,
            }),
            0,
        );
        let job = run.job(2, 1, 5).unwrap();
        let stop = AtomicBool::new(false);
        let (r, lines) = run_job_for_test(&job, &stop);
        assert_eq!(r.penta.pairs(), 2);
        assert_eq!(r.games, 4);
        assert!(lines.is_empty());
    }

    fn run_job_for_test(job: &Job, stop: &AtomicBool) -> (JobResult, Vec<String>) {
        run(job, "test", Limits { threads: 2, memory_mb: 8 }, stop)
    }

    #[test]
    fn stopping_returns_what_was_done() {
        let job = Job {
            run_id: 1,
            kind: Kind::Datagen,
            engine: "e".into(),
            seed: 1,
            count: 1000,
            tc: Tc::Nodes { nodes: 200 },
            a: SearchParams::default(),
            b: SearchParams::default(),
            step: None,
            random_plies: 8,
        };
        let stop = AtomicBool::new(true);
        let (r, lines) = run(&job, "w", Limits { threads: 1, memory_mb: 4 }, &stop);
        assert_eq!(r.games, 0);
        assert!(lines.is_empty());
    }
}
