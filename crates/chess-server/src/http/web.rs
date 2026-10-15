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

use crate::app::{Ctx, Site, Who};
use crate::auth::{SCOPES, SESSION_COOKIE, TOKEN_PREFIX, cookie, hash_key, random_token};
use crate::error::{ApiError, ApiResult};
use crate::http::oauth::page;
use crate::http::{form, html_escape};
use crate::{model, passkeys};

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

use crate::auth::same;

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
/// to date: the email, which the site vouches for). The site's roles stay
/// the site's: roles here are this server's own. Returns the player's id and
/// chess username.
pub async fn link(ctx: &Ctx, p: &SitePlayer) -> ApiResult<(i64, String)> {
    let c = ctx.db.conn().await?;
    let id: i64 = c
        .query_one(
            &ctx.db.sql(
                "INSERT INTO chess.players (name, email, email_verified_at, esports_id, key_hash)
                 VALUES ($1, $2, CASE WHEN $2::text IS NULL THEN NULL ELSE now() END, $3, $4)
                 ON CONFLICT (esports_id) DO UPDATE
                    SET email = EXCLUDED.email, email_verified_at = EXCLUDED.email_verified_at
                 RETURNING id",
            ),
            &[&p.name, &p.email, &p.sub, &hash_key(&random_token(28))],
        )
        .await?
        .get(0);
    let a = model::ensure_account(&ctx.db, &*c, id).await?;
    drop(c);
    grant_listed(ctx, id).await?;
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

/// `CHESS_ADMIN_EMAILS`: this player becomes an admin if their confirmed
/// email is listed and they have a passkey (an admin's rights need one).
/// Their email is as good as the host site's sign-in that vouched for it.
pub async fn grant_listed(ctx: &Ctx, player: i64) -> ApiResult<()> {
    let emails = ctx.cfg.admin_emails();
    if emails.is_empty() {
        return Ok(());
    }
    let c = ctx.db.conn().await?;
    let n = c
        .execute(
            &ctx.db.sql(
                "UPDATE chess.players SET role = 'admin'
                  WHERE id = $1 AND role <> 'admin' AND email_verified_at IS NOT NULL AND lower(email) = ANY($2)
                    AND EXISTS (SELECT 1 FROM chess.passkeys WHERE player_id = $1)",
            ),
            &[&player, &emails],
        )
        .await?;
    if n > 0 {
        tracing::info!(player, "made an admin (CHESS_ADMIN_EMAILS)");
        ctx.auth.forget_all();
    }
    Ok(())
}

/// Start a session: the cookie to set. `checked`: signed in with a passkey,
/// which counts as an admin's or manager's passkey check.
async fn start_session(ctx: &Ctx, player: i64, checked: bool) -> ApiResult<HeaderValue> {
    grant_listed(ctx, player).await?;
    let token = random_token(40);
    let c = ctx.db.conn().await?;
    c.execute(
        &ctx.db.sql(
            "INSERT INTO chess.sessions (token_hash, player_id, expires_at, staff_verified_at)
             VALUES ($1, $2, now() + make_interval(days => $3), CASE WHEN $4 THEN now() END)",
        ),
        &[&hash_key(&token), &player, &i32::try_from(SESSION_DAYS).unwrap_or(30), &checked],
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

/// A string as a JavaScript literal that's safe inside an inline
/// `<script>`: no `<`, `>` or `&` survive to end the script or start a
/// comment.
fn js_string(s: &str) -> String {
    serde_json::to_string(s)
        .unwrap_or_else(|_| "\"\"".into())
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

/// Where to go after signing in: a path on this server, or the Account page.
fn back_to(ctx: &Ctx, want: Option<&str>) -> String {
    let base = ctx.cfg.base_path().unwrap_or_default();
    match want {
        Some(p)
            if p.starts_with('/')
                && !p.starts_with("//")
                && !p.contains('\\')
                && p.chars().all(|c| c.is_ascii_graphic() && !matches!(c, '<' | '>' | '"' | '\'' | '`')) =>
        {
            p.to_owned()
        }
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
            site = html_escape(ctx.cfg.host_name())
        );
    }
    let _ = write!(
        body,
        "<form method=post action=\"{base}/login\">{h}\
         <label>Username or email<input name=name autocomplete=\"username webauthn\" required></label>\
         <label>Password<input name=password type=password autocomplete=current-password required></label>\
         <button class=go>Sign in</button></form>\
         <p class=pk hidden><button type=button id=pkSignIn>Sign in with a passkey</button></p><p class=err id=pkErr></p>\
         <script>{js}\
         if (pkWorks()) {{ document.querySelector('.pk').hidden = false; }}\
         document.getElementById('pkSignIn').onclick = () => pkRun(async () => {{\
           const r = await pkGet('{base}/auth/passkey/options', {{}}, '{base}/auth/passkey', {{ return: {ret_js} }});\
           location.assign(r.to);\
         }});</script>",
        h = hidden("return", ret),
        js = PASSKEY_JS,
        ret_js = js_string(ret),
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
    let cookie = start_session(&ctx, row.get(0), false).await?;
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
         <label>Password (10 or more characters)<input name=password type=password autocomplete=new-password minlength=10></label>\
         <button class=go>Make the account</button></form>\
         <p class=pk hidden>Or, instead of a password: <button type=button id=pkSignUp>Make it with a passkey</button></p><p class=err id=pkErr></p>\
         <p class=muted>Already have one? <a href=\"{base}/login?return={r}\">Sign in</a>.</p>\
         <script>{js}\
         if (pkWorks()) {{ document.querySelector('.pk').hidden = false; }}\
         document.getElementById('pkSignUp').onclick = () => pkRun(async () => {{\
           const f = document.querySelector('form');\
           const who = {{ name: f.name.value.trim(), email: f.email.value.trim() }};\
           const r = await pkCreate('{base}/signup/passkey/options', who, '{base}/signup/passkey', {{ return: {ret_js} }});\
           location.assign(r.to);\
         }});</script>",
        h = hidden("return", ret),
        n = html_escape(name),
        e = html_escape(email),
        r = urlencoding::encode(ret),
        js = PASSKEY_JS,
        ret_js = js_string(ret),
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
    if let Some(why) = name_problem(&name, &email) {
        return refuse(why);
    }
    if password.chars().count() < 10 {
        return refuse("The password needs 10 or more characters.");
    }
    let hash = hash_password(&password)?;
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    if let Some(why) = taken(&ctx, &tx, &name, &email).await? {
        return refuse(why);
    }
    let id = make_account(&ctx, &tx, &name, &email, Some(&hash)).await?;
    tx.commit().await?;
    let cookie = start_session(&ctx, id, false).await?;
    Ok(redirect_with_cookies(&ret, vec![cookie]))
}

/// What's wrong with a new account's name or email, if anything.
pub(crate) fn name_problem(name: &str, email: &str) -> Option<&'static str> {
    let valid = name.len() >= 2
        && name.len() <= 20
        && name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !valid {
        return Some("A username is 2 to 20 letters, digits, _ and -, starting with a letter or digit.");
    }
    if name.to_lowercase().starts_with("ai-level-") {
        return Some("That name is the computer's.");
    }
    if !email.is_empty() && !email.contains('@') {
        return Some("That email doesn't look right.");
    }
    None
}

/// The name or email someone here already has, if so.
pub(crate) async fn taken(
    ctx: &Ctx,
    c: &impl tokio_postgres::GenericClient,
    name: &str,
    email: &str,
) -> ApiResult<Option<&'static str>> {
    if model::account_by_name(&ctx.db, c, name).await?.is_some() {
        return Ok(Some("That username is taken."));
    }
    if !email.is_empty()
        && c.query_opt(&ctx.db.sql("SELECT 1 FROM chess.players WHERE lower(email) = lower($1)"), &[&email])
            .await?
            .is_some()
    {
        return Ok(Some("An account here already has that email."));
    }
    Ok(None)
}

fn hash_password(password: &str) -> ApiResult<String> {
    Ok(Argon2::default()
        .hash_password(password.as_bytes())
        .map_err(|e| ApiError::from(anyhow::anyhow!("hashing the password: {e}")))?
        .to_string())
}

/// A new player and their chess account.
pub(crate) async fn make_account(
    ctx: &Ctx,
    c: &impl tokio_postgres::GenericClient,
    name: &str,
    email: &str,
    password_hash: Option<&str>,
) -> ApiResult<i64> {
    let id: i64 = c
        .query_one(
            &ctx.db.sql(
                "INSERT INTO chess.players (name, email, password_hash) VALUES ($1, $2, $3) RETURNING id",
            ),
            &[&name, &(!email.is_empty()).then_some(email), &password_hash],
        )
        .await?
        .get(0);
    c.execute(&ctx.db.sql("INSERT INTO chess.accounts (player_id, username) VALUES ($1, $2)"), &[&id, &name])
        .await?;
    Ok(id)
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
    let me = c
        .query_one(
            &ctx.db.sql(
                "SELECT esports_id, role, roles, password_hash IS NOT NULL,
                        coalesce(s.staff_verified_at > now() - make_interval(hours => $3), false)
                   FROM chess.players p LEFT JOIN chess.sessions s ON s.token_hash = $2
                  WHERE p.id = $1",
            ),
            &[&player, &hash_key(session), &ctx.cfg.staff_passkey_hours],
        )
        .await?;
    let linked: Option<String> = me.get(0);
    let (admin, manager) = crate::auth::staff_roles(&me.get::<_, String>(1), &me.get::<_, Vec<String>>(2));
    let has_password: bool = me.get(3);
    let checked: bool = me.get(4);
    let keys = c
        .query(
            &ctx.db.sql(
                "SELECT id, name, to_char(created_at, 'YYYY-MM-DD'), coalesce(to_char(last_used_at, 'YYYY-MM-DD'), 'never')
                   FROM chess.passkeys WHERE player_id = $1 ORDER BY id",
            ),
            &[&player],
        )
        .await?;
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
    let pk_csrf = csrf(session, "passkeys");
    let mut body = format!(
        "<p>Signed in as <b>{}</b>{}.</p>",
        html_escape(&a.username),
        if linked.is_some() {
            format!(" (linked to your {} account)", html_escape(ctx.cfg.host_name()))
        } else {
            String::new()
        }
    );
    if admin || manager {
        let what = if admin { "an admin" } else { "a manager" };
        if checked {
            let _ = write!(body, "<p class=ok>You're {what} here, with your passkey checked.</p>");
        } else if keys.is_empty() {
            let _ = write!(
                body,
                "<p>You're {what} here. Your rights need a passkey check: add a passkey below first.</p>"
            );
        } else {
            let _ = write!(
                body,
                "<p>You're {what} here. Your rights need a passkey check: confirm it's you below.</p>"
            );
        }
    }
    // Changing passkeys or the password needs the passkey check first.
    let locked = !keys.is_empty() && !checked;
    if locked {
        body.push_str(
            "<p class=pk hidden><button type=button class=go id=pkStaff>Confirm it's you with your passkey</button> \
             <span class=muted>to change your passkeys or password, or use an admin's or manager's rights.</span></p>",
        );
    }
    if let Some(t) = fresh {
        let _ = write!(
            body,
            "<p class=ok>Your new token, shown once: copy it now.</p><p><code class=token>{}</code></p>",
            html_escape(t)
        );
    }

    body.push_str("<h2>Passkeys</h2><p class=muted>Sign in with your fingerprint, face or device PIN instead of a password.</p>");
    if keys.is_empty() {
        body.push_str("<p class=muted>None yet.</p>");
    } else {
        body.push_str("<table><tr><th>Name</th><th>Added</th><th>Last used</th><th></th></tr>");
        for k in &keys {
            let id: i64 = k.get(0);
            // The last passkey of an account with no password stays.
            let removable = !locked && (has_password || keys.len() > 1);
            let _ = write!(
                body,
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                html_escape(&k.get::<_, String>(1)),
                k.get::<_, String>(2),
                k.get::<_, String>(3),
                if removable {
                    format!(
                        "<form method=post action=\"{base}/account/passkeys/remove\">{}{}<button>Remove</button></form>",
                        hidden("csrf", &pk_csrf),
                        hidden("id", &id.to_string())
                    )
                } else {
                    String::new()
                }
            );
        }
        body.push_str("</table>");
    }
    if !locked {
        body.push_str(
            "<p class=pk hidden><label>Name for it<input id=pkName maxlength=60 placeholder=\"e.g. my phone\"></label>\
             <button type=button id=pkAdd>Add a passkey</button></p>",
        );
    }
    body.push_str("<p class=err id=pkErr></p><h2>Password</h2>");
    if locked {
        body.push_str("<p class=muted>Confirm it's you with your passkey (above) to change it.</p>");
    } else {
        let _ = write!(
            body,
            "<form method=post action=\"{base}/account/password\">{c}{current}\
             <label>{label} (10 or more characters)<input name=password type=password autocomplete=new-password required minlength=10></label>\
             <button>{verb}</button></form>",
            c = hidden("csrf", &pk_csrf),
            current = if has_password && keys.is_empty() {
                "<label>Your current password<input name=current type=password autocomplete=current-password required></label>"
            } else {
                ""
            },
            label = if has_password { "New password" } else { "A password" },
            verb = if has_password { "Change the password" } else { "Set a password" },
        );
    }
    if has_password && !keys.is_empty() && !locked {
        let _ = write!(
            body,
            "<form method=post action=\"{base}/account/password/remove\">{}<button>Sign in with passkeys only</button></form>",
            hidden("csrf", &pk_csrf)
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
         <form method=post action=\"{base}/logout\"><button>Sign out</button></form>\
         <script>{js}\
         const csrf = {pk_csrf_js};\
         if (pkWorks()) document.querySelectorAll('.pk').forEach((e) => (e.hidden = false));\
         const add = document.getElementById('pkAdd');\
         if (add) add.onclick = () => pkRun(async () => {{\
           await pkCreate('{base}/account/passkeys/options', {{}}, '{base}/account/passkeys', {{ name: document.getElementById('pkName').value }}, csrf);\
           location.reload();\
         }});\
         const staff = document.getElementById('pkStaff');\
         if (staff) staff.onclick = () => pkRun(async () => {{\
           await pkGet('{base}/account/staff/options', {{}}, '{base}/account/staff', {{}}, csrf);\
           location.reload();\
         }});</script>",
        c = hidden("csrf", &token_csrf),
        js = PASSKEY_JS,
        pk_csrf_js = js_string(&pk_csrf),
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

// ─── passkeys ───

/// The pages' passkey helpers: `pkWorks()`, `pkGet(optionsUrl, body,
/// doneUrl, extra, csrf)` (sign in, or the staff check), `pkCreate(…)` (a
/// new passkey) and `pkRun(fn)` (shows a failure in `#pkErr`).
const PASSKEY_JS: &str = r"
const b64u = {
  enc: (b) => btoa(String.fromCharCode(...new Uint8Array(b))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, ''),
  dec: (s) => Uint8Array.from(atob(s.replace(/-/g, '+').replace(/_/g, '/')), (c) => c.charCodeAt(0)),
};
const pkWorks = () => !!(window.PublicKeyCredential && navigator.credentials);
async function pkPost(url, body, csrf) {
  const r = await fetch(url, { method: 'POST', credentials: 'same-origin',
    headers: Object.assign({ 'Content-Type': 'application/json' }, csrf ? { 'X-CSRF': csrf } : {}),
    body: JSON.stringify(body || {}) });
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(j.error || ('error ' + r.status));
  return j;
}
const ids = (l) => (l || []).map((c) => Object.assign({}, c, { id: b64u.dec(c.id) }));
async function pkGet(optUrl, optBody, doneUrl, extra, csrf) {
  const o = await pkPost(optUrl, optBody, csrf);
  const pk = o.public_key;
  const cred = await navigator.credentials.get({ publicKey: Object.assign({}, pk, {
    challenge: b64u.dec(pk.challenge), allowCredentials: ids(pk.allowCredentials) }) });
  const r = cred.response;
  return pkPost(doneUrl, Object.assign({ challenge_id: o.challenge_id, id: b64u.enc(cred.rawId),
    client_data_json: b64u.enc(r.clientDataJSON), authenticator_data: b64u.enc(r.authenticatorData),
    signature: b64u.enc(r.signature), user_handle: r.userHandle ? b64u.enc(r.userHandle) : null }, extra), csrf);
}
async function pkCreate(optUrl, optBody, doneUrl, extra, csrf) {
  const o = await pkPost(optUrl, optBody, csrf);
  const pk = o.public_key;
  const cred = await navigator.credentials.create({ publicKey: Object.assign({}, pk, {
    challenge: b64u.dec(pk.challenge), user: Object.assign({}, pk.user, { id: b64u.dec(pk.user.id) }),
    excludeCredentials: ids(pk.excludeCredentials) }) });
  const r = cred.response;
  return pkPost(doneUrl, Object.assign({ challenge_id: o.challenge_id,
    client_data_json: b64u.enc(r.clientDataJSON), attestation_object: b64u.enc(r.attestationObject) }, extra), csrf);
}
async function pkRun(fn) {
  const err = document.getElementById('pkErr');
  err.textContent = '';
  try { await fn(); } catch (e) {
    err.textContent = e.name === 'NotAllowedError' ? 'The passkey request was cancelled or timed out.' : e.message;
  }
}
";

fn rp(ctx: &Ctx) -> ApiResult<passkeys::Rp> {
    passkeys::Rp::from_public_url(ctx.cfg.public_url()).map_err(|e| ApiError::from(anyhow::anyhow!("{e:#}")))
}

/// A passkey that didn't pass: the reason, to the person.
fn refused(e: &anyhow::Error) -> ApiError {
    tracing::info!("passkey refused: {e:#}");
    ApiError::forbidden(format!("{e:#}"))
}

/// The session's player, for the account page's passkey calls (with the
/// page's CSRF header).
async fn session_for(ctx: &Ctx, h: &HeaderMap) -> ApiResult<(i64, String)> {
    let (player, session) = signed_in(ctx, h).await?.ok_or_else(|| ApiError::forbidden("sign in first"))?;
    let sent = h.get("x-csrf").and_then(|v| v.to_str().ok()).unwrap_or_default();
    if !same(sent.as_bytes(), csrf(&session, "passkeys").as_bytes()) {
        return Err(ApiError::forbidden("this page has expired: reload it"));
    }
    Ok((player, session))
}

#[derive(serde::Deserialize, Default)]
pub struct NewAccount {
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
}

/// `POST /auth/passkey/options`: a challenge to sign in with any of this
/// server's passkeys.
pub async fn passkey_signin_options(State(ctx): State<Ctx>) -> ApiResult<Response> {
    let c = ctx.db.conn().await?;
    let (id, challenge) = passkeys::new_challenge(&ctx.db, &*c, passkeys::SIGNIN, None, None).await?;
    Ok(Json(
        json!({"challenge_id": id, "public_key": passkeys::request_options(&rp(&ctx)?, &challenge, &[])}),
    )
    .into_response())
}

#[derive(serde::Deserialize)]
pub struct SignIn {
    #[serde(flatten)]
    assertion: passkeys::AssertionJson,
    #[serde(rename = "return")]
    ret: Option<String>,
}

/// `POST /auth/passkey`: signed in with a passkey (which is also the staff
/// passkey check). Answers where to go next.
pub async fn passkey_signin(State(ctx): State<Ctx>, Json(b): Json<SignIn>) -> ApiResult<Response> {
    let c = ctx.db.conn().await?;
    let (player, _) =
        passkeys::check_assertion(&ctx.db, &*c, &rp(&ctx)?, passkeys::SIGNIN, &b.assertion, None)
            .await
            .map_err(|e| refused(&e))?;
    drop(c);
    let cookie = start_session(&ctx, player, true).await?;
    let mut r = Json(json!({"to": back_to(&ctx, b.ret.as_deref())})).into_response();
    r.headers_mut().append(header::SET_COOKIE, cookie);
    Ok(r)
}

/// `POST /signup/passkey/options` (`{name, email}`): a challenge to make an
/// account with a passkey instead of a password.
pub async fn signup_passkey_options(
    State(ctx): State<Ctx>,
    Json(b): Json<NewAccount>,
) -> ApiResult<Response> {
    if !ctx.cfg.signup {
        return Err(ApiError::forbidden("this server doesn't take new accounts"));
    }
    let (name, email) = (b.name.trim(), b.email.trim());
    if let Some(why) = name_problem(name, email) {
        return Err(ApiError::bad(why));
    }
    let c = ctx.db.conn().await?;
    if let Some(why) = taken(&ctx, &*c, name, email).await? {
        return Err(ApiError::bad(why));
    }
    let handle: Vec<u8> = (0..16).map(|_| rand::random::<u8>()).collect();
    let data = json!({"name": name, "email": email, "handle": B64.encode(&handle)});
    let (id, challenge) = passkeys::new_challenge(&ctx.db, &*c, passkeys::SIGNUP, None, Some(&data)).await?;
    Ok(Json(json!({
        "challenge_id": id,
        "public_key": passkeys::creation_options(&rp(&ctx)?, &handle, name, &challenge, &[]),
    }))
    .into_response())
}

#[derive(serde::Deserialize)]
pub struct SignUp {
    #[serde(flatten)]
    attestation: passkeys::AttestationJson,
    #[serde(rename = "return")]
    ret: Option<String>,
}

/// `POST /signup/passkey`: the new account, with its passkey, signed in.
pub async fn signup_passkey(State(ctx): State<Ctx>, Json(form): Json<SignUp>) -> ApiResult<Response> {
    if !ctx.cfg.signup {
        return Err(ApiError::forbidden("this server doesn't take new accounts"));
    }
    let att = &form.attestation;
    let mut conn = ctx.db.conn().await?;
    let tx = conn.transaction().await?;
    let taken_ch = passkeys::take_challenge(&ctx.db, &tx, att.challenge_id, passkeys::SIGNUP)
        .await
        .map_err(|e| refused(&e))?;
    let data = taken_ch.data.unwrap_or_default();
    let field = |k: &str| data.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    let (name, email) = (field("name"), field("email"));
    let handle = passkeys::b64("handle", &field("handle")).map_err(|e| refused(&e))?;
    let key = passkeys::verify_registration(
        &rp(&ctx)?,
        &taken_ch.challenge,
        &passkeys::b64("client_data_json", &att.client_data_json).map_err(|e| refused(&e))?,
        &passkeys::b64("attestation_object", &att.attestation_object).map_err(|e| refused(&e))?,
    )
    .map_err(|e| refused(&e))?;
    if let Some(why) = taken(&ctx, &tx, &name, &email).await? {
        return Err(ApiError::bad(why));
    }
    let player = make_account(&ctx, &tx, &name, &email, None).await?;
    passkeys::add(&ctx.db, &tx, player, &handle, "passkey", &key)
        .await
        .map_err(|e| ApiError::bad(format!("{e:#}")))?;
    tx.commit().await?;
    let cookie = start_session(&ctx, player, true).await?;
    let mut r = Json(json!({"to": back_to(&ctx, form.ret.as_deref())})).into_response();
    r.headers_mut().append(header::SET_COOKIE, cookie);
    Ok(r)
}

/// `POST /account/passkeys/options`: a challenge to add a passkey.
pub async fn account_passkey_options(State(ctx): State<Ctx>, h: HeaderMap) -> ApiResult<Response> {
    let (player, session) = session_for(&ctx, &h).await?;
    may_change(&ctx, player, &session).await?;
    let c = ctx.db.conn().await?;
    let a = model::ensure_account(&ctx.db, &*c, player).await?;
    let exclude = passkeys::ids(&ctx.db, &*c, player).await?;
    let (id, challenge) =
        passkeys::new_challenge(&ctx.db, &*c, passkeys::REGISTER, Some(player), None).await?;
    Ok(Json(json!({
        "challenge_id": id,
        "public_key": passkeys::creation_options(&rp(&ctx)?, &passkeys::user_handle(player), &a.username, &challenge, &exclude),
    }))
    .into_response())
}

/// `POST /account/passkeys`: the new passkey.
pub async fn account_passkey_add(
    State(ctx): State<Ctx>,
    h: HeaderMap,
    Json(att): Json<passkeys::AttestationJson>,
) -> ApiResult<Response> {
    let (player, session) = session_for(&ctx, &h).await?;
    may_change(&ctx, player, &session).await?;
    let c = ctx.db.conn().await?;
    let taken_ch = passkeys::take_challenge(&ctx.db, &*c, att.challenge_id, passkeys::REGISTER)
        .await
        .map_err(|e| refused(&e))?;
    if taken_ch.player != Some(player) {
        return Err(ApiError::forbidden("that passkey request was for someone else"));
    }
    let key = passkeys::verify_registration(
        &rp(&ctx)?,
        &taken_ch.challenge,
        &passkeys::b64("client_data_json", &att.client_data_json).map_err(|e| refused(&e))?,
        &passkeys::b64("attestation_object", &att.attestation_object).map_err(|e| refused(&e))?,
    )
    .map_err(|e| refused(&e))?;
    let name: String = att.name.as_deref().unwrap_or_default().trim().chars().take(60).collect();
    let name = if name.is_empty() { "passkey".to_owned() } else { name };
    passkeys::add(&ctx.db, &*c, player, &passkeys::user_handle(player), &name, &key)
        .await
        .map_err(|e| ApiError::bad(format!("{e:#}")))?;
    drop(c);
    grant_listed(&ctx, player).await?;
    Ok(Json(json!({"ok": true})).into_response())
}

/// Changing how an account signs in (its passkeys, its password) needs a
/// fresh passkey check in this session when it has passkeys, so a stolen
/// password or session can't add the thief's own.
async fn may_change(ctx: &Ctx, player: i64, session: &str) -> ApiResult<()> {
    let c = ctx.db.conn().await?;
    let r = c
        .query_one(
            &ctx.db.sql(
                "SELECT EXISTS (SELECT 1 FROM chess.passkeys WHERE player_id = $1),
                        coalesce((SELECT s.staff_verified_at > now() - make_interval(hours => $3)
                                    FROM chess.sessions s WHERE s.token_hash = $2), false)",
            ),
            &[&player, &hash_key(session), &ctx.cfg.staff_passkey_hours],
        )
        .await?;
    let (has_keys, checked): (bool, bool) = (r.get(0), r.get(1));
    if has_keys && !checked {
        return Err(ApiError::forbidden("confirm it's you with one of your passkeys first"));
    }
    Ok(())
}

/// Forget passkey checks made in this player's sessions and tokens (a
/// passkey went: a check made with it mustn't outlive it).
async fn forget_checks(ctx: &Ctx, player: i64) -> ApiResult<()> {
    let c = ctx.db.conn().await?;
    c.execute(
        &ctx.db.sql("UPDATE chess.sessions SET staff_verified_at = NULL WHERE player_id = $1"),
        &[&player],
    )
    .await?;
    c.execute(
        &ctx.db.sql("UPDATE chess.api_tokens SET staff_verified_at = NULL WHERE player_id = $1"),
        &[&player],
    )
    .await?;
    ctx.auth.forget_all();
    Ok(())
}

fn form_csrf_ok(f: &HashMap<String, String>, session: &str) -> ApiResult<()> {
    if f.get("csrf").map(String::as_str) == Some(csrf(session, "passkeys").as_str()) {
        Ok(())
    } else {
        Err(ApiError::forbidden("this form has expired: reload the page"))
    }
}

fn to_account(ctx: &Ctx) -> Response {
    let base = ctx.cfg.base_path().unwrap_or_default();
    Redirect::to(&format!("{base}/account")).into_response()
}

/// `POST /account/passkeys/remove` (form `id`). The last passkey of an
/// account without a password stays.
pub async fn account_passkey_remove(
    State(ctx): State<Ctx>,
    h: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let (player, session) = signed_in(&ctx, &h).await?.ok_or_else(|| ApiError::forbidden("sign in first"))?;
    let f = form(&body);
    form_csrf_ok(&f, &session)?;
    let id: i64 = f.get("id").and_then(|i| i.parse().ok()).ok_or_else(|| ApiError::bad("which passkey?"))?;
    may_change(&ctx, player, &session).await?;
    let c = ctx.db.conn().await?;
    let n = c
        .execute(
            &ctx.db.sql(
                "DELETE FROM chess.passkeys k WHERE k.id = $1 AND k.player_id = $2
               AND ((SELECT password_hash IS NOT NULL FROM chess.players WHERE id = $2)
                    OR (SELECT count(*) FROM chess.passkeys WHERE player_id = $2) > 1)",
            ),
            &[&id, &player],
        )
        .await?;
    drop(c);
    if n > 0 {
        forget_checks(&ctx, player).await?;
    }
    Ok(to_account(&ctx))
}

/// `POST /account/password` (form `password`, and `current`: the password
/// it replaces, unless a passkey confirmed it's you): set or change it.
pub async fn account_password(State(ctx): State<Ctx>, h: HeaderMap, body: Bytes) -> ApiResult<Response> {
    let (player, session) = signed_in(&ctx, &h).await?.ok_or_else(|| ApiError::forbidden("sign in first"))?;
    let f = form(&body);
    form_csrf_ok(&f, &session)?;
    may_change(&ctx, player, &session).await?;
    let password = f.get("password").cloned().unwrap_or_default();
    if password.chars().count() < 10 {
        return Err(ApiError::bad("The password needs 10 or more characters."));
    }
    let c = ctx.db.conn().await?;
    let r = c
        .query_one(
            &ctx.db.sql(
                "SELECT password_hash, EXISTS (SELECT 1 FROM chess.passkeys WHERE player_id = $1)
                   FROM chess.players WHERE id = $1",
            ),
            &[&player],
        )
        .await?;
    let (old, has_keys): (Option<String>, bool) = (r.get(0), r.get(1));
    // Without passkeys (so without a passkey check), the old password says
    // it's you.
    if let Some(old) = old.filter(|_| !has_keys) {
        let current = f.get("current").map_or("", String::as_str);
        let right = PasswordHash::new(&old)
            .is_ok_and(|h| Argon2::default().verify_password(current.as_bytes(), &h).is_ok());
        if !right {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            return Err(ApiError::forbidden("your current password doesn't match"));
        }
    }
    let hash = hash_password(&password)?;
    c.execute(&ctx.db.sql("UPDATE chess.players SET password_hash = $2 WHERE id = $1"), &[&player, &hash])
        .await?;
    Ok(to_account(&ctx))
}

/// `POST /account/password/remove`: passkeys only (with one, at least).
pub async fn account_password_remove(
    State(ctx): State<Ctx>,
    h: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let (player, session) = signed_in(&ctx, &h).await?.ok_or_else(|| ApiError::forbidden("sign in first"))?;
    form_csrf_ok(&form(&body), &session)?;
    may_change(&ctx, player, &session).await?;
    let c = ctx.db.conn().await?;
    c.execute(
        &ctx.db.sql(
            "UPDATE chess.players SET password_hash = NULL
              WHERE id = $1 AND EXISTS (SELECT 1 FROM chess.passkeys WHERE player_id = $1)",
        ),
        &[&player],
    )
    .await?;
    Ok(to_account(&ctx))
}

/// A challenge for an admin's or manager's passkey check (their own keys).
async fn staff_options(ctx: &Ctx, player: i64) -> ApiResult<Response> {
    let c = ctx.db.conn().await?;
    let allow = passkeys::ids(&ctx.db, &*c, player).await?;
    if allow.is_empty() {
        return Err(ApiError::bad("add a passkey on your account page first"));
    }
    let (id, challenge) = passkeys::new_challenge(&ctx.db, &*c, passkeys::STAFF, Some(player), None).await?;
    Ok(Json(
        json!({"challenge_id": id, "public_key": passkeys::request_options(&rp(ctx)?, &challenge, &allow)}),
    )
    .into_response())
}

/// `POST /account/staff/options`.
pub async fn account_staff_options(State(ctx): State<Ctx>, h: HeaderMap) -> ApiResult<Response> {
    let (player, _) = session_for(&ctx, &h).await?;
    staff_options(&ctx, player).await
}

/// `POST /account/staff`: the passkey check, for this session.
pub async fn account_staff(
    State(ctx): State<Ctx>,
    h: HeaderMap,
    Json(a): Json<passkeys::AssertionJson>,
) -> ApiResult<Response> {
    let (player, session) = session_for(&ctx, &h).await?;
    let c = ctx.db.conn().await?;
    passkeys::check_assertion(&ctx.db, &*c, &rp(&ctx)?, passkeys::STAFF, &a, Some(player))
        .await
        .map_err(|e| refused(&e))?;
    c.execute(
        &ctx.db.sql("UPDATE chess.sessions SET staff_verified_at = now() WHERE token_hash = $1"),
        &[&hash_key(&session)],
    )
    .await?;
    Ok(Json(json!({"ok": true})).into_response())
}

/// `POST /api/account/passkey/options` (a token): a challenge for the staff
/// passkey check, made in an app.
pub async fn api_staff_options(State(ctx): State<Ctx>, who: Who) -> ApiResult<Response> {
    staff_options(&ctx, who.0.player).await
}

/// `POST /api/account/passkey`: the passkey check, for the token used.
pub async fn api_staff(
    State(ctx): State<Ctx>,
    who: Who,
    h: HeaderMap,
    Json(a): Json<passkeys::AssertionJson>,
) -> ApiResult<Response> {
    let c = ctx.db.conn().await?;
    passkeys::check_assertion(&ctx.db, &*c, &rp(&ctx)?, passkeys::STAFF, &a, Some(who.0.player))
        .await
        .map_err(|e| refused(&e))?;
    c.execute(
        &ctx.db.sql("UPDATE chess.api_tokens SET staff_verified_at = now() WHERE id = $1"),
        &[&who.0.token_id],
    )
    .await?;
    if let Some(t) = crate::auth::bearer(&h) {
        ctx.auth.forget(t);
    }
    Ok(Json(json!({"ok": true})).into_response())
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
    let session = start_session(&ctx, player, false).await?;
    let gone = HeaderValue::from_str(&format!("{SSO_COOKIE}=; Path={}; Max-Age=0", cookie_path(&ctx)))
        .map_err(|e| ApiError::from(anyhow::anyhow!("{e}")))?;
    Ok(redirect_with_cookies(&back_to(&ctx, Some(&ret)), vec![session, gone]))
}

/// `POST /api/esports/token`: the site's server asks for a token for one
/// of its players (who gets linked, and a chess account, on first use).
pub async fn esports_token(State(ctx): State<Ctx>, _site: Site, body: Bytes) -> ApiResult<Response> {
    let f = form(&body);
    let p = SitePlayer::from_fields(|k| f.get(k).cloned())
        .ok_or_else(|| ApiError::bad("sub (the site's player id) is required"))?;
    let (player, username) = link(&ctx, &p).await?;
    let wanted: Vec<String> = f.get("scopes").map_or_else(
        || ["board:play", "challenge:read", "challenge:write"].map(str::to_owned).to_vec(),
        |s| s.split([' ', ',']).filter(|x| !x.is_empty()).map(str::to_owned).collect(),
    );
    let scopes = allowed(&ctx, player, &wanted).await?;
    let label = f.get("label").map_or_else(|| format!("{} (site)", ctx.cfg.host_name()), Clone::clone);
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
pub async fn esports_players(State(ctx): State<Ctx>, _site: Site) -> ApiResult<Response> {
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
pub async fn esports_revoke(State(ctx): State<Ctx>, _site: Site, body: Bytes) -> ApiResult<Response> {
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

/// `POST /api/esports/link` (bearer the secret; form `sub`, `username`):
/// a manager on the site says the account `username` here is their player
/// `sub`'s, made before they signed in with the site. The site player's
/// link moves to it (an account it was linked to before stays, unlinked,
/// with its games). Refused (409) when that account is another site
/// player's. An empty `username` unlinks. Answers `{"username"}`.
pub async fn esports_link(State(ctx): State<Ctx>, _site: Site, body: Bytes) -> ApiResult<Response> {
    let f = form(&body);
    let sub = f.get("sub").filter(|s| !s.is_empty()).ok_or_else(|| ApiError::bad("sub is required"))?;
    let username = f.get("username").map(|u| u.trim()).unwrap_or_default();
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    if username.is_empty() {
        tx.execute(&ctx.db.sql("UPDATE chess.players SET esports_id = NULL WHERE esports_id = $1"), &[sub])
            .await?;
        tx.commit().await?;
        return Ok(Json(json!({ "username": null })).into_response());
    }
    let row = tx
        .query_opt(
            &ctx.db.sql(
                "SELECT p.id, p.esports_id, a.username FROM chess.accounts a JOIN chess.players p ON p.id = a.player_id
                  WHERE lower(a.username) = lower($1)",
            ),
            &[&username],
        )
        .await?
        .ok_or_else(|| ApiError::not_found("no account of that name here"))?;
    let (player, linked, name): (i64, Option<String>, String) = (row.get(0), row.get(1), row.get(2));
    if linked.as_deref().is_some_and(|l| l != sub) {
        return Err(ApiError::new(
            axum::http::StatusCode::CONFLICT,
            format!("{name} is another of the site's players' account"),
        ));
    }
    tx.execute(
        &ctx.db.sql("UPDATE chess.players SET esports_id = NULL WHERE esports_id = $1 AND id <> $2"),
        &[sub, &player],
    )
    .await?;
    tx.execute(&ctx.db.sql("UPDATE chess.players SET esports_id = $1 WHERE id = $2"), &[sub, &player])
        .await?;
    tx.commit().await?;
    Ok(Json(json!({ "username": name })).into_response())
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
