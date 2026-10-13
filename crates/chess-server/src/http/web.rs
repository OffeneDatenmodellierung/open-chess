//! Signing in to this server: its own accounts (a username and a
//! password), and, with `ESPORTS_LOGIN`, "Sign in with the host site"
//! (esports-lab). Both end in this server's session cookie, which the OAuth
//! consent page and the Account page (API tokens) use.
//!
//! **The host site.** It shares a secret with this server
//! (`ESPORTS_SECRET`) and uses it two ways:
//!
//! * Signing in: `GET /auth/esports` sends the browser to the site's
//!   `/chess/sso?return=…&nonce=…`; the site sends it back to
//!   `/auth/esports/back?a=…` with a signed statement of who is signed in
//!   there ([`sign`]: `base64url(json).base64url(HMAC-SHA256)`, the JSON
//!   `{sub, name, email, role, roles, nonce, exp}`).
//! * Its server asks for a token for one of its players:
//!   `POST /api/esports/token` with `Authorization: Bearer <secret>` and
//!   the same fields as a form (plus `scopes`, `days`, `label`).
//!
//! Either way the site's player is linked to a player here by `esports_id`
//! (made on first use, with a chess account named after them), so the
//! same person is the same chess player whichever way they come in.

use std::collections::HashMap;
use std::fmt::Write as _;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use bytes::Bytes;
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::app::Ctx;
use crate::auth::{SCOPES, SESSION_COOKIE, TOKEN_PREFIX, bearer, cookie, hash_key, random_token};
use crate::error::{ApiError, ApiResult};
use crate::http::oauth::page;
use crate::http::{form, html_escape};
use crate::model;

const SESSION_DAYS: i64 = 30;
const SSO_COOKIE: &str = "oc_sso";

// ─── HMAC and the site's signed statements ───

/// HMAC-SHA256 (RFC 2104).
pub fn hmac(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let pad = |b: u8| k.map(|x| x ^ b);
    let inner = Sha256::new().chain_update(pad(0x36)).chain_update(msg).finalize();
    Sha256::new().chain_update(pad(0x5c)).chain_update(inner).finalize().into()
}

fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A signed statement: `base64url(json).base64url(hmac)`.
pub fn sign(secret: &str, v: &Value) -> String {
    let body = B64.encode(v.to_string());
    let mac = B64.encode(hmac(secret.as_bytes(), body.as_bytes()));
    format!("{body}.{mac}")
}

/// The statement, if its signature is right and it hasn't expired.
pub fn verify(secret: &str, s: &str, now_secs: i64) -> Option<Value> {
    let (body, mac) = s.split_once('.')?;
    let want = hmac(secret.as_bytes(), body.as_bytes());
    if !same(&B64.decode(mac).ok()?, &want) {
        return None;
    }
    let v: Value = serde_json::from_slice(&B64.decode(body).ok()?).ok()?;
    (v.get("exp")?.as_i64()? > now_secs).then_some(v)
}

/// A player of the host site, as it describes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SitePlayer {
    pub sub: String,
    pub name: String,
    pub email: Option<String>,
    pub role: String,
    pub roles: Vec<String>,
}

impl SitePlayer {
    fn from_fields(get: impl Fn(&str) -> Option<String>) -> Option<SitePlayer> {
        let sub = get("sub").filter(|s| !s.is_empty() && s.len() <= 64)?;
        Some(SitePlayer {
            name: get("name").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| format!("player{sub}")),
            email: get("email").filter(|e| !e.is_empty()),
            role: get("role").unwrap_or_else(|| "player".into()),
            roles: get("roles")
                .map(|r| r.split([',', ' ']).filter(|x| !x.is_empty()).map(str::to_owned).collect())
                .unwrap_or_default(),
            sub,
        })
    }

    fn from_json(v: &Value) -> Option<SitePlayer> {
        SitePlayer::from_fields(|k| match v.get(k)? {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            Value::Array(a) => Some(a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(",")),
            _ => None,
        })
    }
}

/// The player here linked to the site's, made on first use (and kept up
/// to date: email and roles). Returns the player's id and chess username.
pub async fn link(ctx: &Ctx, p: &SitePlayer) -> ApiResult<(i64, String)> {
    let c = ctx.db.conn().await?;
    let id: i64 = c
        .query_one(
            &ctx.db.sql(
                "INSERT INTO chess.players (name, email, role, roles, esports_id, key_hash)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (esports_id) DO UPDATE
                    SET email = EXCLUDED.email, role = EXCLUDED.role, roles = EXCLUDED.roles
                 RETURNING id",
            ),
            &[&p.name, &p.email, &p.role, &p.roles, &p.sub, &hash_key(&random_token(28))],
        )
        .await?
        .get(0);
    let a = model::ensure_account(&ctx.db, &*c, id).await?;
    Ok((id, a.username))
}

// ─── sessions ───

fn secure(ctx: &Ctx) -> &'static str {
    if ctx.cfg.public_url().starts_with("https://") { "; Secure" } else { "" }
}

fn cookie_path(ctx: &Ctx) -> String {
    let base = ctx.cfg.base_path().unwrap_or_default();
    if base.is_empty() { "/".into() } else { base }
}

/// Start a session: the cookie to set.
async fn start_session(ctx: &Ctx, player: i64) -> ApiResult<HeaderValue> {
    let token = random_token(40);
    let c = ctx.db.conn().await?;
    c.execute(
        &ctx.db.sql(
            "INSERT INTO chess.sessions (token_hash, player_id, expires_at) VALUES ($1, $2, now() + make_interval(days => $3))",
        ),
        &[&hash_key(&token), &player, &i32::try_from(SESSION_DAYS).unwrap_or(30)],
    )
    .await?;
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={token}; Path={}; HttpOnly; SameSite=Lax; Max-Age={}{}",
        cookie_path(ctx),
        SESSION_DAYS * 86_400,
        secure(ctx)
    ))
    .map_err(|e| ApiError::from(anyhow::anyhow!("{e}")))
}

/// Where to go after signing in: a path on this server, or the Account page.
fn back_to(ctx: &Ctx, want: Option<&str>) -> String {
    let base = ctx.cfg.base_path().unwrap_or_default();
    match want {
        Some(p) if p.starts_with('/') && !p.starts_with("//") && !p.contains('\\') => p.to_owned(),
        _ => format!("{base}/account"),
    }
}

fn redirect_with_cookies(to: &str, cookies: Vec<HeaderValue>) -> Response {
    let mut r = Redirect::to(to).into_response();
    for c in cookies {
        r.headers_mut().append(header::SET_COOKIE, c);
    }
    r
}

/// A form tied to the session that saw it.
fn csrf(session: &str, what: &str) -> String {
    hash_key(&format!("{session}|{what}"))
}

// ─── pages ───

fn hidden(k: &str, v: &str) -> String {
    format!("<input type=hidden name=\"{k}\" value=\"{}\">", html_escape(v))
}

fn sign_in_form(ctx: &Ctx, ret: &str, error: Option<&str>) -> String {
    let base = ctx.cfg.base_path().unwrap_or_default();
    let mut body = String::new();
    if let Some(e) = error {
        let _ = write!(body, "<p class=err>{}</p>", html_escape(e));
    }
    if ctx.cfg.esports().is_some() {
        let _ = write!(
            body,
            "<p><a class=btn href=\"{base}/auth/esports?return={r}\">Sign in with {site}</a></p><p class=muted>or with an account here:</p>",
            r = urlencoding::encode(ret),
            site = html_escape(&ctx.cfg.site_name)
        );
    }
    let _ = write!(
        body,
        "<form method=post action=\"{base}/login\">{h}\
         <label>Username or email<input name=name autocomplete=username required></label>\
         <label>Password<input name=password type=password autocomplete=current-password required></label>\
         <button class=go>Sign in</button></form>",
        h = hidden("return", ret)
    );
    if ctx.cfg.signup {
        let _ = write!(
            body,
            "<p class=muted>No account? <a href=\"{base}/signup?return={}\">Make one</a>.</p>",
            urlencoding::encode(ret)
        );
    }
    body
}

/// `GET /login`.
pub async fn login_page(State(ctx): State<Ctx>, Query(q): Query<HashMap<String, String>>) -> Response {
    let ret = back_to(&ctx, q.get("return").map(String::as_str));
    page("Sign in", &sign_in_form(&ctx, &ret, q.get("error").map(String::as_str)))
}

/// `POST /login`: a username (or email) and password.
pub async fn login(State(ctx): State<Ctx>, body: Bytes) -> ApiResult<Response> {
    let f = form(&body);
    let ret = back_to(&ctx, f.get("return").map(String::as_str));
    let name = f.get("name").map_or("", |s| s.trim());
    let password = f.get("password").map_or("", String::as_str);
    let c = ctx.db.conn().await?;
    let row = c
        .query_opt(
            &ctx.db.sql(
                "SELECT p.id, p.password_hash FROM chess.players p JOIN chess.accounts a ON a.player_id = p.id
                  WHERE (lower(a.username) = lower($1) OR lower(p.email) = lower($1))
                    AND p.disabled_at IS NULL AND p.password_hash IS NOT NULL
                  LIMIT 1",
            ),
            &[&name],
        )
        .await?;
    drop(c);
    let ok = row.as_ref().is_some_and(|r| {
        let stored: String = r.get(1);
        PasswordHash::new(&stored)
            .is_ok_and(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
    });
    let Some(row) = row.filter(|_| ok) else {
        // The same pace either way, and a little slower.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        return Ok((
            StatusCode::UNAUTHORIZED,
            page(
                "Sign in",
                &sign_in_form(&ctx, &ret, Some("That name and password don't match an account here.")),
            ),
        )
            .into_response());
    };
    let cookie = start_session(&ctx, row.get(0)).await?;
    Ok(redirect_with_cookies(&ret, vec![cookie]))
}

fn signup_form(ctx: &Ctx, ret: &str, error: Option<&str>, name: &str, email: &str) -> String {
    let base = ctx.cfg.base_path().unwrap_or_default();
    let mut body = String::new();
    if let Some(e) = error {
        let _ = write!(body, "<p class=err>{}</p>", html_escape(e));
    }
    let _ = write!(
        body,
        "<form method=post action=\"{base}/signup\">{h}\
         <label>Username (letters, digits, _ and -)<input name=name value=\"{n}\" autocomplete=username required minlength=2 maxlength=20></label>\
         <label>Email (optional, to sign in with)<input name=email type=email value=\"{e}\" autocomplete=email></label>\
         <label>Password (10 or more characters)<input name=password type=password autocomplete=new-password required minlength=10></label>\
         <button class=go>Make the account</button></form>\
         <p class=muted>Already have one? <a href=\"{base}/login?return={r}\">Sign in</a>.</p>",
        h = hidden("return", ret),
        n = html_escape(name),
        e = html_escape(email),
        r = urlencoding::encode(ret),
    );
    body
}

/// `GET /signup`.
pub async fn signup_page(State(ctx): State<Ctx>, Query(q): Query<HashMap<String, String>>) -> Response {
    if !ctx.cfg.signup {
        return (
            StatusCode::FORBIDDEN,
            page("No sign-ups here", "<p>This server doesn't take new accounts.</p>"),
        )
            .into_response();
    }
    let ret = back_to(&ctx, q.get("return").map(String::as_str));
    page("Make an account", &signup_form(&ctx, &ret, None, "", ""))
}

/// `POST /signup`: a username, a password and (optionally) an email.
pub async fn signup(State(ctx): State<Ctx>, body: Bytes) -> ApiResult<Response> {
    if !ctx.cfg.signup {
        return Err(ApiError::forbidden("this server doesn't take new accounts"));
    }
    let f = form(&body);
    let ret = back_to(&ctx, f.get("return").map(String::as_str));
    let name = f.get("name").map_or("", |s| s.trim()).to_owned();
    let email = f.get("email").map_or("", |s| s.trim()).to_owned();
    let password = f.get("password").cloned().unwrap_or_default();
    let refuse = |why: &str| {
        Ok((
            StatusCode::BAD_REQUEST,
            page("Make an account", &signup_form(&ctx, &ret, Some(why), &name, &email)),
        )
            .into_response())
    };
    let valid = name.len() >= 2
        && name.len() <= 20
        && name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !valid {
        return refuse("A username is 2 to 20 letters, digits, _ and -, starting with a letter or digit.");
    }
    if name.to_lowercase().starts_with("ai-level-") {
        return refuse("That name is the computer's.");
    }
    if password.chars().count() < 10 {
        return refuse("The password needs 10 or more characters.");
    }
    if !email.is_empty() && !email.contains('@') {
        return refuse("That email doesn't look right.");
    }
    let hash = Argon2::default()
        .hash_password(password.as_bytes())
        .map_err(|e| ApiError::from(anyhow::anyhow!("hashing the password: {e}")))?
        .to_string();
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    if model::account_by_name(&ctx.db, &tx, &name).await?.is_some() {
        return refuse("That username is taken.");
    }
    if !email.is_empty()
        && tx
            .query_opt(&ctx.db.sql("SELECT 1 FROM chess.players WHERE lower(email) = lower($1)"), &[&email])
            .await?
            .is_some()
    {
        return refuse("An account here already has that email.");
    }
    let id: i64 = tx
        .query_one(
            &ctx.db.sql(
                "INSERT INTO chess.players (name, email, password_hash) VALUES ($1, $2, $3) RETURNING id",
            ),
            &[&name, &(!email.is_empty()).then_some(&email), &hash],
        )
        .await?
        .get(0);
    tx.execute(
        &ctx.db.sql("INSERT INTO chess.accounts (player_id, username) VALUES ($1, $2)"),
        &[&id, &name],
    )
    .await?;
    tx.commit().await?;
    let cookie = start_session(&ctx, id).await?;
    Ok(redirect_with_cookies(&ret, vec![cookie]))
}

/// `POST /logout`.
pub async fn logout(State(ctx): State<Ctx>, h: HeaderMap) -> ApiResult<Response> {
    if let Some(t) = cookie(&h, SESSION_COOKIE) {
        let c = ctx.db.conn().await?;
        c.execute(
            &ctx.db.sql("UPDATE chess.sessions SET revoked_at = now() WHERE token_hash = $1"),
            &[&hash_key(&t)],
        )
        .await?;
    }
    let base = ctx.cfg.base_path().unwrap_or_default();
    let gone = HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}=; Path={}; Max-Age=0; HttpOnly; SameSite=Lax",
        cookie_path(&ctx)
    ))
    .map_err(|e| ApiError::from(anyhow::anyhow!("{e}")))?;
    Ok(redirect_with_cookies(&format!("{base}/login"), vec![gone]))
}

/// The scopes a player may give a token (staff-only ones aside).
pub async fn allowed(ctx: &Ctx, player: i64, wanted: &[String]) -> ApiResult<Vec<String>> {
    let c = ctx.db.conn().await?;
    let r =
        c.query_one(&ctx.db.sql("SELECT role, roles FROM chess.players WHERE id = $1"), &[&player]).await?;
    let role: String = r.get(0);
    let roles: Vec<String> = r.get(1);
    let staff = role == "admin" || roles.iter().any(|x| x == "manager");
    Ok(wanted
        .iter()
        .filter(|s| SCOPES.iter().any(|(k, _)| k == s))
        .filter(|s| staff || !matches!(s.as_str(), "chess:fairplay" | "bot:play"))
        .cloned()
        .collect())
}

/// Make a token; returns it (shown once) and when it expires.
pub async fn issue(
    ctx: &Ctx,
    player: i64,
    label: &str,
    scopes: &[String],
    days: i64,
) -> ApiResult<(String, chrono::DateTime<Utc>)> {
    issue_for(ctx, player, label, scopes, Duration::days(days.clamp(1, 365))).await
}

/// Make a token lasting `life`.
pub async fn issue_for(
    ctx: &Ctx,
    player: i64,
    label: &str,
    scopes: &[String],
    life: Duration,
) -> ApiResult<(String, chrono::DateTime<Utc>)> {
    let token = format!("{TOKEN_PREFIX}{}", random_token(36));
    let prefix: String = token.chars().take(TOKEN_PREFIX.len() + 6).collect();
    let expires = Utc::now() + life;
    let label: String = label.trim().chars().take(80).collect();
    let c = ctx.db.conn().await?;
    c.execute(
        &ctx.db.sql(
            "INSERT INTO chess.api_tokens (player_id, name, token_hash, prefix, scopes, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
        ),
        &[
            &player,
            &if label.is_empty() { "token".to_owned() } else { label },
            &hash_key(&token),
            &prefix,
            &scopes,
            &expires,
        ],
    )
    .await?;
    Ok((token, expires))
}

async fn account_page(ctx: &Ctx, player: i64, session: &str, fresh: Option<&str>) -> ApiResult<Response> {
    let base = ctx.cfg.base_path().unwrap_or_default();
    let c = ctx.db.conn().await?;
    let a = model::ensure_account(&ctx.db, &*c, player).await?;
    let linked: Option<String> = c
        .query_one(&ctx.db.sql("SELECT esports_id FROM chess.players WHERE id = $1"), &[&player])
        .await?
        .get(0);
    let tokens = c
        .query(
            &ctx.db.sql(
                "SELECT id, name, prefix, scopes, to_char(expires_at, 'YYYY-MM-DD'),
                        coalesce(to_char(last_used_at, 'YYYY-MM-DD'), 'never')
                   FROM chess.api_tokens
                  WHERE player_id = $1 AND revoked_at IS NULL AND expires_at > now()
                  ORDER BY created_at DESC",
            ),
            &[&player],
        )
        .await?;
    drop(c);
    let token_csrf = csrf(session, "tokens");
    let mut body = format!(
        "<p>Signed in as <b>{}</b>{}.</p>",
        html_escape(&a.username),
        if linked.is_some() {
            format!(" (linked to your {} account)", html_escape(&ctx.cfg.site_name))
        } else {
            String::new()
        }
    );
    if let Some(t) = fresh {
        let _ = write!(
            body,
            "<p class=ok>Your new token, shown once: copy it now.</p><p><code class=token>{}</code></p>",
            html_escape(t)
        );
    }
    body.push_str("<h2>API tokens</h2>");
    if tokens.is_empty() {
        body.push_str("<p class=muted>None yet.</p>");
    } else {
        body.push_str("<table><tr><th>Name</th><th>Token</th><th>Scopes</th><th>Expires</th><th>Last used</th><th></th></tr>");
        for t in &tokens {
            let id: i64 = t.get(0);
            let scopes: Vec<String> = t.get(3);
            let _ = write!(
                body,
                "<tr><td>{}</td><td><code>{}…</code></td><td>{}</td><td>{}</td><td>{}</td>\
                 <td><form method=post action=\"{base}/account/tokens/revoke\">{}{}<button>Revoke</button></form></td></tr>",
                html_escape(&t.get::<_, String>(1)),
                html_escape(&t.get::<_, String>(2)),
                html_escape(&scopes.join(" ")),
                t.get::<_, String>(4),
                t.get::<_, String>(5),
                hidden("csrf", &token_csrf),
                hidden("id", &id.to_string()),
            );
        }
        body.push_str("</table>");
    }
    let mut boxes = String::new();
    for (k, d) in SCOPES {
        let checked =
            if matches!(*k, "board:play" | "challenge:read" | "challenge:write") { " checked" } else { "" };
        let _ = write!(
            boxes,
            "<label class=check><input type=checkbox name=\"scope:{k}\"{checked}> {} <code>{k}</code></label>",
            html_escape(d)
        );
    }
    let _ = write!(
        body,
        "<h2>New token</h2><form method=post action=\"{base}/account/tokens\">{c}\
         <label>Name<input name=label placeholder=\"e.g. my board\" maxlength=80></label>{boxes}\
         <label>Days it lasts<input name=days type=number min=1 max=365 value=365></label>\
         <button class=go>Make the token</button></form>\
         <form method=post action=\"{base}/logout\"><button>Sign out</button></form>",
        c = hidden("csrf", &token_csrf)
    );
    Ok(page("Your chess account", &body))
}

async fn signed_in(ctx: &Ctx, h: &HeaderMap) -> ApiResult<Option<(i64, String)>> {
    let session = cookie(h, SESSION_COOKIE).unwrap_or_default();
    Ok(ctx.auth.session(h).await?.map(|p| (p, session)))
}

fn to_login(ctx: &Ctx, ret: &str) -> Response {
    let base = ctx.cfg.base_path().unwrap_or_default();
    Redirect::to(&format!("{base}/login?return={}", urlencoding::encode(ret))).into_response()
}

/// `GET /account`.
pub async fn account(State(ctx): State<Ctx>, h: HeaderMap) -> ApiResult<Response> {
    let base = ctx.cfg.base_path().unwrap_or_default();
    let Some((player, session)) = signed_in(&ctx, &h).await? else {
        return Ok(to_login(&ctx, &format!("{base}/account")));
    };
    account_page(&ctx, player, &session, None).await
}

/// `POST /account/tokens`.
pub async fn new_token(State(ctx): State<Ctx>, h: HeaderMap, body: Bytes) -> ApiResult<Response> {
    let (player, session) = signed_in(&ctx, &h).await?.ok_or_else(|| ApiError::forbidden("sign in first"))?;
    let f = form(&body);
    if f.get("csrf").map(String::as_str) != Some(csrf(&session, "tokens").as_str()) {
        return Err(ApiError::forbidden("this form has expired: reload the page"));
    }
    let wanted: Vec<String> = f.keys().filter_map(|k| k.strip_prefix("scope:")).map(str::to_owned).collect();
    let scopes = allowed(&ctx, player, &wanted).await?;
    let days = f.get("days").and_then(|d| d.parse().ok()).unwrap_or(365);
    let (token, _) = issue(&ctx, player, f.get("label").map_or("", String::as_str), &scopes, days).await?;
    account_page(&ctx, player, &session, Some(&token)).await
}

/// `POST /account/tokens/revoke`.
pub async fn revoke_token(State(ctx): State<Ctx>, h: HeaderMap, body: Bytes) -> ApiResult<Response> {
    let (player, session) = signed_in(&ctx, &h).await?.ok_or_else(|| ApiError::forbidden("sign in first"))?;
    let f = form(&body);
    if f.get("csrf").map(String::as_str) != Some(csrf(&session, "tokens").as_str()) {
        return Err(ApiError::forbidden("this form has expired: reload the page"));
    }
    let id: i64 = f.get("id").and_then(|i| i.parse().ok()).ok_or_else(|| ApiError::bad("which token?"))?;
    let c = ctx.db.conn().await?;
    c.execute(
        &ctx.db.sql(
            "UPDATE chess.api_tokens SET revoked_at = now(), revoked_by = 'chess: account page'
              WHERE id = $1 AND player_id = $2 AND revoked_at IS NULL",
        ),
        &[&id, &player],
    )
    .await?;
    let base = ctx.cfg.base_path().unwrap_or_default();
    Ok(Redirect::to(&format!("{base}/account")).into_response())
}

// ─── the host site ───

/// `GET /auth/esports`: off to the site to sign in.
pub async fn esports_start(
    State(ctx): State<Ctx>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    if ctx.cfg.esports().is_none() {
        return Err(ApiError::not_found("signing in with the site"));
    }
    let ret = back_to(&ctx, q.get("return").map(String::as_str));
    let nonce = random_token(24);
    let back = format!("{}/auth/esports/back", ctx.cfg.public_url());
    let to = format!(
        "{}/chess/sso?return={}&nonce={nonce}",
        ctx.cfg.site_url.trim_end_matches('/'),
        urlencoding::encode(&back)
    );
    let c = HeaderValue::from_str(&format!(
        "{SSO_COOKIE}={nonce}.{}; Path={}; Max-Age=600; HttpOnly; SameSite=Lax{}",
        B64.encode(&ret),
        cookie_path(&ctx),
        secure(&ctx)
    ))
    .map_err(|e| ApiError::from(anyhow::anyhow!("{e}")))?;
    Ok(redirect_with_cookies(&to, vec![c]))
}

/// `GET /auth/esports/back?a=…`: back from the site, signed in there.
pub async fn esports_back(
    State(ctx): State<Ctx>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let secret = ctx.cfg.esports().ok_or_else(|| ApiError::not_found("signing in with the site"))?.to_owned();
    let (nonce, ret) = cookie(&h, SSO_COOKIE)
        .and_then(|c| {
            let (n, r) = c.split_once('.')?;
            Some((n.to_owned(), String::from_utf8(B64.decode(r).ok()?).ok()?))
        })
        .ok_or_else(|| ApiError::forbidden("this sign-in has expired: start again"))?;
    let v = q
        .get("a")
        .and_then(|a| verify(&secret, a, Utc::now().timestamp()))
        .ok_or_else(|| ApiError::forbidden("the site's answer isn't valid or has expired: start again"))?;
    if v.get("nonce").and_then(Value::as_str) != Some(nonce.as_str()) {
        return Err(ApiError::forbidden("this sign-in was for another page: start again"));
    }
    let p = SitePlayer::from_json(&v).ok_or_else(|| ApiError::bad("the site didn't say who you are"))?;
    let (player, _) = link(&ctx, &p).await?;
    let session = start_session(&ctx, player).await?;
    let gone = HeaderValue::from_str(&format!("{SSO_COOKIE}=; Path={}; Max-Age=0", cookie_path(&ctx)))
        .map_err(|e| ApiError::from(anyhow::anyhow!("{e}")))?;
    Ok(redirect_with_cookies(&back_to(&ctx, Some(&ret)), vec![session, gone]))
}

/// `POST /api/esports/token`: the site's server asks for a token for one
/// of its players (who gets linked, and a chess account, on first use).
pub async fn esports_token(State(ctx): State<Ctx>, h: HeaderMap, body: Bytes) -> ApiResult<Response> {
    let secret = ctx.cfg.esports().ok_or_else(|| ApiError::not_found("the site's token endpoint"))?;
    if !bearer(&h).is_some_and(|b| same(b.as_bytes(), secret.as_bytes())) {
        return Err(ApiError::unauthorized());
    }
    let f = form(&body);
    let p = SitePlayer::from_fields(|k| f.get(k).cloned())
        .ok_or_else(|| ApiError::bad("sub (the site's player id) is required"))?;
    let (player, username) = link(&ctx, &p).await?;
    let wanted: Vec<String> = f.get("scopes").map_or_else(
        || ["board:play", "challenge:read", "challenge:write"].map(str::to_owned).to_vec(),
        |s| s.split([' ', ',']).filter(|x| !x.is_empty()).map(str::to_owned).collect(),
    );
    let scopes = allowed(&ctx, player, &wanted).await?;
    let label = f.get("label").map_or_else(|| format!("{} (site)", ctx.cfg.site_name), Clone::clone);
    let (token, expires) = match f.get("hours").and_then(|h| h.parse::<i64>().ok()) {
        Some(hours) => {
            issue_for(&ctx, player, &label, &scopes, Duration::hours(hours.clamp(1, 24 * 365))).await?
        }
        None => {
            issue(&ctx, player, &label, &scopes, f.get("days").and_then(|d| d.parse().ok()).unwrap_or(30))
                .await?
        }
    };
    Ok(Json(json!({
        "token": token,
        "username": username,
        "scopes": scopes,
        "expires": expires.timestamp_millis(),
    }))
    .into_response())
}

/// `GET /api/esports/players` (bearer the secret): the site's players
/// linked here, `{"players": [{"sub", "username"}]}`, so the site can tell
/// whose games the feed shows.
pub async fn esports_players(State(ctx): State<Ctx>, h: HeaderMap) -> ApiResult<Response> {
    let secret = ctx.cfg.esports().ok_or_else(|| ApiError::not_found("the site's token endpoint"))?;
    if !bearer(&h).is_some_and(|b| same(b.as_bytes(), secret.as_bytes())) {
        return Err(ApiError::unauthorized());
    }
    let c = ctx.db.conn().await?;
    let rows = c
        .query(
            &ctx.db.sql(
                "SELECT p.esports_id, a.username FROM chess.players p JOIN chess.accounts a ON a.player_id = p.id
                  WHERE p.esports_id IS NOT NULL ORDER BY p.id",
            ),
            &[],
        )
        .await?;
    let players: Vec<Value> = rows
        .iter()
        .map(|r| json!({"sub": r.get::<_, String>(0), "username": r.get::<_, String>(1)}))
        .collect();
    Ok(Json(json!({ "players": players })).into_response())
}

/// `POST /api/esports/revoke` (bearer the secret; form `label`, and
/// optionally `sub`): revoke the tokens of that name the site asked for
/// (all its players', or one's). Answers how many.
pub async fn esports_revoke(State(ctx): State<Ctx>, h: HeaderMap, body: Bytes) -> ApiResult<Response> {
    let secret = ctx.cfg.esports().ok_or_else(|| ApiError::not_found("the site's token endpoint"))?;
    if !bearer(&h).is_some_and(|b| same(b.as_bytes(), secret.as_bytes())) {
        return Err(ApiError::unauthorized());
    }
    let f = form(&body);
    let label = f.get("label").filter(|l| !l.is_empty()).ok_or_else(|| ApiError::bad("label is required"))?;
    let sub = f.get("sub").filter(|s| !s.is_empty());
    let c = ctx.db.conn().await?;
    let n = c
        .execute(
            &ctx.db.sql(
                "UPDATE chess.api_tokens t SET revoked_at = now(), revoked_by = 'site: ' || $1
                   FROM chess.players p
                  WHERE p.id = t.player_id AND p.esports_id IS NOT NULL AND ($2::text IS NULL OR p.esports_id = $2)
                    AND t.name = $1 AND t.revoked_at IS NULL",
            ),
            &[label, &sub],
        )
        .await?;
    Ok(Json(json!({ "revoked": n })).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc_4231() {
        // Test case 2.
        assert_eq!(
            hex::encode(hmac(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Test case 6: a key longer than the block.
        assert_eq!(
            hex::encode(hmac(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn signed_statements() {
        let secret = "s".repeat(32);
        let v = json!({"sub": "42", "name": "Mark", "roles": ["manager"], "nonce": "n", "exp": 2000});
        let s = sign(&secret, &v);
        assert_eq!(verify(&secret, &s, 1000), Some(v.clone()));
        assert_eq!(verify(&secret, &s, 3000), None, "expired");
        assert_eq!(verify(&"t".repeat(32), &s, 1000), None, "another secret");
        let (body, mac) = s.split_once('.').unwrap();
        let forged = format!("{}x.{mac}", &body[..body.len() - 1]);
        assert_eq!(verify(&secret, &forged, 1000), None, "changed");
        let p = SitePlayer::from_json(&v).unwrap();
        assert_eq!(
            (p.sub.as_str(), p.name.as_str(), p.roles.as_slice()),
            ("42", "Mark", &["manager".to_owned()][..])
        );
    }
}
