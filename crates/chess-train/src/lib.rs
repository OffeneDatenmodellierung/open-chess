//! Distributed training for the open-chess engine.
//!
//! The server keeps a list of runs (SPRT tests, SPSA tuning, self-play data
//! generation) and the current champion parameters. Any number of machines
//! running a training client (esports-lab's `chess train run`) claim jobs, play them within their
//! resource limits and report back; [`Run::apply`] merges each report
//! deterministically, so results from a laptop and a server combine into
//! one test or one tuning run.
//!
//! - [`sprt`]: pentanomial sequential tests (pass / fail / keep going);
//! - [`spsa`]: simultaneous-perturbation tuning of many parameters;
//! - [`play`]: the engine playing itself in-process, by node count or clock;
//! - [`datagen`]: self-play positions for training the evaluation network,
//!   as text or in [`bulletformat`] (the trainer's binary records);
//! - [`run`]: runs, jobs, results and merging;
//! - [`worker`]: running a job on this machine.

pub mod bulletformat;
pub mod datagen;
pub mod play;
pub mod run;
pub mod sprt;
pub mod spsa;
pub mod worker;

pub use play::Tc;
pub use run::{Applied, Job, JobResult, Kind, Run, RunSpec, Status};
