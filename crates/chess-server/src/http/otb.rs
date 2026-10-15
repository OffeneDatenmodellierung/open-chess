//! Games on a real board, recorded live from the app.
//!
//! One of the players records the game on their phone: from a connected
//! board (Chessnut), from photos after each move, or by hand. Each move is
//! sent as it's made, so the game is followed live like any other (the
//! public stream, the live board overlay for a video stream). It ends when
//! the recorder says, or on the board (mate, stalemate…). A rated one is
//! rated, and reaches the feed, once the other player confirms the result
//! (`POST /api/games/{id}/confirm`), as with a game played on one device.
//!
//! * `POST /api/otb`: start one.
//! * `POST /api/otb/{id}/move`, `/undo`, `/end`.
//! * `GET /broadcast/{id}?key=`: the live board overlay (board, last moves,
//!   players, result) for a streaming app's browser source, and
//!   `GET /api/broadcast/{id}/events?key=` behind it. The key is the
//!   secret the recorder was given; nothing else about the game is open.

use std::collections::HashMap;
use std::convert::Infallible;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use chess_core::{Game, Side, Speed, Status};
use futures_util::stream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio_postgres::Transaction;

use crate::app::{Ctx, Who};
use crate::db::{Note, notify};
use crate::error::{ApiError, ApiResult};
use crate::model::{self, GameRow};
use crate::play;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Start {
    white: String,
    black: String,
    /// The rating pool: "blitz", "rapid", "classical"…
    speed: String,
    #[serde(default)]
    rated: bool,
    /// "chessnut", "photos" or "manual".
    board: String,
    /// Make a link for the live board overlay.
    #[serde(default)]
    broadcast: bool,
}

struct Otb {
    recorder: i64,
    opponent: i64,
    rated_requested: bool,
    broadcast_key: Option<String>,
}

async fn otb(ctx: &Ctx, tx: &impl tokio_postgres::GenericClient, id: &str) -> anyhow::Result<Option<Otb>> {
    Ok(tx
        .query_opt(
            &ctx.db.sql(
                "SELECT recorder_id, opponent_id, rated_requested, broadcast_key FROM chess.otb_games WHERE game_id = $1",
            ),
            &[&id],
        )
        .await?
        .map(|r| Otb { recorder: r.get(0), opponent: r.get(1), rated_requested: r.get(2), broadcast_key: r.get(3) }))
}

/// `POST /api/otb`: the recorder is one of the two players; the other
/// confirms the result.
pub async fn start(State(ctx): State<Ctx>, who: Who, Json(b): Json<Start>) -> ApiResult<Response> {
    let id = who.need(&["board:play"])?.clone();
    let speed =
        Speed::from_key(&b.speed).ok_or_else(|| ApiError::bad(format!("unknown speed {}", b.speed)))?;
    if !matches!(b.board.as_str(), "chessnut" | "photos" | "manual") {
        return Err(ApiError::bad("board is chessnut, photos or manual"));
    }
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let me = model::ensure_account(&ctx.db, &tx, id.player).await?;
    let find = async |name: &str| model::account_by_name(&ctx.db, &tx, name).await;
    let white = find(&b.white).await?.ok_or_else(|| ApiError::not_found(&format!("player {}", b.white)))?;
    let black = find(&b.black).await?.ok_or_else(|| ApiError::not_found(&format!("player {}", b.black)))?;
    if white.player_id == black.player_id {
        return Err(ApiError::bad("white and black are the same player"));
    }
    let opponent = if white.player_id == me.player_id {
        &black
    } else if black.player_id == me.player_id {
        &white
    } else {
        return Err(ApiError::bad("you record games you play in"));
    };
    if opponent.bot {
        return Err(ApiError::bad("the computer isn't played on a real board here"));
    }
    let perf = speed.key();
    let (wr, _, _) = model::rating(&ctx.db, &tx, white.player_id, perf).await?;
    let (br, _, _) = model::rating(&ctx.db, &tx, black.player_id, perf).await?;
    let game = Game::new(None).map_err(|e| anyhow::anyhow!("{e}"))?;
    let game_id = model::new_id();
    tx.execute(
        &ctx.db.sql(
            "INSERT INTO chess.games (id, white_id, black_id, variant, rated, source, speed, status, fen,
                    white_rating, black_rating, white_provisional, black_provisional, created_at, turn_started_at)
             VALUES ($1, $2, $3, 'standard', false, 'otb', $4, 20, $5, $6, $7, $8, $9, clock_timestamp(), clock_timestamp())",
        ),
        &[
            &game_id,
            &white.player_id,
            &black.player_id,
            &perf,
            &game.fen(),
            &wr.int(),
            &br.int(),
            &wr.provisional(),
            &br.provisional(),
        ],
    )
    .await?;
    let key = b.broadcast.then(|| crate::auth::random_token(24));
    tx.execute(
        &ctx.db.sql(
            "INSERT INTO chess.otb_games (game_id, recorder_id, opponent_id, board, rated_requested, broadcast_key)
             VALUES ($1, $2, $3, $4, $5, $6)",
        ),
        &[&game_id, &me.player_id, &opponent.player_id, &b.board, &b.rated, &key],
    )
    .await?;
    notify(&tx, &ctx.db, &Note::Game(game_id.clone())).await?;
    tx.commit().await?;
    ctx.metrics.games_started.inc();
    let base = ctx.cfg.public_url();
    Ok(Json(json!({
        "id": game_id,
        "url": format!("{base}/{game_id}"),
        "broadcastUrl": key.as_ref().map(|k| format!("{base}/broadcast/{game_id}?key={k}")),
    }))
    .into_response())
}

/// The game, locked, if `player` records it and it's still going.
async fn recording(
    ctx: &Ctx,
    tx: &Transaction<'_>,
    id: &str,
    player: i64,
) -> ApiResult<(GameRow, Otb, Game)> {
    let o = otb(ctx, tx, id).await?.ok_or_else(|| ApiError::not_found("game"))?;
    if o.recorder != player {
        return Err(ApiError::not_found("game"));
    }
    let row = model::game_for_update(&ctx.db, tx, id).await?.ok_or_else(|| ApiError::not_found("game"))?;
    if !row.status.is_ongoing() {
        return Err(ApiError::conflict("the game is over"));
    }
    let game =
        Game::from_moves(row.initial_fen.as_deref(), &row.moves).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok((row, o, game))
}

#[derive(Deserialize)]
pub struct Move {
    uci: String,
}

/// `POST /api/otb/{id}/move`: the next move, as played on the board.
pub async fn make_move(
    State(ctx): State<Ctx>,
    who: Who,
    Path(id): Path<String>,
    Json(m): Json<Move>,
) -> ApiResult<Response> {
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let (row, o, mut game) = recording(&ctx, &tx, &id, who.0.player).await?;
    let played = game.play_uci(m.uci.trim()).map_err(|e| ApiError::bad(e.to_string()))?;
    let mut move_ms = row.move_ms.clone();
    let mut clock_ms = row.clock_ms.clone();
    move_ms.push(row.elapsed_ms.max(0));
    clock_ms.push(model::NO_CLOCK_MS);
    let moves =
        if row.moves.is_empty() { played.uci.clone() } else { format!("{} {}", row.moves, played.uci) };
    tx.execute(
        &ctx.db.sql(
            "UPDATE chess.games SET moves = $2, ply = $3, fen = $4, move_ms = $5, clock_ms = $6,
                    turn_started_at = clock_timestamp(), last_move_at = clock_timestamp()
              WHERE id = $1",
        ),
        &[&id, &moves, &(row.ply + 1), &game.fen(), &move_ms, &clock_ms],
    )
    .await?;
    if let Some(o2) = game.outcome() {
        let row = model::game(&ctx.db, &tx, &id).await?.ok_or_else(|| ApiError::not_found("game"))?;
        end_game(&ctx, &tx, &row, &o, o2.status, o2.winner).await?;
    } else {
        notify(&tx, &ctx.db, &Note::Game(id.clone())).await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"san": played.san, "fen": game.fen(), "status": game.outcome().map_or("started", |o| o.status.name())}))
        .into_response())
}

#[derive(Deserialize)]
pub struct Undo {
    #[serde(default = "one")]
    plies: usize,
}

fn one() -> usize {
    1
}

/// `POST /api/otb/{id}/undo`: take back moves recorded wrongly.
pub async fn undo(
    State(ctx): State<Ctx>,
    who: Who,
    Path(id): Path<String>,
    Json(u): Json<Undo>,
) -> ApiResult<Response> {
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let (row, _, mut game) = recording(&ctx, &tx, &id, who.0.player).await?;
    let n = u.plies.min(game.ply());
    game.undo(n);
    let keep = game.ply();
    let moves = game.uci_moves().join(" ");
    let move_ms: Vec<i64> = row.move_ms.iter().take(keep).copied().collect();
    let clock_ms: Vec<i64> = row.clock_ms.iter().take(keep).copied().collect();
    tx.execute(
        &ctx.db.sql(
            "UPDATE chess.games SET moves = $2, ply = $3, fen = $4, move_ms = $5, clock_ms = $6,
                    turn_started_at = clock_timestamp()
              WHERE id = $1",
        ),
        &[&id, &moves, &i32::try_from(keep).unwrap_or(0), &game.fen(), &move_ms, &clock_ms],
    )
    .await?;
    notify(&tx, &ctx.db, &Note::Game(id.clone())).await?;
    tx.commit().await?;
    Ok(Json(json!({"fen": game.fen(), "ply": keep})).into_response())
}

#[derive(Deserialize)]
pub struct End {
    /// "resign", "draw", "outoftime" or "aborted".
    status: String,
    #[serde(default)]
    winner: Option<String>,
}

/// `POST /api/otb/{id}/end`: a result decided off the board (resignation,
/// an agreed draw, a flag on a real clock), or the game abandoned.
pub async fn end(
    State(ctx): State<Ctx>,
    who: Who,
    Path(id): Path<String>,
    Json(asked): Json<End>,
) -> ApiResult<Response> {
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let (row, rec, game) = recording(&ctx, &tx, &id, who.0.player).await?;
    let winner = match asked.winner.as_deref() {
        None | Some("") => None,
        Some(w) => Some(Side::from_name(w).ok_or_else(|| ApiError::bad(format!("winner {w}?")))?),
    };
    let (status, winner) = match (Status::from_name(&asked.status), winner) {
        (Some(s @ (Status::Resign | Status::Outoftime)), Some(w)) => {
            if s == Status::Outoftime && game.cannot_win(w) { (s, None) } else { (s, Some(w)) }
        }
        (Some(Status::Draw), _) => (Status::Draw, None),
        (Some(Status::Aborted), _) => (Status::Aborted, None),
        _ => return Err(ApiError::bad("status is resign or outoftime (with a winner), draw or aborted")),
    };
    let pending = end_game(&ctx, &tx, &row, &rec, status, winner).await?;
    tx.commit().await?;
    Ok(Json(json!({"status": status.name(), "winner": winner.map(Side::name), "confirmation": if pending { "pending" } else { "none" }}))
        .into_response())
}

/// Finish a real-board game. A rated one waits for the other player (no
/// rating, not in the feed) until they confirm. Whether it waits.
async fn end_game(
    ctx: &Ctx,
    tx: &Transaction<'_>,
    row: &GameRow,
    o: &Otb,
    status: Status,
    winner: Option<Side>,
) -> anyhow::Result<bool> {
    play::finish(ctx, tx, row, status, winner).await?;
    let waits = o.rated_requested && !status.is_void() && row.ply >= 2;
    if waits {
        tx.execute(&ctx.db.sql("UPDATE chess.games SET feed_at = NULL WHERE id = $1"), &[&row.id]).await?;
        tx.execute(
            &ctx.db.sql("UPDATE chess.otb_games SET confirm = 'pending' WHERE game_id = $1"),
            &[&row.id],
        )
        .await?;
        let recorder = row.player(if row.white.player_id == o.recorder { Side::White } else { Side::Black });
        crate::push::notify(
            ctx,
            o.opponent,
            "A result to confirm".into(),
            format!("{} recorded your game on a real board. Confirm it in the app.", recorder.username),
            Some(row.id.clone()),
            None,
        );
    }
    Ok(waits)
}

/// The other player's answer to a real-board game's result, from
/// `POST /api/games/{id}/confirm`. `None`: no such game waiting for them.
pub(crate) async fn confirm(ctx: &Ctx, player: i64, id: &str, accept: bool) -> ApiResult<Option<Value>> {
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let pending = tx
        .query_opt(
            &ctx.db.sql(
                "SELECT confirm FROM chess.otb_games WHERE game_id = $1 AND opponent_id = $2 FOR UPDATE",
            ),
            &[&id, &player],
        )
        .await?;
    let Some(p) = pending else { return Ok(None) };
    let state: String = p.get(0);
    if state != "pending" {
        return Err(ApiError::conflict(format!(
            "already {}",
            if state == "none" { "recorded" } else { &state }
        )));
    }
    let row = model::game_for_update(&ctx.db, &tx, id).await?.ok_or_else(|| ApiError::not_found("game"))?;
    let diff = if accept {
        let rated = GameRow { rated: true, ..row.clone() };
        let (wd, bd) = play::rate(ctx, &tx, &rated, row.winner).await?;
        tx.execute(
            &ctx.db.sql(
                "UPDATE chess.games SET rated = true, white_rating_diff = $2, black_rating_diff = $3, feed_at = clock_timestamp()
                  WHERE id = $1",
            ),
            &[&id, &wd, &bd],
        )
        .await?;
        if row.white.player_id == player { wd } else { bd }
    } else {
        tx.execute(&ctx.db.sql("UPDATE chess.games SET feed_at = clock_timestamp() WHERE id = $1"), &[&id])
            .await?;
        None
    };
    tx.execute(
        &ctx.db.sql("UPDATE chess.otb_games SET confirm = $2 WHERE game_id = $1"),
        &[&id, &(if accept { "confirmed" } else { "declined" })],
    )
    .await?;
    notify(&tx, &ctx.db, &Note::Game(id.to_owned())).await?;
    tx.commit().await?;
    Ok(Some(json!({"gameId": id, "status": if accept { "rated" } else { "declined" }, "ratingDiff": diff})))
}

/// Real-board games waiting for `player` to confirm, for `/api/sync`.
pub(crate) async fn waiting_for(
    ctx: &Ctx,
    c: &impl tokio_postgres::GenericClient,
    player: i64,
) -> anyhow::Result<Vec<Value>> {
    let rows = c
        .query(
            &ctx.db.sql(&format!(
                "{} JOIN chess.otb_games o ON o.game_id = g.id
                  WHERE o.opponent_id = $1 AND o.confirm = 'pending' ORDER BY g.finished_at",
                model::GAME_SELECT
            )),
            &[&player],
        )
        .await?;
    Ok(rows
        .iter()
        .map(GameRow::from_row)
        .map(|g| {
            let recorder = if g.white.player_id == player { &g.black } else { &g.white };
            json!({"id": g.id, "kind": "otb", "white": g.white.username, "black": g.black.username,
                   "winner": g.winner.map(Side::name), "status": g.status.name(), "speed": g.perf_key(),
                   "moves": g.ply, "uploadedBy": recorder.username, "createdAt": g.created_ms})
        })
        .collect())
}

/// What the overlay shows.
fn view(row: &GameRow) -> Value {
    let game = Game::from_moves(row.initial_fen.as_deref(), &row.moves).ok();
    let sans: Vec<String> = game.as_ref().map(|g| g.sans().to_vec()).unwrap_or_default();
    let first = sans.len().saturating_sub(10);
    json!({
        "id": row.id, "fen": row.fen, "lastMove": row.last_move(), "ply": row.ply,
        "sans": &sans[first..], "firstPly": first,
        "white": {"name": row.white.username, "rating": row.white_rating},
        "black": {"name": row.black.username, "rating": row.black_rating},
        "status": row.status.name(), "winner": row.winner.map(Side::name),
    })
}

/// The game, if `key` is its broadcast key.
async fn broadcast_game(ctx: &Ctx, id: &str, key: Option<&String>) -> ApiResult<GameRow> {
    let c = ctx.db.conn().await?;
    let o = otb(ctx, &*c, id).await?.ok_or_else(|| ApiError::not_found("broadcast"))?;
    let ok = match (o.broadcast_key.as_deref(), key) {
        (Some(k), Some(given)) => crate::auth::hash_key(k) == crate::auth::hash_key(given),
        _ => false,
    };
    if !ok {
        return Err(ApiError::not_found("broadcast"));
    }
    model::game(&ctx.db, &*c, id).await?.ok_or_else(|| ApiError::not_found("broadcast"))
}

/// `GET /api/broadcast/{id}/events?key=`: the game as server-sent `state`
/// events, now and after every move.
pub async fn events(
    State(ctx): State<Ctx>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let row = broadcast_game(&ctx, &id, q.get("key")).await?;
    let mut notes = ctx.db.subscribe();
    let (tx, rx) = mpsc::channel::<Event>(64);
    let first = Event::default().event("state").json_data(view(&row)).map_err(|e| anyhow::anyhow!("{e}"))?;
    let _ = tx.send(first).await;
    let ctx2 = ctx.clone();
    tokio::spawn(async move {
        loop {
            match notes.recv().await {
                Ok(Note::Game(g)) if g == id => {}
                Ok(_) | Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            }
            let Ok(c) = ctx2.db.conn().await else { continue };
            let Ok(Some(row)) = model::game(&ctx2.db, &*c, &id).await else { continue };
            let Ok(e) = Event::default().event("state").json_data(view(&row)) else { continue };
            if tx.send(e).await.is_err() {
                return;
            }
        }
    });
    let events =
        stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|e| (Ok::<_, Infallible>(e), rx)) });
    Ok(Sse::new(events).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))).into_response())
}

/// `GET /broadcast/{id}?key=`: the overlay page (`bg=transparent` for a
/// streaming app's browser source; `theme=light`).
pub async fn page(
    State(ctx): State<Ctx>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    broadcast_game(&ctx, &id, q.get("key")).await?;
    let html = OVERLAY.replace("{{base}}", &ctx.cfg.public_url().replace('"', ""));
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], html)
        .into_response())
}

/// The overlay: the board, the last five moves, the players and the
/// result, in the esports colours by default. Reads its id and key from
/// its own address.
const OVERLAY: &str = include_str!("overlay.html");
