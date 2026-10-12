//! OAuth 2.0 with PKCE, as Lichess does it: no client registration (the
//! `client_id` is any name the app picks), S256 code challenges, and
//! long-lived access tokens.
//!
//! The person approving is whoever is signed in to the site (`CHESS_SITE_NAME`) in
//! this browser (its `esl_session` cookie), so the consent page works when
//! the chess server shares the site's origin (served under /chess). Tokens
//! issued here are ordinary site API tokens, listed and revocable on the
//! site's Account page.

use std::collections::HashMap;
use std::fmt::Write as _;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use bytes::Bytes;
use chrono::{Duration, Utc};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::app::Ctx;
use crate::auth::{SCOPES, TOKEN_PREFIX, bearer, cookie, hash_key, random_token};
use crate::error::{ApiError, ApiResult};
use crate::http::{form, html_escape};
use crate::model;

const TOKEN_DAYS: i64 = 365;

#[derive(Debug, Clone, PartialEq, Eq)]
struct AuthRequest {
    client_id: String,
    redirect_uri: String,
    challenge: String,
    scopes: Vec<String>,
    state: Option<String>,
}

fn parse_request(q: &HashMap<String, String>) -> Result<AuthRequest, String> {
    let get = |k: &str| q.get(k).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    if get("response_type").as_deref() != Some("code") {
        return Err("response_type must be code".into());
    }
    if get("code_challenge_method").as_deref() != Some("S256") {
        return Err("code_challenge_method must be S256".into());
    }
    let challenge = get("code_challenge").ok_or("missing code_challenge")?;
    if !(43..=128).contains(&challenge.len()) {
        return Err("code_challenge has the wrong length".into());
    }
    let client_id = get("client_id").ok_or("missing client_id")?;
    if client_id.chars().count() > 80 {
        return Err("client_id is too long".into());
    }
    let redirect_uri = get("redirect_uri").ok_or("missing redirect_uri")?;
    let u = url::Url::parse(&redirect_uri).map_err(|_| "redirect_uri isn't a URL".to_owned())?;
    if !matches!(u.scheme(), "http" | "https") || u.fragment().is_some() {
        return Err("redirect_uri must be http(s) without a fragment".into());
    }
    let scopes: Vec<String> = get("scope")
        .unwrap_or_default()
        .split([' ', ','])
        .filter(|s| SCOPES.iter().any(|(k, _)| k == s))
        .map(str::to_owned)
        .collect();
    Ok(AuthRequest { client_id, redirect_uri, challenge, scopes, state: get("state") })
}

/// Ties the consent form to the session that saw it.
fn csrf(session: &str, r: &AuthRequest) -> String {
    hash_key(&format!("{session}|{}|{}|{}|{}", r.client_id, r.redirect_uri, r.challenge, r.scopes.join(" ")))
}

fn redirect_with(uri: &str, params: &[(&str, &str)]) -> Response {
    let mut u = url::Url::parse(uri).expect("checked when parsed");
    {
        let mut q = u.query_pairs_mut();
        for (k, v) in params {
            q.append_pair(k, v);
        }
    }
    Redirect::to(u.as_str()).into_response()
}

fn page(title: &str, body: &str) -> Response {
    Html(format!(
        "<!doctype html><html lang=en><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\">\
         <title>{t}</title><style>body{{font:16px/1.5 system-ui,sans-serif;max-width:32rem;margin:3rem auto;padding:0 1rem;\
         color:#e8eaed;background:#14161a}}a{{color:#8ab4f8}}button{{font:inherit;padding:.5rem 1.2rem;margin-right:.5rem;\
         border-radius:6px;border:1px solid #555;background:#2a2d33;color:inherit;cursor:pointer}}button.go{{background:#1a73e8;\
         border-color:#1a73e8}}code{{background:#2a2d33;padding:0 .3rem;border-radius:4px}}li{{margin:.2rem 0}}</style>\
         <h1>{t}</h1>{body}</html>",
        t = html_escape(title)
    ))
    .into_response()
}

/// Players may only grant scopes their roles allow.
async fn allowed_scopes(ctx: &Ctx, player: i64, wanted: &[String]) -> ApiResult<Vec<String>> {
    let c = ctx.db.conn().await?;
    let r =
        c.query_one(&ctx.db.sql("SELECT role, roles FROM site.players WHERE id = $1"), &[&player]).await?;
    let role: String = r.get(0);
    let roles: Vec<String> = r.get(1);
    let staff = role == "admin" || roles.iter().any(|x| x == "manager");
    Ok(wanted
        .iter()
        .filter(|s| staff || !matches!(s.as_str(), "chess:fairplay" | "bot:play"))
        .cloned()
        .collect())
}

/// `GET /oauth`: the consent page.
pub async fn authorize(
    State(ctx): State<Ctx>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let req = match parse_request(&q) {
        Ok(r) => r,
        Err(e) => {
            return Ok((
                StatusCode::BAD_REQUEST,
                page("Can't authorize", &format!("<p>{}</p>", html_escape(&e))),
            )
                .into_response());
        }
    };
    let Some(player) = ctx.auth.session(&h).await? else {
        let site = html_escape(&ctx.cfg.site_url);
        let name = html_escape(&ctx.cfg.site_name);
        return Ok(page(
            "Sign in first",
            &format!(
                "<p><code>{}</code> wants to use your {name} chess account.</p>\
                 <p><a href=\"{site}\">Sign in to {name}</a> in this browser, then come back to this page and reload it.</p>",
                html_escape(&req.client_id)
            ),
        ));
    };
    let session = cookie(&h, crate::auth::SESSION_COOKIE).unwrap_or_default();
    let c = ctx.db.conn().await?;
    let account = model::ensure_account(&ctx.db, &*c, player).await?;
    drop(c);
    let scopes = allowed_scopes(&ctx, player, &req.scopes).await?;
    let granted = AuthRequest { scopes: scopes.clone(), ..req.clone() };
    let list: String = if scopes.is_empty() {
        "<li>Read your public profile</li>".into()
    } else {
        let mut out = String::new();
        for (k, d) in scopes.iter().filter_map(|s| SCOPES.iter().find(|(k, _)| k == s)) {
            let _ = write!(out, "<li>{} <code>{k}</code></li>", html_escape(d));
        }
        out
    };
    let hidden = |k: &str, v: &str| format!("<input type=hidden name=\"{k}\" value=\"{}\">", html_escape(v));
    let base = ctx.cfg.base_path().unwrap_or_default();
    Ok(page(
        "Authorize access",
        &format!(
            "<p><code>{client}</code> wants to act as <b>{user}</b> on this chess server:</p><ul>{list}</ul>\
             <p>Redirect: <code>{redirect}</code></p>\
             <form method=post action=\"{base}/oauth\">{h1}{h2}{h3}{h4}{h5}{h6}\
             <button class=go name=decision value=approve>Authorize</button>\
             <button name=decision value=deny>Cancel</button></form>\
             <p>You can revoke this later on the site's Account page (API tokens).</p>",
            client = html_escape(&req.client_id),
            user = html_escape(&account.username),
            redirect = html_escape(&req.redirect_uri),
            h1 = hidden("client_id", &req.client_id),
            h2 = hidden("redirect_uri", &req.redirect_uri),
            h3 = hidden("code_challenge", &req.challenge),
            h4 = hidden("scope", &scopes.join(" ")),
            h5 = hidden("state", req.state.as_deref().unwrap_or("")),
            h6 = hidden("csrf", &csrf(&session, &granted)),
        ),
    ))
}

/// `POST /oauth`: the consent decision.
pub async fn decide(State(ctx): State<Ctx>, h: HeaderMap, body: Bytes) -> ApiResult<Response> {
    let mut f = form(&body);
    f.insert("response_type".into(), "code".into());
    f.insert("code_challenge_method".into(), "S256".into());
    let req = parse_request(&f).map_err(ApiError::bad)?;
    let player = ctx
        .auth
        .session(&h)
        .await?
        .ok_or_else(|| ApiError::forbidden(format!("sign in to {} first", ctx.cfg.site_name)))?;
    let session = cookie(&h, crate::auth::SESSION_COOKIE).unwrap_or_default();
    if f.get("csrf").map(String::as_str) != Some(csrf(&session, &req).as_str()) {
        return Err(ApiError::forbidden("this consent form has expired: start again"));
    }
    let state = req.state.clone().unwrap_or_default();
    if f.get("decision").map(String::as_str) != Some("approve") {
        return Ok(redirect_with(&req.redirect_uri, &[("error", "access_denied"), ("state", &state)]));
    }
    let scopes = allowed_scopes(&ctx, player, &req.scopes).await?;
    let code = format!("liu_{}", random_token(32));
    let c = ctx.db.conn().await?;
    c.execute(
        &ctx.db.sql(
            "INSERT INTO chess.oauth_codes (code_hash, player_id, client_id, redirect_uri, code_challenge, scopes, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, now() + interval '2 minutes')",
        ),
        &[&hash_key(&code), &player, &req.client_id, &req.redirect_uri, &req.challenge, &scopes],
    )
    .await?;
    Ok(redirect_with(&req.redirect_uri, &[("code", &code), ("state", &state)]))
}

fn oauth_error(error: &str, description: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": error, "error_description": description}))).into_response()
}

/// `POST /api/token`: trade a code (and its PKCE verifier) for a token.
pub async fn token(State(ctx): State<Ctx>, body: Bytes) -> ApiResult<Response> {
    let f = form(&body);
    let get = |k: &str| f.get(k).map_or("", String::as_str);
    if get("grant_type") != "authorization_code" {
        return Ok(oauth_error("unsupported_grant_type", "grant_type must be authorization_code"));
    }
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let row = tx
        .query_opt(
            &ctx.db.sql(
                "UPDATE chess.oauth_codes SET used_at = now()
                  WHERE code_hash = $1 AND used_at IS NULL AND expires_at > now()
                 RETURNING player_id, client_id, redirect_uri, code_challenge, scopes",
            ),
            &[&hash_key(get("code"))],
        )
        .await?;
    let Some(row) = row else {
        return Ok(oauth_error("invalid_grant", "unknown, used or expired code"));
    };
    let (player, client, redirect, challenge, scopes): (i64, String, String, String, Vec<String>) =
        (row.get(0), row.get(1), row.get(2), row.get(3), row.get(4));
    // A code is spent by any attempt, right or wrong.
    if get("client_id") != client || get("redirect_uri") != redirect {
        tx.commit().await?;
        return Ok(oauth_error("invalid_grant", "client_id or redirect_uri doesn't match"));
    }
    if B64.encode(Sha256::digest(get("code_verifier").as_bytes())) != challenge {
        tx.commit().await?;
        return Ok(oauth_error("invalid_grant", "code_verifier doesn't match the code_challenge"));
    }
    let token = format!("{TOKEN_PREFIX}{}", random_token(36));
    let prefix: String = token.chars().take(TOKEN_PREFIX.len() + 6).collect();
    let name: String = format!("OAuth: {client}").chars().take(80).collect();
    let expires = Utc::now() + Duration::days(TOKEN_DAYS);
    tx.execute(
        &ctx.db.sql(
            "INSERT INTO site.api_tokens (player_id, name, token_hash, prefix, scopes, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
        ),
        &[&player, &name, &hash_key(&token), &prefix, &scopes, &expires],
    )
    .await?;
    tx.commit().await?;
    Ok(Json(chess_proto::TokenResponse {
        token_type: "Bearer".into(),
        access_token: token,
        expires_in: TOKEN_DAYS * 86_400,
    })
    .into_response())
}

/// `DELETE /api/token`: revoke the token used for this request.
pub async fn revoke(State(ctx): State<Ctx>, h: HeaderMap) -> ApiResult<Response> {
    let t = bearer(&h).ok_or_else(ApiError::unauthorized)?;
    let c = ctx.db.conn().await?;
    c.execute(
        &ctx.db.sql(
            "UPDATE site.api_tokens SET revoked_at = now(), revoked_by = 'chess: DELETE /api/token'
              WHERE token_hash = $1 AND revoked_at IS NULL",
        ),
        &[&hash_key(t)],
    )
    .await?;
    ctx.auth.forget(t);
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /api/token/test`: which of these tokens work (comma-separated
/// body, up to 1000).
pub async fn test(State(ctx): State<Ctx>, body: Bytes) -> ApiResult<Response> {
    let text = String::from_utf8_lossy(&body);
    let mut out = Map::new();
    for t in text.split(',').map(str::trim).filter(|t| !t.is_empty()).take(1000) {
        let v = match ctx.auth.token(t).await? {
            Some(id) => {
                let c = ctx.db.conn().await?;
                let user = model::ensure_account(&ctx.db, &*c, id.player).await?.id();
                json!({"userId": user, "scopes": id.scopes.join(","), "expires": id.expires_ms})
            }
            None => Value::Null,
        };
        out.insert(t.to_owned(), v);
    }
    let mut r = Json(Value::Object(out)).into_response();
    r.headers_mut().insert(header::CACHE_CONTROL, "no-store".parse().expect("static header"));
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    fn good() -> Vec<(&'static str, &'static str)> {
        vec![
            ("response_type", "code"),
            ("client_id", "esports-lab"),
            ("redirect_uri", "https://training.example/api/me/lichess/callback"),
            ("code_challenge_method", "S256"),
            ("code_challenge", "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"),
            ("scope", "challenge:write board:play email:read study:read"),
            ("state", "xyz"),
        ]
    }

    #[test]
    fn parses_a_lichess_style_request() {
        let r = parse_request(&q(&good())).unwrap();
        assert_eq!(r.scopes, ["challenge:write", "board:play", "email:read"], "unknown scopes dropped");
        assert_eq!(r.state.as_deref(), Some("xyz"));
    }

    #[test]
    fn rejects_bad_requests() {
        for (k, v) in [
            ("response_type", "token"),
            ("code_challenge_method", "plain"),
            ("code_challenge", "short"),
            ("redirect_uri", "javascript:alert(1)"),
            ("redirect_uri", "https://x.example/cb#frag"),
        ] {
            let mut p = good();
            p.retain(|(kk, _)| *kk != k);
            p.push((k, v));
            assert!(parse_request(&q(&p)).is_err(), "{k}={v}");
        }
    }

    #[test]
    fn csrf_binds_the_session_and_request() {
        let r = parse_request(&q(&good())).unwrap();
        assert_ne!(csrf("a", &r), csrf("b", &r));
        let mut r2 = r.clone();
        r2.redirect_uri = "https://evil.example/".into();
        assert_ne!(csrf("a", &r), csrf("a", &r2));
    }

    #[test]
    fn pkce_matches_rfc_7636() {
        let v = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(B64.encode(Sha256::digest(v.as_bytes())), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }
}
