//! `GET /api/feed`: every game on the server as server-sent events, for
//! sites and dashboards that follow it (esports-lab follows it for its
//! ratings and pricing). No token needed; it shows what the public game
//! streams show.
//!
//! Events:
//!
//! * `start`: a game began (the public game description, as the first
//!   line of `/api/stream/game/{id}`).
//! * `move`: it moved on (the same description: position, last move,
//!   ply). While a game is live its moves come
//!   `CHESS_PUBLIC_DELAY_PLIES` behind, as on the public stream.
//! * `end`: it finished: the game as `/game/export/{id}` gives it in JSON
//!   (players, ratings and rating changes, status, winner, moves, clock).
//!   Its SSE id is when the server recorded it finished (Unix ms): the
//!   finish for a game played here, the upload for a game played offline
//!   in the iOS app (which may have finished days before).
//!
//! A client that reconnects with `Last-Event-ID` (browsers do it
//! themselves), or asks with `?since=<Unix ms>`, first gets the `end` of
//! every game recorded since then, oldest first, then the live events, so a
//! consumer that keeps the last id misses nothing, late uploads included.
//! `?game=<id>` follows one game; `?only=end` sends only finished games.
//! A comment line every 15 seconds keeps proxies from closing the stream.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use chess_core::Game;
use futures_util::stream;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;

use crate::app::{Ctx, Reader};
use crate::db::Note;
use crate::error::ApiResult;
use crate::http::{games, streams};
use crate::metrics::StreamGuard;
use crate::model::{self, GAME_SELECT, GameRow};

/// Finished games sent per catch-up query.
const PAGE: i64 = 200;

#[derive(Debug, Clone, Default)]
struct Filter {
    game: Option<String>,
    only_end: bool,
}

impl Filter {
    fn wants(&self, id: &str) -> bool {
        self.game.as_deref().is_none_or(|g| g == id)
    }
}

fn end_event(ctx: &Ctx, row: &GameRow) -> Option<Event> {
    let game = Game::from_moves(row.initial_fen.as_deref(), &row.moves).ok()?;
    let json = games::json(row, &game, &HashMap::new(), &ctx.cfg);
    let mut e = Event::default().event("end").json_data(json).ok()?;
    if let Some(ms) = row.feed_ms.or(row.finished_ms) {
        e = e.id(ms.to_string());
    }
    Some(e)
}

fn view_event(kind: &str, ctx: &Ctx, row: &GameRow) -> Option<Event> {
    Event::default().event(kind).json_data(streams::describe(row, ctx.cfg.public_delay_plies)).ok()
}

pub async fn feed(
    State(ctx): State<Ctx>,
    _reader: Reader,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let filter = Filter {
        game: q.get("game").filter(|g| !g.is_empty()).cloned(),
        only_end: q.get("only").is_some_and(|o| o == "end"),
    };
    let since: Option<i64> = h
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .or_else(|| q.get("since").map(String::as_str))
        .and_then(|s| s.trim().parse().ok());
    // Subscribed before catching up, so nothing falls between the two.
    let notes = ctx.db.subscribe();
    let (tx, rx) = mpsc::channel::<Event>(256);
    tokio::spawn(run(ctx, filter, since, notes, tx));
    let events =
        stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|e| (Ok::<_, Infallible>(e), rx)) });
    Ok(Sse::new(events).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))).into_response())
}

async fn run(
    ctx: Ctx,
    filter: Filter,
    since: Option<i64>,
    mut notes: tokio::sync::broadcast::Receiver<Note>,
    tx: mpsc::Sender<Event>,
) {
    let _g = StreamGuard::new(&ctx.metrics, "feed");
    // Games we've reported finished (an `end` isn't sent twice).
    let mut ended: HashSet<String> = HashSet::new();
    // Plies seen of the live games we've reported.
    let mut seen: HashMap<String, i32> = HashMap::new();
    if let Some(mut after) = since {
        loop {
            let rows = match catch_up(&ctx, &filter, after).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("feed catch-up: {e:#}");
                    break;
                }
            };
            let n = rows.len();
            for row in rows {
                after = after.max(row.feed_ms.or(row.finished_ms).unwrap_or(after));
                if let Some(e) = end_event(&ctx, &row)
                    && ended.insert(row.id.clone())
                    && tx.send(e).await.is_err()
                {
                    return;
                }
            }
            if i64::try_from(n).unwrap_or(0) < PAGE {
                break;
            }
            // The next page starts after the last one's time.
            after += 1;
        }
    }
    loop {
        let id = match notes.recv().await {
            Ok(Note::Game(id)) if filter.wants(&id) => id,
            Ok(_) | Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => return,
        };
        let row = match ctx.db.conn().await {
            Ok(c) => match model::game(&ctx.db, &*c, &id).await {
                Ok(Some(r)) => r,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!("feed: {e:#}");
                    continue;
                }
            },
            Err(e) => {
                tracing::warn!("feed: {e:#}");
                continue;
            }
        };
        let event = if !row.status.is_ongoing() {
            seen.remove(&row.id);
            if !ended.insert(row.id.clone()) {
                continue;
            }
            end_event(&ctx, &row)
        } else if filter.only_end {
            continue;
        } else {
            match seen.insert(row.id.clone(), row.ply) {
                None => view_event("start", &ctx, &row),
                Some(ply) if ply != row.ply => view_event("move", &ctx, &row),
                Some(_) => continue,
            }
        };
        if let Some(e) = event
            && tx.send(e).await.is_err()
        {
            return;
        }
        // Keep the bookkeeping small on a long-lived stream.
        if ended.len() > 50_000 {
            ended.clear();
        }
    }
}

/// Games recorded finished at or after `after` (Unix ms), oldest first.
async fn catch_up(ctx: &Ctx, filter: &Filter, after: i64) -> anyhow::Result<Vec<GameRow>> {
    let c = ctx.db.conn().await?;
    #[allow(clippy::cast_precision_loss)] // ms since 1970 fit an f64 exactly
    let at = after as f64 / 1000.0;
    let rows = c
        .query(
            &ctx.db.sql(&format!(
                "{GAME_SELECT} WHERE g.feed_at >= to_timestamp($1) AND ($2::text IS NULL OR g.id = $2)
                  ORDER BY g.feed_at, g.id LIMIT {PAGE}"
            )),
            &[&at, &filter.game],
        )
        .await?;
    Ok(rows.iter().map(GameRow::from_row).collect())
}
