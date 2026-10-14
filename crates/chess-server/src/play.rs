//! Playing a game: moves, resignations, draws, takebacks, aborts, claims,
//! the end of a game (ratings, notifications) and the sweeper that flags
//! clocks and lapses challenges.
//!
//! Every action locks the game row (`FOR UPDATE`) in a transaction, works
//! out the new state with chess-core, writes it and notifies in the same
//! transaction. Postgres is the only clock (`clock_timestamp()`), so any
//! replica can take any request and they all agree on time used.

use anyhow::Result;
use chess_core::{Clock, Game, GameScore, Side, Status};
use tokio_postgres::GenericClient;
use tokio_postgres::Transaction;
use tracing::{debug, warn};

use crate::app::Ctx;
use crate::db::{Note, notify};
use crate::error::{ApiError, ApiResult};
use crate::model::{self, Account, ChallengeRow, GameRow};

/// Board API (people) or Bot API (bot accounts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Api {
    Board,
    Bot,
}

impl Api {
    pub fn check(self, a: &Account) -> ApiResult<()> {
        match (self, a.bot) {
            (Api::Board, true) => {
                Err(ApiError::bad("This endpoint can only be used with a Board API (non-bot) account"))
            }
            (Api::Bot, false) => Err(ApiError::bad(
                "This endpoint can only be used with a Bot account. See /api#tag/Bot/operation/botAccountUpgrade",
            )),
            _ => Ok(()),
        }
    }
}

fn side_str(s: Option<Side>) -> Option<&'static str> {
    s.map(Side::name)
}

/// Lock an ongoing game the player is in, checking the API matches their
/// account kind.
async fn lock_playing(
    ctx: &Ctx,
    tx: &Transaction<'_>,
    id: &str,
    player: i64,
    api: Api,
) -> ApiResult<(GameRow, Side)> {
    let row = model::game_for_update(&ctx.db, tx, id).await?.ok_or_else(|| ApiError::not_found("game"))?;
    let side = row.side_of(player).ok_or_else(|| ApiError::not_found("game"))?;
    api.check(row.player(side))?;
    if !row.status.is_ongoing() {
        return Err(ApiError::not_your_turn());
    }
    Ok((row, side))
}

fn replay(row: &GameRow) -> Result<Game> {
    Game::from_moves(row.initial_fen.as_deref(), &row.moves)
        .map_err(|e| anyhow::anyhow!("game {} doesn't replay: {e}", row.id))
}

pub async fn make_move(
    ctx: &Ctx,
    id: &str,
    player: i64,
    api: Api,
    uci: &str,
    offering_draw: bool,
) -> ApiResult<()> {
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let (row, side) = lock_playing(ctx, &tx, id, player, api).await?;
    let mut game = replay(&row)?;
    if game.turn() != side {
        return Err(ApiError::not_your_turn());
    }
    // Out of time (or never started) before this move arrived?
    if let Some(end) = timed_out(ctx, &row, &game) {
        finish(ctx, &tx, &row, end.0, end.1).await?;
        tx.commit().await?;
        return Err(ApiError::not_your_turn());
    }
    let mv = game.play_uci(uci).map_err(|e| ApiError::bad(e.to_string()))?;
    let ply = row.ply as usize;
    let mut move_ms = row.move_ms.clone();
    let mut clock_ms = row.clock_ms.clone();
    let (mut white_ms, mut black_ms) = (row.white_ms, row.black_ms);
    if let (Some(init), Some(inc)) = (row.clock_initial_ms, row.clock_increment_ms) {
        let mut clock = Clock {
            initial_ms: init,
            increment_ms: inc,
            remaining_ms: [white_ms.unwrap_or(init), black_ms.unwrap_or(init)],
        };
        // Each side's first move is free: clocks start once both have
        // moved, and there's no increment for it either.
        let used =
            if ply >= 2 { clock.apply_move(side, row.elapsed_ms, ctx.cfg.lag_comp_ms).used_ms } else { 0 };
        move_ms.push(used);
        clock_ms.push(clock.remaining(side));
        white_ms = Some(clock.remaining(Side::White));
        black_ms = Some(clock.remaining(Side::Black));
    } else {
        move_ms.push(row.elapsed_ms.max(0));
        clock_ms.push(model::NO_CLOCK_MS);
    }
    // Draw offers: moving past the opponent's offer declines it; a move
    // "offering" a draw while the opponent offers one agrees to it.
    let opp_offered = row.draw_offer == Some(side.opposite());
    let mut draw_offer = row.draw_offer.filter(|s| *s == side);
    let mut agreed = false;
    if offering_draw {
        if opp_offered {
            agreed = true;
        } else if !(row.has_rule("noEarlyDraw") && ply < 60) {
            draw_offer = Some(side);
        }
    }
    let moves = if row.moves.is_empty() { mv.uci.clone() } else { format!("{} {}", row.moves, mv.uci) };
    tx.execute(
        &ctx.db.sql(
            "UPDATE chess.games SET moves = $2, ply = $3, fen = $4, white_ms = $5, black_ms = $6, move_ms = $7,
                    clock_ms = $8, draw_offer = $9, takeback_offer = NULL,
                    turn_started_at = clock_timestamp(), last_move_at = clock_timestamp()
              WHERE id = $1",
        ),
        &[
            &row.id,
            &moves,
            &(row.ply + 1),
            &game.fen(),
            &white_ms,
            &black_ms,
            &move_ms,
            &clock_ms,
            &side_str(draw_offer),
        ],
    )
    .await?;
    ctx.metrics.moves.inc();
    let end = game.outcome().map(|o| (o.status, o.winner)).or(agreed.then_some((Status::Draw, None)));
    if let Some((status, winner)) = end {
        let row = model::game(&ctx.db, &tx, id).await?.ok_or_else(|| ApiError::not_found("game"))?;
        finish(ctx, &tx, &row, status, winner).await?;
    } else {
        notify(&tx, &ctx.db, &Note::Game(row.id.clone())).await?;
    }
    tx.commit().await?;
    // Correspondence: the other player may be anywhere; tell their phone.
    if row.days_per_turn.is_some() && end.is_none() {
        let me = &row.player(side).username;
        let them = row.player(side.opposite()).player_id;
        crate::push::notify(
            ctx,
            them,
            "Your move".into(),
            format!("{me} played {}.", mv.san),
            Some(row.id.clone()),
            Some(row.id.clone()),
        );
    }
    debug!(game = id, uci = mv.uci, "move");
    Ok(())
}

/// The game is over on time before anything else can happen: a flag (the
/// side to move's clock ran out) or a first move that never came.
fn timed_out(ctx: &Ctx, row: &GameRow, game: &Game) -> Option<(Status, Option<Side>)> {
    if !row.status.is_ongoing() {
        return None;
    }
    if let Some(left) = row.days_left_ms() {
        // Correspondence: the days run from the first move too.
        if left > 0 {
            return None;
        }
        if row.ply < 2 {
            return Some((Status::Aborted, None));
        }
        let winner = row.turn().opposite();
        return Some(if game.cannot_win(winner) {
            (Status::Outoftime, None)
        } else {
            (Status::Outoftime, Some(winner))
        });
    }
    if row.ply < 2 {
        let limit = i64::try_from(ctx.cfg.first_move_secs * 1000).unwrap_or(i64::MAX);
        return (row.elapsed_ms > limit + ctx.cfg.lag_comp_ms).then_some((Status::Aborted, None));
    }
    if !row.clock_running() {
        return None;
    }
    let turn = row.turn();
    let left = match turn {
        Side::White => row.white_ms,
        Side::Black => row.black_ms,
    }
    .unwrap_or(0);
    if row.elapsed_ms - ctx.cfg.lag_comp_ms < left {
        return None;
    }
    let winner = turn.opposite();
    // Lichess's rule: a flag against a side that can't mate is a draw.
    Some(if game.cannot_win(winner) { (Status::Outoftime, None) } else { (Status::Outoftime, Some(winner)) })
}

pub async fn resign(ctx: &Ctx, id: &str, player: i64, api: Api) -> ApiResult<()> {
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let (row, side) = lock_playing(ctx, &tx, id, player, api).await?;
    if row.ply < 2 && !row.has_rule("noAbort") {
        finish(ctx, &tx, &row, Status::Aborted, None).await?;
    } else {
        finish(ctx, &tx, &row, Status::Resign, Some(side.opposite())).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn abort(ctx: &Ctx, id: &str, player: i64, api: Api) -> ApiResult<()> {
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let (row, _) = lock_playing(ctx, &tx, id, player, api).await?;
    if row.ply >= 2 || row.has_rule("noAbort") {
        return Err(ApiError::bad("This game can no longer be aborted"));
    }
    finish(ctx, &tx, &row, Status::Aborted, None).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn draw(ctx: &Ctx, id: &str, player: i64, api: Api, accept: bool) -> ApiResult<()> {
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let (row, side) = lock_playing(ctx, &tx, id, player, api).await?;
    let opp_offered = row.draw_offer == Some(side.opposite());
    if accept && opp_offered {
        finish(ctx, &tx, &row, Status::Draw, None).await?;
    } else {
        let offer = if accept {
            if row.has_rule("noEarlyDraw") && row.ply < 60 {
                return Err(ApiError::bad("Draw offers aren't allowed this early in the game"));
            }
            Some(side)
        } else {
            None
        };
        tx.execute(
            &ctx.db.sql("UPDATE chess.games SET draw_offer = $2 WHERE id = $1"),
            &[&row.id, &side_str(offer)],
        )
        .await?;
        notify(&tx, &ctx.db, &Note::Game(row.id.clone())).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn takeback(ctx: &Ctx, id: &str, player: i64, api: Api, accept: bool) -> ApiResult<()> {
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let (row, side) = lock_playing(ctx, &tx, id, player, api).await?;
    let opp = side.opposite();
    if !accept {
        if row.takeback_offer.is_some() {
            tx.execute(&ctx.db.sql("UPDATE chess.games SET takeback_offer = NULL WHERE id = $1"), &[&row.id])
                .await?;
            notify(&tx, &ctx.db, &Note::Game(row.id.clone())).await?;
        }
    } else if row.takeback_offer == Some(opp) {
        // The proposer's last move goes; if it's their turn, so does ours.
        let n = if row.turn() == opp { 2 } else { 1 };
        if (row.ply as usize) < n {
            return Err(ApiError::bad("Nothing to take back"));
        }
        let mut game = replay(&row)?;
        game.undo(n);
        let keep = game.ply();
        let mut move_ms = row.move_ms.clone();
        let mut clock_ms = row.clock_ms.clone();
        move_ms.truncate(keep);
        clock_ms.truncate(keep);
        let (white_ms, black_ms) = restored_clocks(&row, game.first_to_move(), &clock_ms);
        tx.execute(
            &ctx.db.sql(
                "UPDATE chess.games SET moves = $2, ply = $3, fen = $4, move_ms = $5, clock_ms = $6,
                        white_ms = $7, black_ms = $8,
                        takeback_offer = NULL, draw_offer = NULL, turn_started_at = clock_timestamp()
                  WHERE id = $1",
            ),
            &[
                &row.id,
                &game.uci_moves().join(" "),
                &(keep as i32),
                &game.fen(),
                &move_ms,
                &clock_ms,
                &white_ms,
                &black_ms,
            ],
        )
        .await?;
        notify(&tx, &ctx.db, &Note::Game(row.id.clone())).await?;
    } else if row.takeback_offer != Some(side) {
        if !row.has_moved(side) {
            return Err(ApiError::bad("You haven't moved yet"));
        }
        tx.execute(
            &ctx.db.sql("UPDATE chess.games SET takeback_offer = $2 WHERE id = $1"),
            &[&row.id, &side.name()],
        )
        .await?;
        notify(&tx, &ctx.db, &Note::Game(row.id.clone())).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Each side's clock after a takeback: what it read after that side's last
/// remaining move, or the starting time if it has none left.
fn restored_clocks(row: &GameRow, first: Side, clock_ms: &[i64]) -> (Option<i64>, Option<i64>) {
    let Some(init) = row.clock_initial_ms else { return (row.white_ms, row.black_ms) };
    let last = |side: Side| {
        clock_ms
            .iter()
            .enumerate()
            .rev()
            .find(|(i, _)| Side::at_ply(first, *i) == side)
            .map_or(init, |(_, ms)| *ms)
    };
    (Some(last(Side::White)), Some(last(Side::Black)))
}

/// Seconds since `player` last had a stream open on this game (since the
/// game started, if never).
pub async fn absent_secs(ctx: &Ctx, c: &impl GenericClient, row: &GameRow, player: i64) -> Result<i64> {
    let r = c
        .query_one(
            &ctx.db.sql(
                "SELECT extract(epoch from (clock_timestamp() - coalesce(
                        (SELECT seen_at FROM chess.presence WHERE game_id = $1 AND player_id = $2),
                        g.created_at)))::bigint
                   FROM chess.games g WHERE g.id = $1",
            ),
            &[&row.id, &player],
        )
        .await?;
    Ok(r.get(0))
}

/// Seconds until the opponent may claim the win (None: `player` isn't gone).
pub fn claim_in(ctx: &Ctx, absent: i64) -> Option<i64> {
    let gone = i64::try_from(ctx.cfg.gone_after_secs).unwrap_or(i64::MAX);
    let claim = i64::try_from(ctx.cfg.claim_win_secs).unwrap_or(i64::MAX);
    (absent >= gone).then(|| (gone + claim - absent).max(0))
}

async fn claim(ctx: &Ctx, id: &str, player: i64, api: Api, victory: bool) -> ApiResult<()> {
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let (row, side) = lock_playing(ctx, &tx, id, player, api).await?;
    if row.has_rule("noClaimWin") {
        return Err(ApiError::bad("Claiming isn't allowed in this game"));
    }
    let opp = row.player(side.opposite()).player_id;
    let absent = absent_secs(ctx, &tx, &row, opp).await?;
    if claim_in(ctx, absent) != Some(0) {
        return Err(ApiError::bad("Your opponent is still here"));
    }
    if victory {
        finish(ctx, &tx, &row, Status::Timeout, Some(side)).await?;
    } else {
        finish(ctx, &tx, &row, Status::Draw, None).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn claim_victory(ctx: &Ctx, id: &str, player: i64, api: Api) -> ApiResult<()> {
    claim(ctx, id, player, api, true).await
}

pub async fn claim_draw(ctx: &Ctx, id: &str, player: i64, api: Api) -> ApiResult<()> {
    claim(ctx, id, player, api, false).await
}

/// Record that `player` has a stream open on a game.
pub async fn touch(ctx: &Ctx, game: &str, player: i64) -> Result<()> {
    let c = ctx.db.conn().await?;
    c.execute(
        &ctx.db.sql(
            "INSERT INTO chess.presence (game_id, player_id, seen_at) VALUES ($1, $2, clock_timestamp())
             ON CONFLICT (game_id, player_id) DO UPDATE SET seen_at = EXCLUDED.seen_at",
        ),
        &[&game, &player],
    )
    .await?;
    Ok(())
}

/// End a game: status and winner, ratings for a rated game, fair-play
/// queueing, and the gameFinish events. `row` must be locked.
pub async fn finish(
    ctx: &Ctx,
    tx: &Transaction<'_>,
    row: &GameRow,
    status: Status,
    winner: Option<Side>,
) -> Result<()> {
    let humans = !row.white.bot || !row.black.bot;
    let analysis =
        if ctx.cfg.fairplay && humans && !status.is_void() && row.ply >= 20 { "pending" } else { "skipped" };
    let (wd, bd) =
        if row.rated && !status.is_void() { rate(ctx, tx, row, winner).await? } else { (None, None) };
    // The clocks stop: the side to move is charged its running time, and a
    // flag leaves it with nothing.
    let (mut wms, mut bms) = (row.white_ms, row.black_ms);
    if row.clock_running() {
        let left = if status == Status::Outoftime { 0 } else { row.live_ms(row.turn()) };
        match row.turn() {
            Side::White => wms = Some(left),
            Side::Black => bms = Some(left),
        }
    }
    tx.execute(
        &ctx.db.sql(
            "UPDATE chess.games SET status = $2, winner = $3, finished_at = clock_timestamp(), feed_at = clock_timestamp(), draw_offer = NULL,
                    takeback_offer = NULL, analysis = $4, white_rating_diff = $5, black_rating_diff = $6,
                    white_ms = $7, black_ms = $8
              WHERE id = $1",
        ),
        &[&row.id, &status.id(), &side_str(winner), &analysis, &wd, &bd, &wms, &bms],
    )
    .await?;
    ctx.metrics.games_finished.with_label_values(&[status.name()]).inc();
    let done = model::game(&ctx.db, tx, &row.id).await?.unwrap_or_else(|| row.clone());
    notify(tx, &ctx.db, &Note::Game(row.id.clone())).await?;
    for p in [row.white.player_id, row.black.player_id] {
        if let Some(e) = done.event(p, chess_proto::GameEvent::FINISH) {
            notify(tx, &ctx.db, &Note::User { player: p, event: e }).await?;
        }
    }
    debug!(game = row.id, status = status.name(), "game over");
    Ok(())
}

/// Glicko-2 update for both players (rows locked in player-id order so two
/// games finishing at once can't deadlock).
async fn rate(
    ctx: &Ctx,
    tx: &Transaction<'_>,
    row: &GameRow,
    winner: Option<Side>,
) -> Result<(Option<i32>, Option<i32>)> {
    let perf = row.perf_key();
    let mut ids = [row.white.player_id, row.black.player_id];
    ids.sort_unstable();
    for id in ids {
        tx.execute(
            &ctx.db.sql(
                "INSERT INTO chess.ratings (player_id, perf, rating, deviation, volatility)
                 VALUES ($1, $2, 1500, 500, 0.09) ON CONFLICT DO NOTHING",
            ),
            &[&id, &perf],
        )
        .await?;
        tx.execute(
            &ctx.db.sql("SELECT 1 FROM chess.ratings WHERE player_id = $1 AND perf = $2 FOR UPDATE"),
            &[&id, &perf],
        )
        .await?;
    }
    let (w, _, _) = model::rating(&ctx.db, tx, row.white.player_id, perf).await?;
    let (b, _, _) = model::rating(&ctx.db, tx, row.black.player_id, perf).await?;
    let (sw, sb) = match winner {
        Some(Side::White) => (GameScore::Win, GameScore::Loss),
        Some(Side::Black) => (GameScore::Loss, GameScore::Win),
        None => (GameScore::Draw, GameScore::Draw),
    };
    let nw = w.after_game(&b, sw);
    let nb = b.after_game(&w, sb);
    let mut diffs = [0; 2];
    for (i, (account, old, new)) in [(&row.white, w, nw), (&row.black, b, nb)].into_iter().enumerate() {
        // The computer keeps its level's fixed rating.
        if crate::ai::level_of(&account.username).is_some() {
            continue;
        }
        let id = account.player_id;
        let diff = new.int() - old.int();
        diffs[i] = diff;
        tx.execute(
            &ctx.db.sql(
                "UPDATE chess.ratings SET rating = $3, deviation = $4, volatility = $5, games = games + 1,
                        prog = $6, updated_at = now()
                  WHERE player_id = $1 AND perf = $2",
            ),
            &[&id, &perf, &new.rating, &new.deviation, &new.volatility, &diff],
        )
        .await?;
    }
    Ok((Some(diffs[0]), Some(diffs[1])))
}

/// Start the game for an accepted challenge (same id as the challenge).
pub async fn start_game(ctx: &Ctx, tx: &Transaction<'_>, ch: &ChallengeRow) -> Result<GameRow> {
    let (white, black) = match ch.final_color {
        Side::White => (&ch.challenger, &ch.dest),
        Side::Black => (&ch.dest, &ch.challenger),
    };
    let speed = ch.speed();
    let (wr, _, _) = model::rating(&ctx.db, tx, white.player_id, speed.key()).await?;
    let (br, _, _) = model::rating(&ctx.db, tx, black.player_id, speed.key()).await?;
    let game = Game::new(ch.initial_fen.as_deref()).map_err(|e| anyhow::anyhow!("{e}"))?;
    let init = ch.clock_limit.map(|l| i64::from(l) * 1000);
    let inc = ch.clock_increment.map(|i| i64::from(i) * 1000);
    tx.execute(
        &ctx.db.sql(
            "INSERT INTO chess.games (id, white_id, black_id, variant, initial_fen, rated, source, speed, rules,
                    clock_initial_ms, clock_increment_ms, status, fen, white_ms, black_ms, white_rating, black_rating,
                    white_provisional, black_provisional, created_at, turn_started_at, days_per_turn)
             VALUES ($1, $2, $3, $4, $5, $6, 'friend', $7, $8, $9, $10, 20, $11, $9, $9, $12, $13, $14, $15,
                     clock_timestamp(), clock_timestamp(), $16)",
        ),
        &[
            &ch.id,
            &white.player_id,
            &black.player_id,
            &ch.variant,
            &game.initial_fen(),
            &ch.rated,
            &speed.key(),
            &ch.rules,
            &init,
            &inc,
            &game.fen(),
            &wr.int(),
            &br.int(),
            &wr.provisional(),
            &br.provisional(),
            &ch.days,
        ],
    )
    .await?;
    let row = model::game(&ctx.db, tx, &ch.id).await?.ok_or_else(|| anyhow::anyhow!("new game vanished"))?;
    for p in [white.player_id, black.player_id] {
        if let Some(e) = row.event(p, chess_proto::GameEvent::START) {
            notify(tx, &ctx.db, &Note::User { player: p, event: e }).await?;
        }
    }
    notify(tx, &ctx.db, &Note::Game(row.id.clone())).await?;
    ctx.metrics.games_started.inc();
    Ok(row)
}

/// One pass of the sweeper: lapsed challenges, flags and first moves that
/// never came. Safe on every replica at once (rows are locked with SKIP
/// LOCKED and conditions rechecked under the lock).
pub async fn sweep(ctx: &Ctx) -> Result<usize> {
    let mut done = 0;
    done += crate::challenges::expire(ctx).await?;
    let c = ctx.db.conn().await?;
    let ids: Vec<String> = c
        .query(
            &ctx.db.sql(
                "SELECT id FROM chess.games
                  WHERE status = 20 AND (
                        (days_per_turn IS NOT NULL AND clock_timestamp() > turn_started_at + make_interval(days => days_per_turn))
                     OR (days_per_turn IS NULL AND ply < 2
                         AND clock_timestamp() > turn_started_at + make_interval(secs => ($1::float8 + $2::float8) / 1000.0))
                     OR (ply >= 2 AND clock_initial_ms IS NOT NULL
                         AND clock_timestamp() > turn_started_at
                             + make_interval(secs => ((CASE WHEN fen LIKE '% w %' THEN white_ms ELSE black_ms END) + $2::float8) / 1000.0)))
                  LIMIT 100",
            ),
            &[&(ctx.cfg.first_move_secs as f64 * 1000.0), &(ctx.cfg.lag_comp_ms as f64)],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    drop(c);
    for id in ids {
        let mut c = ctx.db.conn().await?;
        let tx = c.transaction().await?;
        let row = tx
            .query_opt(
                &ctx.db.sql(&format!("{} WHERE g.id = $1 FOR UPDATE OF g SKIP LOCKED", model::GAME_SELECT)),
                &[&id],
            )
            .await?
            .map(|r| GameRow::from_row(&r));
        let Some(row) = row else { continue };
        let game = match replay(&row) {
            Ok(g) => g,
            Err(e) => {
                warn!("{e:#}");
                continue;
            }
        };
        if let Some((status, winner)) = timed_out(ctx, &row, &game) {
            finish(ctx, &tx, &row, status, winner).await?;
            done += 1;
        }
        tx.commit().await?;
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_kind_checks() {
        let human = Account {
            player_id: 1,
            username: "a".into(),
            bot: false,
            marked: false,
            created_ms: 0,
            seen_ms: None,
        };
        let bot = Account { bot: true, ..human.clone() };
        assert!(Api::Board.check(&human).is_ok());
        assert!(Api::Bot.check(&bot).is_ok());
        assert!(Api::Board.check(&bot).is_err());
        assert!(Api::Bot.check(&human).is_err());
    }
}
