//! Account, user and bot endpoints.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::json;

use crate::app::{Ctx, Who};
use crate::error::{ApiError, ApiResult};
use crate::http::{line, ndjson, ok};
use crate::model;

pub async fn me(State(ctx): State<Ctx>, who: Who) -> ApiResult<Response> {
    let c = ctx.db.conn().await?;
    let a = model::ensure_account(&ctx.db, &*c, who.0.player).await?;
    Ok(Json(model::user_json(&ctx.db, &*c, &a, ctx.cfg.public_url()).await?).into_response())
}

pub async fn email(State(ctx): State<Ctx>, who: Who) -> ApiResult<Response> {
    let id = who.need(&["email:read"])?;
    let c = ctx.db.conn().await?;
    let email: Option<String> = c
        .query_opt(&ctx.db.sql("SELECT email FROM site.players WHERE id = $1"), &[&id.player])
        .await?
        .and_then(|r| r.get(0));
    Ok(Json(json!({"email": email.unwrap_or_default()})).into_response())
}

#[derive(Deserialize)]
pub struct Nb {
    nb: Option<usize>,
}

pub async fn playing(State(ctx): State<Ctx>, who: Who, Query(q): Query<Nb>) -> ApiResult<Response> {
    let c = ctx.db.conn().await?;
    let games = model::ongoing_games(&ctx.db, &*c, who.0.player).await?;
    let now: Vec<chess_proto::GameEventInfo> = games
        .iter()
        .filter_map(|g| g.event_info(who.0.player))
        .take(q.nb.unwrap_or(9).clamp(1, 50))
        .collect();
    let my_turn = now.iter().filter(|g| g.is_my_turn).count();
    Ok(Json(chess_proto::NowPlaying { now_playing: now, nb_my_turn: my_turn as i64 }).into_response())
}

pub async fn user(State(ctx): State<Ctx>, Path(username): Path<String>) -> ApiResult<Response> {
    let c = ctx.db.conn().await?;
    let a =
        model::account_by_name(&ctx.db, &*c, &username).await?.ok_or_else(|| ApiError::not_found("user"))?;
    Ok(Json(model::user_json(&ctx.db, &*c, &a, ctx.cfg.public_url()).await?).into_response())
}

/// Make this account a bot. Lichess's rules: irreversible, and only for an
/// account that hasn't played a game.
pub async fn upgrade(State(ctx): State<Ctx>, who: Who) -> ApiResult<Response> {
    let id = who.need(&["bot:play"])?;
    let c = ctx.db.conn().await?;
    let a = model::ensure_account(&ctx.db, &*c, id.player).await?;
    if a.bot {
        return Err(ApiError::bad("This account is already a bot account"));
    }
    let played: bool = c
        .query_one(
            &ctx.db.sql("SELECT EXISTS (SELECT 1 FROM chess.games WHERE white_id = $1 OR black_id = $1)"),
            &[&id.player],
        )
        .await?
        .get(0);
    if played {
        return Err(ApiError::bad("This account has played games and can't be a bot account"));
    }
    c.execute(&ctx.db.sql("UPDATE chess.accounts SET kind = 'bot' WHERE player_id = $1"), &[&id.player])
        .await?;
    Ok(ok())
}

/// Bots seen in the last ten minutes, as NDJSON users.
pub async fn bots_online(State(ctx): State<Ctx>, Query(q): Query<Nb>) -> ApiResult<Response> {
    let c = ctx.db.conn().await?;
    let rows = c
        .query(
            &ctx.db.sql(
                "SELECT player_id FROM chess.accounts
                  WHERE kind = 'bot' AND seen_at > now() - interval '10 minutes'
                  ORDER BY seen_at DESC LIMIT $1",
            ),
            &[&(q.nb.unwrap_or(50).clamp(1, 300) as i64)],
        )
        .await?;
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(rows.len().max(1));
    for r in rows {
        if let Some(a) = model::account(&ctx.db, &*c, r.get(0)).await? {
            let u = model::user_json(&ctx.db, &*c, &a, ctx.cfg.public_url()).await?;
            let _ = tx.try_send(line(&u));
        }
    }
    drop(tx);
    Ok(ndjson(rx))
}
