//! Game export: PGN by default, JSON with `Accept: application/json`.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use chess_core::{Game, PgnGame, PgnPlayer, Side, pgn};
use chess_proto as p;

use crate::app::{Ctx, Reader};
use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use crate::model::{self, GameRow};

fn flag(q: &HashMap<String, String>, k: &str, default: bool) -> bool {
    q.get(k).map_or(default, |v| matches!(v.as_str(), "true" | "1" | "yes"))
}

fn pgn_text(row: &GameRow, game: &Game, cfg: &Config, clocks: bool) -> String {
    let public_url = cfg.public_url();
    let player = |s: Side| {
        let a = row.player(s);
        let (rating, _, diff) = row.rating(s);
        PgnPlayer { name: a.username.clone(), rating: Some(rating), rating_diff: diff, title: a.title() }
    };
    let first_fullmove = row
        .initial_fen
        .as_deref()
        .and_then(|f| f.split_whitespace().nth(5))
        .and_then(|n| n.parse().ok())
        .unwrap_or(1);
    let event = format!("{} {} game", cfg.site_name, if row.rated { "rated" } else { "casual" });
    let clocks_ms: Vec<i64> = if clocks && row.has_clock() { row.clock_ms.clone() } else { Vec::new() };
    pgn(&PgnGame {
        event: &event,
        site: format!("{public_url}/{}", row.id),
        game_id: &row.id,
        created_at_ms: row.created_ms,
        white: player(Side::White),
        black: player(Side::Black),
        status: row.status,
        winner: row.winner,
        clock: row
            .clock_initial_ms
            .map(|i| ((i / 1000) as u32, (row.clock_increment_ms.unwrap_or(0) / 1000) as u32)),
        initial_fen: row.initial_fen.as_deref(),
        sans: game.sans(),
        first_to_move: game.first_to_move(),
        first_fullmove,
        clocks_ms: &clocks_ms,
    })
}

pub(crate) fn json(row: &GameRow, game: &Game, q: &HashMap<String, String>, cfg: &Config) -> p::GameJson {
    let player = |s: Side| {
        let a = row.player(s);
        let (rating, prov, diff) = row.rating(s);
        p::GamePlayer {
            user: p::LightUser { id: a.id(), name: a.username.clone(), title: a.title() },
            rating,
            rating_diff: diff,
            provisional: prov.then_some(true),
            analysis: None,
        }
    };
    p::GameJson {
        id: row.id.clone(),
        rated: row.rated,
        variant: row.variant.clone(),
        speed: row.speed,
        perf: row.perf_key().into(),
        created_at: row.created_ms,
        last_move_at: row.last_move_ms.or(row.finished_ms).unwrap_or(row.created_ms),
        status: row.status,
        source: row.source.clone(),
        players: p::GamePlayers { white: player(Side::White), black: player(Side::Black) },
        initial_fen: row.initial_fen.clone(),
        winner: row.winner,
        moves: flag(q, "moves", true).then(|| game.sans().join(" ")),
        pgn: flag(q, "pgnInJson", false).then(|| pgn_text(row, game, cfg, flag(q, "clocks", false))),
        clock: row.clock_initial_ms.map(|i| {
            let inc = row.clock_increment_ms.unwrap_or(0) / 1000;
            p::ExportClock { initial: i / 1000, increment: inc, total_time: i / 1000 + 40 * inc }
        }),
        clocks: (flag(q, "clocks", false) && row.has_clock())
            .then(|| row.clock_ms.iter().map(|ms| ms / 10).collect()),
    }
}

pub async fn export(
    State(ctx): State<Ctx>,
    _reader: Reader,
    h: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let c = ctx.db.conn().await?;
    let row = model::game(&ctx.db, &*c, &id).await?.ok_or_else(|| ApiError::not_found("game"))?;
    let game =
        Game::from_moves(row.initial_fen.as_deref(), &row.moves).map_err(|e| anyhow::anyhow!("{e}"))?;
    let wants_json = h
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("application/json") || a.contains("application/x-ndjson"));
    if wants_json {
        return Ok(Json(json(&row, &game, &q, &ctx.cfg)).into_response());
    }
    let mut r = pgn_text(&row, &game, &ctx.cfg, flag(&q, "clocks", false)).into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/x-chess-pgn"));
    Ok(r)
}

/// `GET /api/games/user/{username}`: a player's games, newest first, as
/// NDJSON (Lichess's shape; `max`, default 20, up to 100; `ongoing=false`
/// leaves out games in progress).
pub async fn by_user(
    State(ctx): State<Ctx>,
    _reader: Reader,
    Path(username): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let c = ctx.db.conn().await?;
    let a =
        model::account_by_name(&ctx.db, &*c, &username).await?.ok_or_else(|| ApiError::not_found("user"))?;
    let max: i64 = q.get("max").and_then(|m| m.parse().ok()).unwrap_or(20).clamp(1, 100);
    let ongoing = flag(&q, "ongoing", true);
    let rows = c
        .query(
            &ctx.db.sql(&format!(
                "{} WHERE (g.white_id = $1 OR g.black_id = $1) AND ($2 OR g.status NOT IN (10, 20))
                 ORDER BY g.created_at DESC LIMIT $3",
                model::GAME_SELECT
            )),
            &[&a.player_id, &ongoing, &max],
        )
        .await?;
    drop(c);
    let (tx, rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(rows.len().max(1));
    for r in &rows {
        let row = GameRow::from_row(r);
        let Ok(game) = Game::from_moves(row.initial_fen.as_deref(), &row.moves) else { continue };
        let _ = tx.try_send(crate::http::line(&json(&row, &game, &q, &ctx.cfg)));
    }
    drop(tx);
    Ok(crate::http::ndjson(rx))
}
