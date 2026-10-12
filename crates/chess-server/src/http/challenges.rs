//! Challenge endpoints.

use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::warn;

use crate::app::{Ctx, Who};
use crate::challenges::{self, Request};
use crate::db::Note;
use crate::error::{ApiError, ApiResult};
use crate::http::{form, line, ndjson, ok};
use crate::metrics::StreamGuard;
use crate::model;

const PLAY: &[&str] = &["challenge:write", "board:play", "bot:play"];

pub async fn create(
    State(ctx): State<Ctx>,
    who: Who,
    Path(username): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let id = who.need(PLAY)?.clone();
    let req = Request::parse(&form(&body))?;
    let (ch, ratings) = challenges::create(&ctx, &id, &username, &req).await?;
    let json = ch.json(ctx.cfg.public_url(), Some(id.player), ratings);
    if !req.keep_alive || ch.status != "created" {
        return Ok(Json(json).into_response());
    }
    // keepAliveStream: the challenge lives while this stream is open;
    // when it's answered, `{"done": "<status>"}` and the stream ends.
    let slot = ctx.slot(id.player, "challenge")?;
    let (tx, rx) = mpsc::channel::<Bytes>(16);
    let _ = tx.send(line(&json)).await;
    tokio::spawn(async move {
        keep_alive(ctx, ch.id.clone(), id.player, tx).await;
        drop(slot);
    });
    Ok(ndjson(rx))
}

async fn keep_alive(ctx: Ctx, id: String, player: i64, tx: mpsc::Sender<Bytes>) {
    let _g = StreamGuard::new(&ctx.metrics, "challenge");
    let mut notes = ctx.db.subscribe();
    let mut refresh = tokio::time::interval(Duration::from_secs(5));
    let mut keep = tokio::time::interval(Duration::from_secs(6));
    let done = |status: &str| line(&json!({"done": status}));
    loop {
        tokio::select! {
            n = notes.recv() => {
                let relevant = match &n {
                    Ok(Note::User { player: p, event }) => *p == player
                        && (event.pointer("/challenge/id").and_then(|v| v.as_str()) == Some(id.as_str())
                            || event.pointer("/game/id").and_then(|v| v.as_str()) == Some(id.as_str())),
                    Ok(Note::Resync) | Err(_) => true,
                    _ => false,
                };
                if relevant {
                    match challenges::status(&ctx, &id).await {
                        Ok(Some(s)) if s != "created" => {
                            let _ = tx.send(done(&s)).await;
                            return;
                        }
                        Ok(_) => {}
                        Err(e) => warn!("challenge stream: {e:#}"),
                    }
                }
            }
            _ = refresh.tick() => {
                if let Err(e) = challenges::keep_alive(&ctx, &id).await {
                    warn!("keeping challenge {id} alive: {e:#}");
                }
            }
            _ = keep.tick() => {
                if tx.send(Bytes::from_static(b"\n")).await.is_err() {
                    break;
                }
            }
        }
    }
    // The challenger went away: the challenge goes with them.
    let who = crate::auth::Identity {
        player,
        token_id: 0,
        scopes: Vec::new(),
        admin: false,
        manager: false,
        expires_ms: 0,
    };
    let _ = challenges::cancel(&ctx, &who, &id).await;
}

pub async fn list(State(ctx): State<Ctx>, who: Who) -> ApiResult<Response> {
    let id = who.need(&["challenge:read", "challenge:write", "board:play", "bot:play"])?;
    let (incoming, outgoing) = challenges::list(&ctx, id.player).await?;
    Ok(Json(chess_proto::ChallengeList { incoming, outgoing }).into_response())
}

pub async fn show(State(ctx): State<Ctx>, who: Who, Path(id): Path<String>) -> ApiResult<Response> {
    let me = who.need(&["challenge:read", "challenge:write", "board:play", "bot:play"])?;
    let c = ctx.db.conn().await?;
    let ch =
        model::challenge(&ctx.db, &*c, &id, false).await?.ok_or_else(|| ApiError::not_found("challenge"))?;
    if ch.challenger.player_id != me.player && ch.dest.player_id != me.player {
        return Err(ApiError::not_found("challenge"));
    }
    let ratings = model::challenge_ratings(&ctx.db, &*c, &ch).await?;
    Ok(Json(ch.json(ctx.cfg.public_url(), Some(me.player), ratings)).into_response())
}

pub async fn accept(State(ctx): State<Ctx>, who: Who, Path(id): Path<String>) -> ApiResult<Response> {
    let me = who.need(PLAY)?;
    challenges::accept(&ctx, me, &id).await?;
    Ok(ok())
}

pub async fn decline(
    State(ctx): State<Ctx>,
    who: Who,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let me = who.need(PLAY)?;
    let f = form(&body);
    challenges::decline(&ctx, me, &id, f.get("reason").map(String::as_str)).await?;
    Ok(ok())
}

pub async fn cancel(State(ctx): State<Ctx>, who: Who, Path(id): Path<String>) -> ApiResult<Response> {
    let me = who.need(PLAY)?;
    challenges::cancel(&ctx, me, &id).await?;
    Ok(ok())
}
