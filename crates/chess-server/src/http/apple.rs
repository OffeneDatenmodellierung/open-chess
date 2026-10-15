//! `POST /api/auth/apple` and `DELETE /api/auth/apple`: Sign in with Apple
//! from the iOS app ([`crate::apple`] checks what Apple gave it).
//!
//! - **Sign in** (no bearer token): the account linked to the Apple ID gets
//!   a new API token, as the OAuth flow would give. An Apple ID not linked
//!   yet makes a new account: the first answer is 409 `choose_username`
//!   with a suggestion, and the app sends the same request again with
//!   `username`. An Apple ID whose email another account has is refused
//!   (`email_in_use`): that person signs in as before and links Apple.
//! - **Link** (with the player's bearer token): the Apple ID is linked to
//!   that account. No new token.
//! - **Unlink** (`DELETE`, bearer): unless it's the account's only way in;
//!   Apple is told.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::app::{Ctx, Who};
use crate::apple::{Apple, AppleId};
use crate::auth::bearer;
use crate::error::{ApiError, ApiResult};
use crate::http::web;

const TOKEN_DAYS: i64 = 365;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignIn {
    identity_token: String,
    authorization_code: String,
    nonce: String,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(default)]
    username: Option<String>,
    /// The person's name, which Apple gives the app the first time only.
    #[serde(default)]
    name: Option<String>,
}

fn coded(status: StatusCode, error: &str, message: &str) -> Response {
    (status, Json(json!({"error": error, "message": message}))).into_response()
}

fn apple(ctx: &Ctx) -> ApiResult<&Apple> {
    ctx.apple.as_deref().ok_or_else(|| ApiError::forbidden("Sign in with Apple isn't set up on this server"))
}

/// A free username from what we know of the person.
async fn suggest(
    ctx: &Ctx,
    c: &impl tokio_postgres::GenericClient,
    name: &str,
    email: &str,
) -> ApiResult<String> {
    let from = |s: &str| -> String {
        s.chars().filter(char::is_ascii_alphanumeric).take(14).collect::<String>().to_lowercase()
    };
    let mut base = from(name);
    if base.len() < 3 && !email.contains("privaterelay") {
        base = from(email.split('@').next().unwrap_or_default());
    }
    if base.len() < 3 {
        base = "player".into();
    }
    if web::name_problem(&base, "").is_none() && web::taken(ctx, c, &base, "").await?.is_none() {
        return Ok(base);
    }
    for _ in 0..8 {
        let n: u32 = rand::random_range(10..10_000);
        let s = format!("{base}{n}");
        if web::taken(ctx, c, &s, "").await?.is_none() {
            return Ok(s);
        }
    }
    Ok(format!("{base}{}", crate::auth::random_token(5).to_lowercase()))
}

/// Keep the link's refresh token (newest wins) and when it was used.
async fn remember(
    ctx: &Ctx,
    c: &impl tokio_postgres::GenericClient,
    id: &AppleId,
    refresh: Option<&str>,
) -> ApiResult<()> {
    c.execute(
        &ctx.db.sql(
            "UPDATE chess.apple_ids SET last_used_at = now(), client_id = $2,
                    refresh_token = coalesce($3, refresh_token)
              WHERE sub = $1",
        ),
        &[&id.sub, &id.client_id, &refresh],
    )
    .await?;
    Ok(())
}

/// `POST /api/auth/apple`.
pub async fn sign_in(State(ctx): State<Ctx>, h: HeaderMap, Json(req): Json<SignIn>) -> ApiResult<Response> {
    let apple = apple(&ctx)?;
    let id = apple.verify(&req.identity_token, &req.nonce).await.map_err(|e| {
        tracing::info!("Sign in with Apple refused: {e:#}");
        ApiError::forbidden(format!("{e:#}"))
    })?;
    let linking = match bearer(&h) {
        Some(t) => Some(ctx.auth.token(t).await?.ok_or_else(ApiError::unauthorized)?.player),
        None => None,
    };
    let c = ctx.db.conn().await?;
    let linked: Option<i64> = c
        .query_opt(&ctx.db.sql("SELECT player_id FROM chess.apple_ids WHERE sub = $1"), &[&id.sub])
        .await?
        .map(|r| r.get(0));

    if let Some(player) = linking {
        if linked.is_some_and(|p| p != player) {
            return Ok(coded(
                StatusCode::CONFLICT,
                "apple_linked_elsewhere",
                "That Apple ID is linked to another account here.",
            ));
        }
        if linked.is_none() {
            let other = c
                .query_opt(&ctx.db.sql("SELECT 1 FROM chess.apple_ids WHERE player_id = $1"), &[&player])
                .await?;
            if other.is_some() {
                return Ok(coded(
                    StatusCode::CONFLICT,
                    "already_linked",
                    "This account already has an Apple ID linked: unlink it first.",
                ));
            }
        }
        let refresh = apple
            .redeem(&req.authorization_code, &id)
            .await
            .map_err(|e| ApiError::forbidden(format!("{e:#}")))?;
        c.execute(
            &ctx.db.sql(
                "INSERT INTO chess.apple_ids (sub, player_id, client_id, refresh_token, email, private_email)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (sub) DO NOTHING",
            ),
            &[&id.sub, &player, &id.client_id, &refresh, &id.email, &id.private_email],
        )
        .await?;
        remember(&ctx, &*c, &id, refresh.as_deref()).await?;
        let a = crate::model::ensure_account(&ctx.db, &*c, player).await?;
        tracing::info!(player, "Apple ID linked");
        return Ok(Json(json!({"linked": true, "created": false, "username": a.username})).into_response());
    }

    let (player, created) = if let Some(player) = linked {
        let live = c
            .query_opt(
                &ctx.db.sql("SELECT 1 FROM chess.players WHERE id = $1 AND disabled_at IS NULL"),
                &[&player],
            )
            .await?;
        if live.is_none() {
            return Err(ApiError::forbidden("this account is closed"));
        }
        let refresh = apple
            .redeem(&req.authorization_code, &id)
            .await
            .map_err(|e| ApiError::forbidden(format!("{e:#}")))?;
        remember(&ctx, &*c, &id, refresh.as_deref()).await?;
        (player, false)
    } else {
        if !ctx.cfg.signup {
            return Err(ApiError::forbidden("this server doesn't take new accounts"));
        }
        let email = id.email.clone().unwrap_or_default();
        if !email.is_empty() && web::taken(&ctx, &*c, "", &email).await?.is_some() {
            return Ok(coded(
                StatusCode::CONFLICT,
                "email_in_use",
                "An account here already has your Apple ID's email. Sign in to it the usual way, then link your Apple ID under Account.",
            ));
        }
        let Some(name) = req.username.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            let suggestion = suggest(&ctx, &*c, req.name.as_deref().unwrap_or_default(), &email).await?;
            return Ok((
                StatusCode::CONFLICT,
                Json(json!({"error": "choose_username", "suggestion": suggestion})),
            )
                .into_response());
        };
        if let Some(why) = web::name_problem(name, "") {
            return Ok(coded(StatusCode::BAD_REQUEST, "bad_username", why));
        }
        if let Some(why) = web::taken(&ctx, &*c, name, "").await? {
            return Ok(coded(StatusCode::BAD_REQUEST, "bad_username", why));
        }
        // Only now, with everything else in order, is the one-time code spent.
        let refresh = apple
            .redeem(&req.authorization_code, &id)
            .await
            .map_err(|e| ApiError::forbidden(format!("{e:#}")))?;
        drop(c);
        let mut c = ctx.db.conn().await?;
        let tx = c.transaction().await?;
        if let Some(why) = web::taken(&ctx, &tx, name, &email).await? {
            return Ok(coded(StatusCode::BAD_REQUEST, "bad_username", why));
        }
        let player = web::make_account(&ctx, &tx, name, &email, None).await?;
        let n = tx
            .execute(
                &ctx.db.sql(
                    "INSERT INTO chess.apple_ids (sub, player_id, client_id, refresh_token, email, private_email)
                     VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (sub) DO NOTHING",
                ),
                &[&id.sub, &player, &id.client_id, &refresh, &id.email, &id.private_email],
            )
            .await?;
        if n == 0 {
            // Another request made the account first.
            return Err(ApiError::conflict("that Apple ID was just linked: sign in again"));
        }
        tx.commit().await?;
        tracing::info!(player, "account made with Sign in with Apple");
        (player, true)
    };

    web::grant_listed(&ctx, player).await?;
    let scopes = web::allowed(&ctx, player, &req.scopes).await?;
    let client = req.client_id.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("app");
    let (token, _) =
        web::issue(&ctx, player, &format!("Sign in with Apple: {client}"), &scopes, TOKEN_DAYS).await?;
    let c = ctx.db.conn().await?;
    let a = crate::model::ensure_account(&ctx.db, &*c, player).await?;
    Ok(Json(json!({
        "access_token": token,
        "token_type": "Bearer",
        "expires_in": TOKEN_DAYS * 86_400,
        "username": a.username,
        "created": created,
        "linked": true,
    }))
    .into_response())
}

/// `DELETE /api/auth/apple`: unlink the caller's Apple ID.
pub async fn unlink(State(ctx): State<Ctx>, who: Who) -> ApiResult<Response> {
    let player = who.0.player;
    let c = ctx.db.conn().await?;
    let r = c
        .query_one(
            &ctx.db.sql(
                "SELECT p.password_hash IS NOT NULL OR p.esports_id IS NOT NULL
                        OR EXISTS (SELECT 1 FROM chess.passkeys k WHERE k.player_id = p.id)
                   FROM chess.players p WHERE p.id = $1",
            ),
            &[&player],
        )
        .await?;
    let other_way_in: bool = r.get(0);
    let Some(row) = c
        .query_opt(
            &ctx.db.sql("SELECT client_id, refresh_token FROM chess.apple_ids WHERE player_id = $1"),
            &[&player],
        )
        .await?
    else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    if !other_way_in {
        return Ok(coded(
            StatusCode::CONFLICT,
            "only_sign_in",
            "Your Apple ID is how you sign in here. Add a password or a passkey first.",
        ));
    }
    c.execute(&ctx.db.sql("DELETE FROM chess.apple_ids WHERE player_id = $1"), &[&player]).await?;
    revoke(&ctx, row.get(0), row.get(1)).await;
    tracing::info!(player, "Apple ID unlinked");
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Tell Apple a link has ended; a failure is logged, not fatal.
pub async fn revoke(ctx: &Ctx, client_id: String, refresh: Option<String>) {
    let (Some(apple), Some(refresh)) = (ctx.apple.as_deref(), refresh) else {
        return;
    };
    if let Err(e) = apple.revoke(&client_id, &refresh).await {
        tracing::warn!("revoking an Apple ID link: {e:#}");
    }
}
