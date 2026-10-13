//! Playing the computer: `POST /api/challenge/ai` with `level` 1–8, as on
//! Lichess. The computer is open-chess's own engine (`chess_analysis::bot`)
//! at that level, played by the server itself: a game against
//! `ai-level-N`, a bot account made on first use, with a fixed rating so it
//! can't be farmed (games against it are never rated).
//!
//! Every replica runs [`run`], which finds games where it's the computer's
//! turn and moves. A game is taken with a Postgres advisory lock, so two
//! replicas never think about the same move.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use chess_analysis::bot::{Level, bot_move};
use chess_analysis::search::Searcher;
use chess_core::Game;
use tokio::sync::watch;
use tracing::{debug, warn};

use crate::app::Ctx;
use crate::auth::Identity;
use crate::challenges::{self, Request};
use crate::error::{ApiError, ApiResult};
use crate::model::{self, Account, ChallengeRow};
use crate::play::{self, Api};

/// The computer's username at a level.
pub fn username(level: u8) -> String {
    format!("ai-level-{}", level.clamp(1, 8))
}

/// The level of a computer account, from its username.
pub fn level_of(username: &str) -> Option<u8> {
    username.strip_prefix("ai-level-")?.parse::<u8>().ok().filter(|l| (1..=8).contains(l))
}

/// Each level's fixed rating (roughly what it plays like).
pub fn rating(level: u8) -> i32 {
    [800, 1000, 1200, 1400, 1600, 1800, 2000, 2200][usize::from(level.clamp(1, 8) - 1)]
}

const PERFS: &[&str] = &["ultraBullet", "bullet", "blitz", "rapid", "classical", "correspondence"];

/// The computer's account at a level, made on first use.
pub async fn ensure(ctx: &Ctx, level: u8) -> Result<Account> {
    let name = username(level);
    let mut c = ctx.db.conn().await?;
    if let Some(a) = model::account_by_name(&ctx.db, &*c, &name).await? {
        return Ok(a);
    }
    let tx = c.transaction().await?;
    let player: i64 = tx
        .query_one(
            &ctx.db.sql("INSERT INTO chess.players (name, key_hash) VALUES ($1, $2) RETURNING id"),
            &[&format!("Computer level {level}"), &crate::auth::hash_key(&crate::auth::random_token(28))],
        )
        .await?
        .get(0);
    tx.execute(
        &ctx.db.sql("INSERT INTO chess.accounts (player_id, username, kind) VALUES ($1, $2, 'bot')"),
        &[&player, &name],
    )
    .await?;
    for perf in PERFS {
        tx.execute(
            &ctx.db.sql(
                "INSERT INTO chess.ratings (player_id, perf, rating, deviation, volatility) VALUES ($1, $2, $3, 45, 0.06)
                 ON CONFLICT DO NOTHING",
            ),
            &[&player, perf, &f64::from(rating(level))],
        )
        .await?;
    }
    if let Err(e) = tx.commit().await {
        // Another replica made it first.
        debug!("computer level {level}: {e}");
    }
    let c = ctx.db.conn().await?;
    model::account_by_name(&ctx.db, &*c, &name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("the computer's account {name} wasn't made"))
}

/// `POST /api/challenge/ai`: a game against the computer, started at once.
pub async fn challenge(ctx: &Ctx, who: &Identity, level: u8, req: &Request) -> ApiResult<ChallengeRow> {
    if !(1..=8).contains(&level) {
        return Err(ApiError::bad("level is 1 to 8"));
    }
    let ai = ensure(ctx, level).await?;
    let req = Request { rated: false, keep_alive: false, accept_by_token: None, ..req.clone() };
    let (ch, _) = challenges::create_accepted(ctx, who, &ai.username, &req).await?;
    Ok(ch)
}

/// Games where it's the computer's turn.
async fn waiting(ctx: &Ctx) -> Result<Vec<String>> {
    let c = ctx.db.conn().await?;
    Ok(c.query(
        &ctx.db.sql(
            "SELECT g.id FROM chess.games g
               JOIN chess.accounts w ON w.player_id = g.white_id
               JOIN chess.accounts b ON b.player_id = g.black_id
              WHERE g.status = 20
                AND ((g.fen LIKE '% w %' AND w.username LIKE 'ai-level-%')
                  OR (g.fen LIKE '% b %' AND b.username LIKE 'ai-level-%'))
              LIMIT 50",
        ),
        &[],
    )
    .await?
    .iter()
    .map(|r| r.get(0))
    .collect())
}

/// Think about and play one game's move, if no other replica is.
async fn play_one(ctx: &Ctx, id: &str) -> Result<()> {
    let c = ctx.db.conn().await?;
    let locked: bool =
        c.query_one("SELECT pg_try_advisory_lock(hashtext($1))", &[&format!("ai:{id}")]).await?.get(0);
    if !locked {
        return Ok(());
    }
    let result = async {
        let Some(row) = model::game(&ctx.db, &*c, id).await? else { return Ok(()) };
        let game =
            Game::from_moves(row.initial_fen.as_deref(), &row.moves).map_err(|e| anyhow::anyhow!("{e}"))?;
        let side = game.turn();
        let me = row.player(side).clone();
        let Some(level) = level_of(&me.username) else { return Ok(()) };
        let seed = id.bytes().fold(u64::from(row.ply.unsigned_abs()), |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
        let started = std::time::Instant::now();
        let mv = tokio::task::spawn_blocking(move || {
            let mut searcher = Searcher::new(1 << 18);
            bot_move(&mut searcher, &game, Level::new(level), seed)
        })
        .await?;
        let Some(mv) = mv else { return Ok(()) };
        // A moment's pause, so the computer doesn't answer instantly.
        let pause = Duration::from_millis(400).saturating_sub(started.elapsed());
        tokio::time::sleep(pause).await;
        match play::make_move(ctx, id, me.player_id, Api::Bot, &mv.uci, false).await {
            Ok(()) => debug!("computer level {level} played {} in {id}", mv.san),
            // The game moved on meanwhile (a resignation, a flag).
            Err(e) => debug!("computer level {level} in {id}: {e:?}"),
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    let _ = c.execute("SELECT pg_advisory_unlock(hashtext($1))", &[&format!("ai:{id}")]).await;
    result
}

/// Play the computer's moves, on every replica, until `stop`.
pub async fn run(ctx: Ctx, mut stop: watch::Receiver<bool>) {
    // Games this replica is thinking about, so a slow move isn't started twice.
    let busy: Arc<Mutex<HashSet<String>>> = Arc::default();
    let mut t = tokio::time::interval(Duration::from_millis(250));
    t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = t.tick() => {}
            () = crate::stopped(&mut stop) => return,
        }
        let ids = match waiting(&ctx).await {
            Ok(ids) => ids,
            Err(e) => {
                warn!("computer: {e:#}");
                continue;
            }
        };
        for id in ids {
            if !busy.lock().is_ok_and(|mut b| b.insert(id.clone())) {
                continue;
            }
            let (ctx, busy) = (ctx.clone(), busy.clone());
            tokio::spawn(async move {
                if let Err(e) = play_one(&ctx, &id).await {
                    warn!("computer in {id}: {e:#}");
                }
                if let Ok(mut b) = busy.lock() {
                    b.remove(&id);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_levels() {
        assert_eq!(username(3), "ai-level-3");
        assert_eq!(level_of("ai-level-8"), Some(8));
        assert_eq!(level_of("ai-level-9"), None);
        assert_eq!(level_of("anna"), None);
        assert!(rating(1) < rating(8));
    }
}
