//! NDJSON streams: a player's event stream, a game's board/bot stream and
//! the public move stream.
//!
//! Each stream is a task writing lines into a channel; it ends when the
//! client goes (a send fails) or, for a game, when the game is over.
//! Changes reach every replica through Postgres notifications; a stream
//! reloads the game from the database on each one, so it never relies on
//! a note's contents for state.

use std::collections::HashSet;
use std::time::Duration;

use axum::extract::{MatchedPath, Path, State};
use axum::response::Response;
use bytes::Bytes;
use chess_core::{Game, Side};
use chess_proto as p;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::app::{Ctx, Who};
use crate::db::Note;
use crate::error::{ApiError, ApiResult};
use crate::http::{line, ndjson};
use crate::metrics::StreamGuard;
use crate::model::{self, GameRow};
use crate::play::{self, Api};

const KEEPALIVE: Duration = Duration::from_secs(6);

// ─── /api/stream/event ───

pub async fn events(State(ctx): State<Ctx>, who: Who) -> ApiResult<Response> {
    let id = who.need(&["challenge:read", "bot:play", "board:play"])?.clone();
    let c = ctx.db.conn().await?;
    model::ensure_account(&ctx.db, &*c, id.player).await?;
    drop(c);
    let slot = ctx.slot(id.player, "event")?;
    let (tx, rx) = mpsc::channel::<Bytes>(64);
    tokio::spawn(async move {
        event_stream(ctx, id.player, tx).await;
        drop(slot);
    });
    Ok(ndjson(rx))
}

/// What one event stream has told its client, so a catch-up after missed
/// notifications sends what's missing and nothing twice.
#[derive(Default)]
struct Sent {
    games: HashSet<String>,
    challenges: HashSet<String>,
    /// Games and challenges whose end (gameFinish, challengeDeclined,
    /// challengeCanceled, or a challenge turned into a game) was sent.
    ended: HashSet<String>,
}

impl Sent {
    /// Record an event; false if it was already sent.
    fn note(&mut self, event: &serde_json::Value) -> bool {
        let kind = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let game = event.pointer("/game/id").and_then(|v| v.as_str()).map(str::to_owned);
        let challenge = event.pointer("/challenge/id").and_then(|v| v.as_str()).map(str::to_owned);
        match (kind, game, challenge) {
            (p::GameEvent::START, Some(g), _) => self.games.insert(g),
            (p::GameEvent::FINISH, Some(g), _) => {
                self.games.insert(g.clone());
                self.ended.insert(g)
            }
            (p::ChallengeEvent::CREATED, _, Some(c)) => self.challenges.insert(c),
            (p::ChallengeEvent::DECLINED | p::ChallengeEvent::CANCELED, _, Some(c)) => {
                self.challenges.insert(c.clone());
                self.ended.insert(c)
            }
            _ => true,
        }
    }
}

async fn event_stream(ctx: Ctx, player: i64, tx: mpsc::Sender<Bytes>) {
    let _g = StreamGuard::new(&ctx.metrics, "event");
    // Subscribe first, so nothing between the catch-up and the loop is lost.
    let mut notes = ctx.db.subscribe();
    let mut sent = Sent::default();
    if catch_up(&ctx, player, &tx, &mut sent).await.is_err() {
        return;
    }
    let mut keep = tokio::time::interval(KEEPALIVE);
    let mut seen = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            n = notes.recv() => match n {
                Ok(Note::User { player: p, event }) if p == player => {
                    if sent.note(&event) && tx.send(line(&event)).await.is_err() {
                        break;
                    }
                }
                Ok(Note::Resync) | Err(RecvError::Lagged(_)) => {
                    if catch_up(&ctx, player, &tx, &mut sent).await.is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(RecvError::Closed) => break,
            },
            _ = keep.tick() => {
                if tx.send(Bytes::from_static(b"\n")).await.is_err() {
                    break;
                }
            }
            _ = seen.tick() => {
                if let Ok(c) = ctx.db.conn().await {
                    let _ = c.execute(&ctx.db.sql("UPDATE chess.accounts SET seen_at = now() WHERE player_id = $1"), &[&player]).await;
                }
            }
        }
    }
    debug!(player, "event stream closed");
}

/// Bring the client up to date from the database: ongoing games
/// (gameStart), open challenges, and the ends of games and challenges it
/// was told about whose end notification may have been missed.
async fn catch_up(ctx: &Ctx, player: i64, tx: &mpsc::Sender<Bytes>, sent: &mut Sent) -> Result<(), ()> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    let r: anyhow::Result<()> = async {
        let c = ctx.db.conn().await?;
        c.execute(&ctx.db.sql("UPDATE chess.accounts SET seen_at = now() WHERE player_id = $1"), &[&player])
            .await?;
        let ongoing = model::ongoing_games(&ctx.db, &*c, player).await?;
        let live: HashSet<String> = ongoing.iter().map(|g| g.id.clone()).collect();
        out.extend(ongoing.iter().filter_map(|g| g.event(player, p::GameEvent::START)));
        // Games we announced that have ended since.
        let gone: Vec<String> =
            sent.games.iter().filter(|g| !live.contains(*g) && !sent.ended.contains(*g)).cloned().collect();
        for id in gone {
            if let Some(g) = model::game(&ctx.db, &*c, &id).await?
                && !g.status.is_ongoing()
                && let Some(e) = g.event(player, p::GameEvent::FINISH)
            {
                out.push(e);
            }
        }
        let (incoming, outgoing) = crate::challenges::list(ctx, player).await?;
        let open: HashSet<String> = incoming.iter().chain(&outgoing).map(|c| c.id.clone()).collect();
        for ch in incoming.into_iter().chain(outgoing) {
            out.push(json!({"type": "challenge", "challenge": ch, "compat": {"bot": true, "board": true}}));
        }
        // Challenges we announced that were answered since.
        let answered: Vec<String> = sent
            .challenges
            .iter()
            .filter(|c| !open.contains(*c) && !sent.ended.contains(*c))
            .cloned()
            .collect();
        for id in answered {
            let Some(ch) = model::challenge(&ctx.db, &*c, &id, false).await? else { continue };
            let kind = match ch.status.as_str() {
                "declined" => p::ChallengeEvent::DECLINED,
                "canceled" | "offline" => p::ChallengeEvent::CANCELED,
                _ => {
                    // Accepted: its gameStart (above, or already sent) says so.
                    sent.ended.insert(id);
                    continue;
                }
            };
            let ratings = model::challenge_ratings(&ctx.db, &*c, &ch).await?;
            out.push(
                json!({"type": kind, "challenge": ch.json(ctx.cfg.public_url(), Some(player), ratings)}),
            );
        }
        Ok(())
    }
    .await;
    if let Err(e) = r {
        warn!("event stream catch-up: {e:#}");
    }
    for e in out {
        if sent.note(&e) && tx.send(line(&e)).await.is_err() {
            return Err(());
        }
    }
    Ok(())
}

// ─── /api/{board,bot}/game/stream/{id} ───

pub async fn board_game(
    State(ctx): State<Ctx>,
    mp: MatchedPath,
    who: Who,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    game_stream(ctx, &mp, who, id, Api::Board).await
}

pub async fn bot_game(
    State(ctx): State<Ctx>,
    mp: MatchedPath,
    who: Who,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    game_stream(ctx, &mp, who, id, Api::Bot).await
}

async fn game_stream(ctx: Ctx, _mp: &MatchedPath, who: Who, id: String, api: Api) -> ApiResult<Response> {
    let scope = if api == Api::Bot { "bot:play" } else { "board:play" };
    let me = who.need(&[scope])?.player;
    // Subscribe before loading, so a change made in between still arrives
    // (at worst as a reload that finds nothing new).
    let notes = ctx.db.subscribe();
    let c = ctx.db.conn().await?;
    let row = model::game(&ctx.db, &*c, &id).await?.ok_or_else(|| ApiError::not_found("game"))?;
    drop(c);
    let side = row.side_of(me).ok_or_else(|| ApiError::not_found("game"))?;
    api.check(row.player(side))?;
    let slot = ctx.slot(me, "game")?;
    let (tx, rx) = mpsc::channel::<Bytes>(64);
    let first = line(&row.full(ctx.first_move_ms()));
    let _ = tx.send(first).await;
    if row.status.is_ongoing() {
        tokio::spawn(async move {
            follow_game(ctx, row, me, side, tx, notes).await;
            drop(slot);
        });
    }
    Ok(ndjson(rx))
}

async fn follow_game(
    ctx: Ctx,
    mut row: GameRow,
    me: i64,
    side: Side,
    tx: mpsc::Sender<Bytes>,
    mut notes: tokio::sync::broadcast::Receiver<Note>,
) {
    let _g = StreamGuard::new(&ctx.metrics, "game");
    let fm = ctx.first_move_ms();
    let opp = row.player(side.opposite()).player_id;
    let state_key = |r: &GameRow| serde_json::to_string(&strip_clock(r.state(fm))).unwrap_or_default();
    let mut last = state_key(&row);
    let mut gone: Option<Option<i64>> = None;
    let _ = play::touch(&ctx, &row.id, me).await;
    let mut keep = tokio::time::interval(KEEPALIVE);
    // Refresh presence well inside the "gone" window, however it's set.
    let beat_ms = (ctx.cfg.gone_after_secs * 1000 / 4).clamp(250, 5000);
    let mut beat = tokio::time::interval(Duration::from_millis(beat_ms));
    let mut check = tokio::time::interval(Duration::from_secs(2));
    loop {
        let reload = tokio::select! {
            n = notes.recv() => match n {
                Ok(Note::Game(g)) => g == row.id,
                Ok(Note::Chat { game, line: l }) if game == row.id => {
                    if tx.send(line(&l)).await.is_err() {
                        break;
                    }
                    false
                }
                Ok(Note::Resync) | Err(RecvError::Lagged(_)) => true,
                Ok(_) => false,
                Err(RecvError::Closed) => break,
            },
            _ = keep.tick() => {
                if tx.send(Bytes::from_static(b"\n")).await.is_err() {
                    break;
                }
                false
            }
            _ = beat.tick() => {
                let _ = play::touch(&ctx, &row.id, me).await;
                false
            }
            _ = check.tick() => {
                // Is the opponent still here?
                if let Ok(c) = ctx.db.conn().await
                    && let Ok(absent) = play::absent_secs(&ctx, &*c, &row, opp).await
                {
                    let now = play::claim_in(&ctx, absent);
                    let changed = match (gone, now) {
                        (None, _) => now.is_some(),
                        (Some(a), b) => a.is_some() != b.is_some() || (a != Some(0) && b == Some(0)),
                    };
                    gone = Some(now);
                    if changed {
                        let ev = p::OpponentGone {
                            kind: p::OpponentGone::TYPE.into(),
                            gone: now.is_some(),
                            claim_win_in_seconds: now,
                        };
                        if tx.send(line(&ev)).await.is_err() {
                            break;
                        }
                    }
                }
                false
            }
        };
        if !reload {
            continue;
        }
        let fresh = match ctx.db.conn().await {
            Ok(c) => model::game(&ctx.db, &*c, &row.id).await,
            Err(e) => Err(e),
        };
        match fresh {
            Ok(Some(r)) => row = r,
            Ok(None) => break,
            Err(e) => {
                warn!("game stream reload: {e:#}");
                continue;
            }
        }
        let key = state_key(&row);
        if key != last {
            last = key;
            if tx.send(line(&row.state(fm))).await.is_err() {
                break;
            }
        }
        if !row.status.is_ongoing() {
            break;
        }
    }
}

/// A state without live clocks, to tell real changes from time passing.
fn strip_clock(mut s: p::GameState) -> p::GameState {
    s.wtime = 0;
    s.btime = 0;
    s.expiration = None;
    s
}

// ─── /api/stream/game/{id} (public, delayed while live) ───

pub async fn moves(State(ctx): State<Ctx>, Path(id): Path<String>) -> ApiResult<Response> {
    let notes = ctx.db.subscribe();
    let c = ctx.db.conn().await?;
    let row = model::game(&ctx.db, &*c, &id).await?.ok_or_else(|| ApiError::not_found("game"))?;
    drop(c);
    let (tx, rx) = mpsc::channel::<Bytes>(64);
    tokio::spawn(move_stream(ctx, row, tx, notes));
    Ok(ndjson(rx))
}

/// The game description line. `delay` plies are held back while the game
/// is live, so the position shown is as old as the move lines.
fn describe(row: &GameRow, delay: usize) -> serde_json::Value {
    let lines = move_lines(row);
    let shown = if row.status.is_ongoing() { lines.len().saturating_sub(delay).max(1) } else { lines.len() };
    let at = lines.get(shown.saturating_sub(1));
    let first = chess_core::Game::new(row.initial_fen.as_deref()).map_or(Side::White, |g| g.first_to_move());
    let turns = shown.saturating_sub(1);
    let player = |s: Side| {
        let a = row.player(s);
        let (rating, prov, diff) = row.rating(s);
        let mut v = json!({"user": {"name": a.username, "id": a.id()}, "rating": rating});
        if let Some(t) = a.title() {
            v["user"]["title"] = json!(t);
        }
        if prov {
            v["provisional"] = json!(true);
        }
        if let Some(d) = diff {
            v["ratingDiff"] = json!(d);
        }
        v
    };
    let mut v = json!({
        "id": row.id,
        "variant": row.variant(),
        "speed": row.speed,
        "perf": row.perf_key(),
        "rated": row.rated,
        "initialFen": row.initial_fen.clone().unwrap_or_else(|| chess_core::STARTPOS_FEN.into()),
        "fen": at.map_or_else(|| row.fen.clone(), |l| l.fen.clone()),
        "player": Side::at_ply(first, turns),
        "turns": turns,
        "source": row.source,
        "status": p::StatusRef::from(row.status),
        "createdAt": row.created_ms,
        "players": {"white": player(Side::White), "black": player(Side::Black)},
    });
    if let Some(w) = row.winner {
        v["winner"] = json!(w);
    }
    if let Some(lm) = at.and_then(|l| l.lm.as_ref()) {
        v["lastMove"] = json!(lm);
    }
    if let (Some(i), Some(inc)) = (row.clock_initial_ms, row.clock_increment_ms) {
        v["clock"] =
            json!({"initial": i / 1000, "increment": inc / 1000, "totalTime": i / 1000 + 40 * inc / 1000});
    }
    v
}

/// FEN after each ply and both clocks (seconds) at that point.
fn move_lines(row: &GameRow) -> Vec<p::MoveLine> {
    let Ok(mut g) = Game::new(row.initial_fen.as_deref()) else { return Vec::new() };
    let first = g.first_to_move();
    let init = row.clock_initial_ms.unwrap_or(0) / 1000;
    let (mut wc, mut bc) = (init, init);
    let mut out = vec![p::MoveLine { fen: g.fen(), lm: None, wc, bc }];
    for (i, m) in row.moves.split_whitespace().enumerate() {
        let Ok(played) = g.play_uci(m) else { break };
        if row.has_clock()
            && let Some(ms) = row.clock_ms.get(i)
        {
            match Side::at_ply(first, i) {
                Side::White => wc = ms / 1000,
                Side::Black => bc = ms / 1000,
            }
        }
        out.push(p::MoveLine { fen: g.fen(), lm: Some(played.uci), wc, bc });
    }
    out
}

async fn move_stream(
    ctx: Ctx,
    mut row: GameRow,
    tx: mpsc::Sender<Bytes>,
    mut notes: tokio::sync::broadcast::Receiver<Note>,
) {
    let _g = StreamGuard::new(&ctx.metrics, "moves");
    let delay = ctx.cfg.public_delay_plies;
    if tx.send(line(&describe(&row, delay))).await.is_err() {
        return;
    }
    // Lines sent so far (the starting position is line 0).
    let mut sent = 0usize;
    let mut keep = tokio::time::interval(KEEPALIVE);
    loop {
        let lines = move_lines(&row);
        let visible =
            if row.status.is_ongoing() { lines.len().saturating_sub(delay).max(1) } else { lines.len() };
        for l in lines.iter().take(visible).skip(sent) {
            if tx.send(line(l)).await.is_err() {
                return;
            }
        }
        sent = sent.max(visible);
        if !row.status.is_ongoing() {
            let _ = tx.send(line(&describe(&row, delay))).await;
            return;
        }
        let reload = loop {
            tokio::select! {
                n = notes.recv() => match n {
                    Ok(Note::Game(g)) if g == row.id => break true,
                    Ok(Note::Resync) | Err(RecvError::Lagged(_)) => break true,
                    Err(RecvError::Closed) => return,
                    Ok(_) => {}
                },
                _ = keep.tick() => {
                    if tx.send(Bytes::from_static(b"\n")).await.is_err() {
                        return;
                    }
                }
            }
        };
        if reload {
            match ctx.db.conn().await {
                Ok(c) => match model::game(&ctx.db, &*c, &row.id).await {
                    Ok(Some(r)) => row = r,
                    Ok(None) => return,
                    Err(e) => warn!("move stream reload: {e:#}"),
                },
                Err(e) => warn!("move stream reload: {e:#}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sent_events_are_not_repeated() {
        let mut s = Sent::default();
        let start = json!({"type": "gameStart", "game": {"id": "g1"}});
        let finish = json!({"type": "gameFinish", "game": {"id": "g1"}});
        let ch = json!({"type": "challenge", "challenge": {"id": "c1"}});
        let declined = json!({"type": "challengeDeclined", "challenge": {"id": "c1"}});
        assert!(s.note(&start));
        assert!(!s.note(&start), "a gameStart once");
        assert!(s.note(&finish));
        assert!(!s.note(&finish), "a gameFinish once");
        assert!(s.note(&ch) && s.note(&declined) && !s.note(&declined));
        assert!(s.note(&json!({"type": "somethingNew"})), "unknown kinds pass");
        assert!(s.ended.contains("g1") && s.ended.contains("c1"));
    }
}
