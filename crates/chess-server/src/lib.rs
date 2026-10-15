//! The open-chess server, as a library (the `open-chess` binary is a
//! thin wrapper; integration tests drive the same pieces).

pub mod ai;
pub mod analyse;
pub mod app;
pub mod apple;
pub mod auth;
pub mod challenges;
pub mod cli;
pub mod config;
pub mod db;
pub mod error;
pub mod http;
pub mod metrics;
pub mod model;
pub mod passkeys;
pub mod play;
pub mod push;
pub mod vision;

use std::time::Duration;

use tokio::sync::watch;
use tracing::warn;

/// Resolves once shutdown has begun (the watch guard isn't held, so the
/// future stays `Send`).
pub async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|v| *v).await.map(drop);
}

/// What every serving replica runs besides HTTP: the LISTEN connection
/// that brings other replicas' changes in, and the clock/challenge
/// sweeper (rows are claimed with SKIP LOCKED, so replicas don't collide).
pub fn spawn_background(ctx: &app::Ctx, stop: &watch::Receiver<bool>) {
    tokio::spawn(ctx.db.clone().listen((*ctx.cfg).clone(), stop.clone()));
    tokio::spawn(ai::run(ctx.clone(), stop.clone()));
    let sweeper = ctx.clone();
    let mut stop_sweep = stop.clone();
    tokio::spawn(async move {
        let mut t = tokio::time::interval(Duration::from_millis(sweeper.cfg.sweep_ms.max(100)));
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = t.tick() => {
                    if let Err(e) = play::sweep(&sweeper).await {
                        warn!("sweeper: {e:#}");
                    }
                }
                () = stopped(&mut stop_sweep) => break,
            }
        }
    });
}
