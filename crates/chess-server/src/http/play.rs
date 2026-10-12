//! Board API and Bot API game actions (the same handlers serve both; the
//! matched path says which).

use std::collections::HashMap;

use axum::Json;
use axum::extract::{MatchedPath, Path, Query, State};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde_json::json;

use crate::app::{Ctx, Who};
use crate::db::{Note, notify};
use crate::error::{ApiError, ApiResult};
use crate::http::{form, ok};
use crate::model;
use crate::play::{self, Api};

/// Which API was called, and the token scope it needs.
fn api(path: &MatchedPath, who: &Who) -> ApiResult<(Api, i64)> {
    let (api, scope) =
        if path.as_str().contains("/api/bot/") { (Api::Bot, "bot:play") } else { (Api::Board, "board:play") };
    let id = who.need(&[scope])?;
    Ok((api, id.player))
}

fn yes(s: &str) -> ApiResult<bool> {
    match s {
        "yes" | "true" | "accept" => Ok(true),
        "no" | "false" | "decline" => Ok(false),
        _ => Err(ApiError::bad("say yes or no")),
    }
}

pub async fn make_move(
    State(ctx): State<Ctx>,
    mp: MatchedPath,
    who: Who,
    Path((id, uci)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let (api, player) = api(&mp, &who)?;
    let offering = q.get("offeringDraw").is_some_and(|v| v == "true" || v == "1");
    play::make_move(&ctx, &id, player, api, &uci, offering).await?;
    Ok(ok())
}

pub async fn resign(
    State(ctx): State<Ctx>,
    mp: MatchedPath,
    who: Who,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let (api, player) = api(&mp, &who)?;
    play::resign(&ctx, &id, player, api).await?;
    Ok(ok())
}

pub async fn abort(
    State(ctx): State<Ctx>,
    mp: MatchedPath,
    who: Who,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let (api, player) = api(&mp, &who)?;
    play::abort(&ctx, &id, player, api).await?;
    Ok(ok())
}

pub async fn draw(
    State(ctx): State<Ctx>,
    mp: MatchedPath,
    who: Who,
    Path((id, accept)): Path<(String, String)>,
) -> ApiResult<Response> {
    let (api, player) = api(&mp, &who)?;
    play::draw(&ctx, &id, player, api, yes(&accept)?).await?;
    Ok(ok())
}

pub async fn takeback(
    State(ctx): State<Ctx>,
    mp: MatchedPath,
    who: Who,
    Path((id, accept)): Path<(String, String)>,
) -> ApiResult<Response> {
    let (api, player) = api(&mp, &who)?;
    play::takeback(&ctx, &id, player, api, yes(&accept)?).await?;
    Ok(ok())
}

pub async fn claim_victory(
    State(ctx): State<Ctx>,
    mp: MatchedPath,
    who: Who,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let (api, player) = api(&mp, &who)?;
    play::claim_victory(&ctx, &id, player, api).await?;
    Ok(ok())
}

pub async fn claim_draw(
    State(ctx): State<Ctx>,
    mp: MatchedPath,
    who: Who,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let (api, player) = api(&mp, &who)?;
    play::claim_draw(&ctx, &id, player, api).await?;
    Ok(ok())
}

pub async fn chat_post(
    State(ctx): State<Ctx>,
    mp: MatchedPath,
    who: Who,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let (api, player) = api(&mp, &who)?;
    let f = form(&body);
    let room = f.get("room").map_or("player", String::as_str);
    if !matches!(room, "player" | "spectator") {
        return Err(ApiError::bad("room is player or spectator"));
    }
    let text: String = f.get("text").map(|t| t.trim().chars().take(140).collect()).unwrap_or_default();
    if text.is_empty() {
        return Err(ApiError::bad("say something"));
    }
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let row = model::game(&ctx.db, &tx, &id).await?.ok_or_else(|| ApiError::not_found("game"))?;
    let side = row.side_of(player).ok_or_else(|| ApiError::not_found("game"))?;
    api.check(row.player(side))?;
    let username = row.player(side).username.clone();
    tx.execute(
        &ctx.db.sql(
            "INSERT INTO chess.chat (game_id, room, player_id, username, text) VALUES ($1, $2, $3, $4, $5)",
        ),
        &[&id, &room, &player, &username, &text],
    )
    .await?;
    let line = json!({"type": "chatLine", "room": room, "username": username, "text": text});
    notify(&tx, &ctx.db, &Note::Chat { game: id, line }).await?;
    tx.commit().await?;
    Ok(ok())
}

pub async fn chat_get(
    State(ctx): State<Ctx>,
    mp: MatchedPath,
    who: Who,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let (_, player) = api(&mp, &who)?;
    let c = ctx.db.conn().await?;
    let row = model::game(&ctx.db, &*c, &id).await?.ok_or_else(|| ApiError::not_found("game"))?;
    row.side_of(player).ok_or_else(|| ApiError::not_found("game"))?;
    let lines: Vec<serde_json::Value> = c
        .query(
            &ctx.db.sql(
                "SELECT username, text FROM chess.chat WHERE game_id = $1 AND room = 'player' ORDER BY id",
            ),
            &[&id],
        )
        .await?
        .iter()
        .map(|r| json!({"text": r.get::<_, String>(1), "user": r.get::<_, String>(0)}))
        .collect();
    Ok(Json(lines).into_response())
}
