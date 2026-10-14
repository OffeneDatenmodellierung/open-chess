//! Account, user and bot endpoints.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::json;

use crate::app::{Ctx, Reader, Who};
use crate::error::{ApiError, ApiResult};
use crate::http::{line, ndjson, ok};
use crate::model;

/// `GET /api/account`: Lichess's, plus what this server adds about you:
/// `roles` here (admin, manager), whether your passkey check is done
/// (`staff`: "checked", "pending" or absent) and how many passkeys you have.
pub async fn me(State(ctx): State<Ctx>, who: Who) -> ApiResult<Response> {
    let c = ctx.db.conn().await?;
    let a = model::ensure_account(&ctx.db, &*c, who.0.player).await?;
    let mut v = serde_json::to_value(model::user_json(&ctx.db, &*c, &a, ctx.cfg.public_url()).await?)
        .map_err(|e| ApiError::from(anyhow::Error::from(e)))?;
    let r = c
        .query_one(
            &ctx.db.sql(
                "SELECT role, roles, (SELECT count(*) FROM chess.passkeys WHERE player_id = $1)
                   FROM chess.players WHERE id = $1",
            ),
            &[&who.0.player],
        )
        .await?;
    let (admin, manager) = crate::auth::staff_roles(&r.get::<_, String>(0), &r.get::<_, Vec<String>>(1));
    let roles: Vec<&str> =
        [(admin, "admin"), (manager, "manager")].iter().filter(|x| x.0).map(|x| x.1).collect();
    v["roles"] = json!(roles);
    v["passkeys"] = json!(r.get::<_, i64>(2));
    if who.0.admin || who.0.manager {
        v["staff"] = json!("checked");
    } else if who.0.pending {
        v["staff"] = json!("pending");
    }
    v["accountUrl"] = json!(format!("{}/account", ctx.cfg.public_url()));
    Ok(Json(v).into_response())
}

pub async fn email(State(ctx): State<Ctx>, who: Who) -> ApiResult<Response> {
    let id = who.need(&["email:read"])?;
    let c = ctx.db.conn().await?;
    let email: Option<String> = c
        .query_opt(&ctx.db.sql("SELECT email FROM chess.players WHERE id = $1"), &[&id.player])
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

pub async fn user(
    State(ctx): State<Ctx>,
    _reader: Reader,
    Path(username): Path<String>,
) -> ApiResult<Response> {
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
pub async fn bots_online(
    State(ctx): State<Ctx>,
    _reader: Reader,
    Query(q): Query<Nb>,
) -> ApiResult<Response> {
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

#[derive(Debug, Deserialize)]
pub struct Autocomplete {
    term: Option<String>,
    object: Option<bool>,
    nb: Option<i64>,
}

/// Lichess's `GET /api/player/autocomplete`: players whose name starts
/// with (then contains) `term`, for a type-ahead list. With `object=true`,
/// `{"result": [{id, name, online}]}`; otherwise a JSON array of names. The
/// computer's accounts (`ai-level-N`) aren't offered: it's played with
/// `POST /api/challenge/ai`.
pub async fn autocomplete(
    State(ctx): State<Ctx>,
    _reader: Reader,
    Query(q): Query<Autocomplete>,
) -> ApiResult<Response> {
    let term = q.term.unwrap_or_default().trim().to_lowercase();
    if term.is_empty() {
        return Ok(
            Json(if q.object.unwrap_or(false) { json!({ "result": [] }) } else { json!([]) }).into_response()
        );
    }
    let like = format!("%{}%", term.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    let c = ctx.db.conn().await?;
    let rows = c
        .query(
            &ctx.db.sql(
                "SELECT username, seen_at > now() - interval '10 minutes' AS online
                   FROM chess.accounts
                  WHERE lower(username) LIKE $1 AND username !~ '^ai-level-[0-9]$'
                  ORDER BY (lower(username) LIKE $2) DESC, lower(username)
                  LIMIT $3",
            ),
            &[&like, &format!("{}%", &like[1..]), &q.nb.unwrap_or(12).clamp(1, 50)],
        )
        .await?;
    let names: Vec<(String, bool)> = rows
        .iter()
        .map(|r| (r.get("username"), r.get::<_, Option<bool>>("online").unwrap_or(false)))
        .collect();
    Ok(Json(if q.object.unwrap_or(false) {
        json!({ "result": names.iter().map(|(n, on)| json!({ "id": n.to_lowercase(), "name": n, "online": on })).collect::<Vec<_>>() })
    } else {
        json!(names.iter().map(|(n, _)| n).collect::<Vec<_>>())
    })
    .into_response())
}
