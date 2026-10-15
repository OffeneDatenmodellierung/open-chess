//! Sign in with Apple, from the iOS app and on the website
//! ([`crate::apple`] checks what Apple gave).
//!
//! **The app**: `POST /api/auth/apple` and `DELETE /api/auth/apple`.
//! - Sign in (no bearer token): the account linked to the Apple ID gets a
//!   new API token, as the OAuth flow would give. An Apple ID not linked
//!   yet makes a new account: the first answer is 409 `choose_username`
//!   with a suggestion, and the app sends the same request again with
//!   `username`. An Apple ID whose email another account has is refused
//!   (`email_in_use`): that person signs in as before and links Apple.
//! - Link (with the player's bearer token): the Apple ID is linked to that
//!   account. No new token.
//! - Unlink (`DELETE`, bearer): unless it's the account's only way in;
//!   Apple is told.
//!
//! **The website** (with `CHESS_APPLE_SERVICES_ID`): `GET /auth/apple` sends
//! the browser to Apple, which posts its answer to `/auth/apple/callback`;
//! the same rules decide, with a session instead of a token and a page to
//! choose a username (`/auth/apple/name`). From the account page,
//! `?link=1` links the signed-in account, and `/account/apple/remove`
//! unlinks.
//!
//! An account closed (`DELETE /api/account`) has its link revoked too.

use std::collections::HashMap;
use std::fmt::Write as _;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::app::{Ctx, Who};
use crate::apple::{Apple, AppleId};
use crate::auth::{bearer, cookie, hash_key, random_token};
use crate::error::{ApiError, ApiResult};
use crate::http::oauth::page;
use crate::http::{form, html_escape, web};

const TOKEN_DAYS: i64 = 365;
/// The cookie that ties Apple's answer to the browser that asked.
const STATE_COOKIE: &str = "oc_apple";

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
    Ok(format!("{base}{}", random_token(5).to_lowercase()))
}

/// Apple's one-time code: spent (traded for the link's refresh token) only
/// once everything else is in order, and at most once.
struct Code<'a> {
    apple: &'a Apple,
    pending: Option<&'a str>,
    /// The website's return URL, for a code from the website's flow.
    redirect: Option<&'a str>,
    refresh: Option<String>,
}

impl Code<'_> {
    async fn spend(&mut self, id: &AppleId) -> Result<Option<String>, ApiError> {
        if let Some(code) = self.pending.take() {
            self.refresh = self.apple.redeem(code, id, self.redirect).await.map_err(|e| {
                tracing::info!("Sign in with Apple refused: {e:#}");
                ApiError::forbidden(format!("{e:#}"))
            })?;
        }
        Ok(self.refresh.clone())
    }
}

/// What a checked Apple sign-in comes to.
#[derive(Debug)]
enum Outcome {
    /// Signed in to this account (`created`: just made).
    Player { player: i64, created: bool },
    /// Linked to the signed-in account.
    Linked { player: i64 },
    /// A new Apple ID: a username is wanted first.
    ChooseName { suggestion: String },
    /// Refused, with a code the app knows and a message for people.
    Refused(StatusCode, &'static str, &'static str),
}

/// The rules, the same for the app and the website. `link`: the signed-in
/// player to link to. `username`: chosen for a new account.
async fn decide(
    ctx: &Ctx,
    id: &AppleId,
    code: &mut Code<'_>,
    link: Option<i64>,
    username: Option<&str>,
    name_hint: &str,
) -> ApiResult<Outcome> {
    let c = ctx.db.conn().await?;
    let linked: Option<i64> = c
        .query_opt(&ctx.db.sql("SELECT player_id FROM chess.apple_ids WHERE sub = $1"), &[&id.sub])
        .await?
        .map(|r| r.get(0));

    if let Some(player) = link {
        if linked.is_some_and(|p| p != player) {
            return Ok(Outcome::Refused(
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
                return Ok(Outcome::Refused(
                    StatusCode::CONFLICT,
                    "already_linked",
                    "This account already has an Apple ID linked: unlink it first.",
                ));
            }
        }
        let refresh = code.spend(id).await?;
        c.execute(
            &ctx.db.sql(
                "INSERT INTO chess.apple_ids (sub, player_id, client_id, refresh_token, email, private_email)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (sub) DO NOTHING",
            ),
            &[&id.sub, &player, &id.client_id, &refresh, &id.email, &id.private_email],
        )
        .await?;
        remember(ctx, &*c, id, refresh.as_deref()).await?;
        tracing::info!(player, "Apple ID linked");
        return Ok(Outcome::Linked { player });
    }

    if let Some(player) = linked {
        let live = c
            .query_opt(
                &ctx.db.sql("SELECT 1 FROM chess.players WHERE id = $1 AND disabled_at IS NULL"),
                &[&player],
            )
            .await?;
        if live.is_none() {
            return Ok(Outcome::Refused(StatusCode::FORBIDDEN, "closed", "This account is closed."));
        }
        let refresh = code.spend(id).await?;
        remember(ctx, &*c, id, refresh.as_deref()).await?;
        return Ok(Outcome::Player { player, created: false });
    }

    if !ctx.cfg.signup {
        return Ok(Outcome::Refused(
            StatusCode::FORBIDDEN,
            "no_signup",
            "This server doesn't take new accounts, and no account here has that Apple ID.",
        ));
    }
    let email = id.email.clone().unwrap_or_default();
    if !email.is_empty() && web::taken(ctx, &*c, "", &email).await?.is_some() {
        return Ok(Outcome::Refused(
            StatusCode::CONFLICT,
            "email_in_use",
            "An account here already has your Apple ID's email. Sign in to it the usual way, then link your Apple ID under Account.",
        ));
    }
    let Some(name) = username.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(Outcome::ChooseName { suggestion: suggest(ctx, &*c, name_hint, &email).await? });
    };
    if let Some(why) = web::name_problem(name, "").or(web::taken(ctx, &*c, name, "").await?) {
        return Ok(Outcome::Refused(StatusCode::BAD_REQUEST, "bad_username", why));
    }
    let refresh = code.spend(id).await?;
    drop(c);
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    if let Some(why) = web::taken(ctx, &tx, name, &email).await? {
        return Ok(Outcome::Refused(StatusCode::BAD_REQUEST, "bad_username", why));
    }
    let player = web::make_account(ctx, &tx, name, &email, None).await?;
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
        return Ok(Outcome::Refused(
            StatusCode::CONFLICT,
            "just_linked",
            "That Apple ID was just linked: sign in again.",
        ));
    }
    tx.commit().await?;
    tracing::info!(player, "account made with Sign in with Apple");
    Ok(Outcome::Player { player, created: true })
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

async fn username(ctx: &Ctx, player: i64) -> ApiResult<String> {
    let c = ctx.db.conn().await?;
    Ok(crate::model::ensure_account(&ctx.db, &*c, player).await?.username)
}

// ─── the app ───

/// `POST /api/auth/apple`.
pub async fn sign_in(State(ctx): State<Ctx>, h: HeaderMap, Json(req): Json<SignIn>) -> ApiResult<Response> {
    let apple = apple(&ctx)?;
    let id = apple.verify(&req.identity_token, &req.nonce).await.map_err(|e| {
        tracing::info!("Sign in with Apple refused: {e:#}");
        ApiError::forbidden(format!("{e:#}"))
    })?;
    let link = match bearer(&h) {
        Some(t) => Some(ctx.auth.token(t).await?.ok_or_else(ApiError::unauthorized)?.player),
        None => None,
    };
    let mut code = Code { apple, pending: Some(&req.authorization_code), redirect: None, refresh: None };
    let hint = req.name.as_deref().unwrap_or_default();
    let (player, created) = match decide(&ctx, &id, &mut code, link, req.username.as_deref(), hint).await? {
        Outcome::Player { player, created } => (player, created),
        Outcome::Linked { player } => {
            let name = username(&ctx, player).await?;
            return Ok(Json(json!({"linked": true, "created": false, "username": name})).into_response());
        }
        Outcome::ChooseName { suggestion } => {
            return Ok((
                StatusCode::CONFLICT,
                Json(json!({"error": "choose_username", "suggestion": suggestion})),
            )
                .into_response());
        }
        Outcome::Refused(status, error, message) => return Ok(coded(status, error, message)),
    };
    web::grant_listed(&ctx, player).await?;
    let scopes = web::allowed(&ctx, player, &req.scopes).await?;
    let client = req.client_id.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("app");
    let (token, _) =
        web::issue(&ctx, player, &format!("Sign in with Apple: {client}"), &scopes, TOKEN_DAYS).await?;
    Ok(Json(json!({
        "access_token": token,
        "token_type": "Bearer",
        "expires_in": TOKEN_DAYS * 86_400,
        "username": username(&ctx, player).await?,
        "created": created,
        "linked": true,
    }))
    .into_response())
}

/// Unlink a player's Apple ID, unless it's their only way in (then the
/// message why). Apple is told.
async fn unlink_player(ctx: &Ctx, player: i64) -> ApiResult<Result<(), &'static str>> {
    let c = ctx.db.conn().await?;
    let other_way_in: bool = c
        .query_one(
            &ctx.db.sql(
                "SELECT p.password_hash IS NOT NULL OR p.esports_id IS NOT NULL
                        OR EXISTS (SELECT 1 FROM chess.passkeys k WHERE k.player_id = p.id)
                   FROM chess.players p WHERE p.id = $1",
            ),
            &[&player],
        )
        .await?
        .get(0);
    let Some(row) = c
        .query_opt(
            &ctx.db.sql("SELECT client_id, refresh_token FROM chess.apple_ids WHERE player_id = $1"),
            &[&player],
        )
        .await?
    else {
        return Ok(Ok(()));
    };
    if !other_way_in {
        return Ok(Err("Your Apple ID is how you sign in here. Add a password or a passkey first."));
    }
    c.execute(&ctx.db.sql("DELETE FROM chess.apple_ids WHERE player_id = $1"), &[&player]).await?;
    revoke(ctx, row.get(0), row.get(1)).await;
    tracing::info!(player, "Apple ID unlinked");
    Ok(Ok(()))
}

/// `DELETE /api/auth/apple`: unlink the caller's Apple ID.
pub async fn unlink(State(ctx): State<Ctx>, who: Who) -> ApiResult<Response> {
    Ok(match unlink_player(&ctx, who.0.player).await? {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(why) => coded(StatusCode::CONFLICT, "only_sign_in", why),
    })
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

// ─── the website ───

/// Whether the website signs in with Apple.
pub fn on_web(ctx: &Ctx) -> bool {
    ctx.apple.as_deref().is_some_and(|a| a.services_id().is_some())
}

fn callback_url(ctx: &Ctx) -> String {
    format!("{}/auth/apple/callback", ctx.cfg.public_url().trim_end_matches('/'))
}

/// The state cookie: sent back with Apple's cross-site POST, so
/// `SameSite=None` (which needs `Secure`; plain http keeps `Lax`).
fn state_cookie(ctx: &Ctx, value: &str, max_age: i64) -> ApiResult<HeaderValue> {
    let same_site = if web::secure(ctx).is_empty() { "Lax" } else { "None" };
    HeaderValue::from_str(&format!(
        "{STATE_COOKIE}={value}; Path={}; HttpOnly; SameSite={same_site}; Max-Age={max_age}{}",
        web::cookie_path(ctx),
        web::secure(ctx)
    ))
    .map_err(|e| ApiError::from(anyhow::anyhow!("{e}")))
}

fn sign_in_page(ctx: &Ctx, status: StatusCode, message: &str, ret: &str) -> Response {
    let base = ctx.cfg.base_path().unwrap_or_default();
    (
        status,
        page(
            "Sign in with Apple",
            &format!(
                "<p class=err>{}</p><p><a href=\"{base}/login?return={}\">Back to sign in</a></p>",
                html_escape(message),
                urlencoding::encode(ret)
            ),
        ),
    )
        .into_response()
}

fn name_form(ctx: &Ctx, ticket: &str, name: &str, error: Option<&str>) -> Response {
    let base = ctx.cfg.base_path().unwrap_or_default();
    let err = error.map(|e| format!("<p class=err>{}</p>", html_escape(e))).unwrap_or_default();
    (
        if error.is_some() { StatusCode::BAD_REQUEST } else { StatusCode::OK },
        page(
            "Choose a username",
            &format!(
                "<p>Your Apple ID is new here: choose the name other players will see.</p>{err}\
                 <form method=post action=\"{base}/auth/apple/name\">{t}\
                 <label>Username (letters, digits, _ and -)<input name=name value=\"{n}\" required minlength=2 maxlength=20 autocomplete=username></label>\
                 <button class=go>Make the account</button></form>",
                t = web::hidden("ticket", ticket),
                n = html_escape(name),
            ),
        ),
    )
        .into_response()
}

/// Signed in on the website: a session, and on to where they were going.
async fn signed_in(ctx: &Ctx, player: i64, to: &str, clear_state: bool) -> ApiResult<Response> {
    let session = web::start_session(ctx, player, false).await?;
    let mut r = Redirect::to(to).into_response();
    r.headers_mut().append(header::SET_COOKIE, session);
    if clear_state {
        r.headers_mut().append(header::SET_COOKIE, state_cookie(ctx, "", 0)?);
    }
    Ok(r)
}

/// `GET /auth/apple?return=…[&link=1]`: off to Apple.
pub async fn start(
    State(ctx): State<Ctx>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let ret = web::back_to(&ctx, q.get("return").map(String::as_str));
    let Some(apple) = ctx.apple.as_deref().filter(|_| on_web(&ctx)) else {
        return Ok(sign_in_page(
            &ctx,
            StatusCode::NOT_FOUND,
            "This server doesn't sign in with Apple on the web.",
            &ret,
        ));
    };
    let link = if q.get("link").is_some_and(|v| v == "1") {
        match web::signed_in(&ctx, &h).await? {
            Some((player, _)) => Some(player),
            None => {
                return Ok(Redirect::to(&format!("{}/login", ctx.cfg.base_path().unwrap_or_default()))
                    .into_response());
            }
        }
    } else {
        None
    };
    let (state, nonce) = (random_token(32), random_token(32));
    let c = ctx.db.conn().await?;
    c.execute(&ctx.db.sql("DELETE FROM chess.apple_flows WHERE expires_at < now()"), &[]).await?;
    c.execute(
        &ctx.db.sql(
            "INSERT INTO chess.apple_flows (state_hash, nonce, return_to, player_id, expires_at)
             VALUES ($1, $2, $3, $4, now() + interval '10 minutes')",
        ),
        &[&hash_key(&state), &nonce, &ret, &link],
    )
    .await?;
    let to = apple
        .authorize_url(&callback_url(&ctx), &state, &nonce)
        .ok_or_else(|| ApiError::from(anyhow::anyhow!("Apple's sign-in address")))?;
    let mut r = Redirect::to(&to).into_response();
    r.headers_mut().append(header::SET_COOKIE, state_cookie(&ctx, &state, 600)?);
    Ok(r)
}

/// `POST /auth/apple/callback`: Apple's answer (`code`, `id_token`,
/// `state`, the first time `user` with the name; or `error`).
pub async fn callback(State(ctx): State<Ctx>, h: HeaderMap, body: Bytes) -> ApiResult<Response> {
    let f = form(&body);
    let get = |k: &str| f.get(k).map_or("", String::as_str);
    let apple = apple(&ctx)?;
    let state = get("state");
    let flow = {
        let c = ctx.db.conn().await?;
        c.query_opt(
            &ctx.db.sql(
                "DELETE FROM chess.apple_flows WHERE state_hash = $1 AND expires_at > now()
                 RETURNING nonce, return_to, player_id",
            ),
            &[&hash_key(state)],
        )
        .await?
    };
    let home = web::back_to(&ctx, None);
    let Some(flow) = flow else {
        return Ok(sign_in_page(
            &ctx,
            StatusCode::BAD_REQUEST,
            "That sign-in has expired: try again.",
            &home,
        ));
    };
    let (nonce, ret, link): (String, String, Option<i64>) = (flow.get(0), flow.get(1), flow.get(2));
    if state.is_empty() || cookie(&h, STATE_COOKIE).as_deref() != Some(state) {
        return Ok(sign_in_page(
            &ctx,
            StatusCode::FORBIDDEN,
            "That sign-in didn't start in this browser: try again.",
            &ret,
        ));
    }
    if !get("error").is_empty() {
        return Ok(Redirect::to(&format!(
            "{}/login?return={}",
            ctx.cfg.base_path().unwrap_or_default(),
            urlencoding::encode(&ret)
        ))
        .into_response());
    }
    let id = match apple.verify(get("id_token"), &nonce).await {
        Ok(id) if Some(id.client_id.as_str()) == apple.services_id() => id,
        Ok(_) => return Ok(sign_in_page(&ctx, StatusCode::FORBIDDEN, "That sign-in was for the app.", &ret)),
        Err(e) => {
            tracing::info!("Sign in with Apple refused: {e:#}");
            return Ok(sign_in_page(&ctx, StatusCode::FORBIDDEN, &format!("{e:#}"), &ret));
        }
    };
    let user: Value = serde_json::from_str(get("user")).unwrap_or_default();
    let hint = format!(
        "{}{}",
        user["name"]["firstName"].as_str().unwrap_or_default(),
        user["name"]["lastName"].as_str().unwrap_or_default()
    );
    let redirect = callback_url(&ctx);
    let mut code = Code { apple, pending: Some(get("code")), redirect: Some(&redirect), refresh: None };
    match decide(&ctx, &id, &mut code, link, None, &hint).await? {
        Outcome::Player { player, .. } => signed_in(&ctx, player, &ret, true).await,
        Outcome::Linked { .. } => {
            let mut r = web::to_account(&ctx);
            r.headers_mut().append(header::SET_COOKIE, state_cookie(&ctx, "", 0)?);
            Ok(r)
        }
        Outcome::ChooseName { suggestion } => {
            // Apple's code lasts minutes; choosing a name may take longer.
            let refresh = code.spend(&id).await?;
            let ticket = random_token(40);
            let c = ctx.db.conn().await?;
            c.execute(&ctx.db.sql("DELETE FROM chess.apple_pending WHERE expires_at < now()"), &[]).await?;
            c.execute(
                &ctx.db.sql(
                    "INSERT INTO chess.apple_pending
                       (ticket_hash, sub, client_id, refresh_token, email, private_email, return_to, expires_at)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, now() + interval '30 minutes')",
                ),
                &[&hash_key(&ticket), &id.sub, &id.client_id, &refresh, &id.email, &id.private_email, &ret],
            )
            .await?;
            Ok(name_form(&ctx, &ticket, &suggestion, None))
        }
        Outcome::Refused(status, _, message) => Ok(sign_in_page(&ctx, status, message, &ret)),
    }
}

/// `POST /auth/apple/name`: the username for a new Apple ID (`ticket`, `name`).
pub async fn choose_name(State(ctx): State<Ctx>, body: Bytes) -> ApiResult<Response> {
    let f = form(&body);
    let ticket = f.get("ticket").map_or("", String::as_str);
    let name = f.get("name").map_or("", |s| s.trim());
    let apple = apple(&ctx)?;
    let row = {
        let c = ctx.db.conn().await?;
        c.query_opt(
            &ctx.db.sql(
                "SELECT sub, client_id, refresh_token, email, private_email, return_to
                   FROM chess.apple_pending WHERE ticket_hash = $1 AND expires_at > now()",
            ),
            &[&hash_key(ticket)],
        )
        .await?
    };
    let home = web::back_to(&ctx, None);
    let Some(row) = row else {
        return Ok(sign_in_page(
            &ctx,
            StatusCode::BAD_REQUEST,
            "That sign-in has expired: try again.",
            &home,
        ));
    };
    let id = AppleId { sub: row.get(0), client_id: row.get(1), email: row.get(3), private_email: row.get(4) };
    let ret: String = row.get(5);
    let mut code = Code { apple, pending: None, redirect: None, refresh: row.get(2) };
    match decide(&ctx, &id, &mut code, None, Some(name), "").await? {
        Outcome::Player { player, .. } => {
            let c = ctx.db.conn().await?;
            c.execute(
                &ctx.db.sql("DELETE FROM chess.apple_pending WHERE ticket_hash = $1"),
                &[&hash_key(ticket)],
            )
            .await?;
            signed_in(&ctx, player, &ret, false).await
        }
        Outcome::Refused(_, "bad_username", why) => Ok(name_form(&ctx, ticket, name, Some(why))),
        Outcome::ChooseName { suggestion } => {
            Ok(name_form(&ctx, ticket, &suggestion, Some("Choose a username.")))
        }
        Outcome::Linked { .. } => Ok(web::to_account(&ctx)),
        Outcome::Refused(status, _, message) => Ok(sign_in_page(&ctx, status, message, &ret)),
    }
}

/// `POST /account/apple/remove`: unlink from the account page.
pub async fn account_remove(State(ctx): State<Ctx>, h: HeaderMap, body: Bytes) -> ApiResult<Response> {
    let (player, session) =
        web::signed_in(&ctx, &h).await?.ok_or_else(|| ApiError::forbidden("sign in first"))?;
    web::form_csrf_ok(&form(&body), &session)?;
    web::may_change(&ctx, player, &session).await?;
    match unlink_player(&ctx, player).await? {
        Ok(()) => Ok(web::to_account(&ctx)),
        Err(why) => Err(ApiError::conflict(why)),
    }
}

/// The account page's Apple ID section.
pub async fn account_section(ctx: &Ctx, player: i64, csrf: &str, locked: bool) -> ApiResult<String> {
    let base = ctx.cfg.base_path().unwrap_or_default();
    let c = ctx.db.conn().await?;
    let row = c
        .query_opt(
            &ctx.db.sql(
                "SELECT a.email, a.private_email, to_char(a.created_at, 'YYYY-MM-DD'),
                        p.password_hash IS NOT NULL OR p.esports_id IS NOT NULL
                        OR EXISTS (SELECT 1 FROM chess.passkeys k WHERE k.player_id = p.id)
                   FROM chess.apple_ids a JOIN chess.players p ON p.id = a.player_id WHERE a.player_id = $1",
            ),
            &[&player],
        )
        .await?;
    if row.is_none() && !on_web(ctx) {
        return Ok(String::new());
    }
    let mut s = String::from("<h2>Apple ID</h2>");
    match row {
        Some(r) => {
            let email: Option<String> = r.get(0);
            let private: bool = r.get(1);
            let shown = match (email, private) {
                (Some(_), true) => " (with a private email address)".to_owned(),
                (Some(e), false) => format!(" ({})", html_escape(&e)),
                (None, _) => String::new(),
            };
            let _ = write!(
                s,
                "<p>Linked{shown} since {}: you can sign in with Apple.</p>",
                r.get::<_, String>(2)
            );
            let other_way_in: bool = r.get(3);
            if !other_way_in {
                s.push_str("<p class=muted>To unlink it, add a password or a passkey first.</p>");
            } else if !locked {
                let _ = write!(
                    s,
                    "<form method=post action=\"{base}/account/apple/remove\">{}<button>Unlink your Apple ID</button></form>",
                    web::hidden("csrf", csrf)
                );
            }
        }
        None => {
            let _ = write!(
                s,
                "<p class=muted>Sign in with your Apple ID too.</p><p><a class=btn href=\"{base}/auth/apple?link=1&amp;return={}\">Link your Apple ID</a></p>",
                urlencoding::encode(&format!("{base}/account"))
            );
        }
    }
    Ok(s)
}
