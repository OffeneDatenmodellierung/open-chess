//! Who is calling: the site's own API tokens (bearer) and session cookies.
//!
//! Tokens live in the site's `api_tokens` table, hashed with SHA-256 like
//! the site does, with Lichess's scope names (`board:play`, `bot:play`,
//! `challenge:read`, `challenge:write`, `email:read`) plus
//! `chess:fairplay` for reviewers. People make and revoke them on the
//! site's Account page, or through this server's OAuth flow. A short cache
//! keeps a busy game from hitting the database for every move; a revoked
//! token stops working within [`CACHE_TTL`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::http::{HeaderMap, header};
use sha2::{Digest, Sha256};

use crate::db::Db;

pub const TOKEN_PREFIX: &str = "esl_";
pub const SESSION_COOKIE: &str = "esl_session";
pub const CACHE_TTL: Duration = Duration::from_secs(15);
const NEGATIVE_TTL: Duration = Duration::from_secs(5);

/// Scopes this server understands (Lichess's names, and one of ours).
pub const SCOPES: &[(&str, &str)] = &[
    ("board:play", "Play games with the Board API"),
    ("bot:play", "Play games with the Bot API (bot accounts)"),
    ("challenge:read", "Read incoming challenges"),
    ("challenge:write", "Create, accept and decline challenges"),
    ("email:read", "Read your email address"),
    ("chess:fairplay", "Review fair-play reports (managers and admins)"),
];

pub fn hash_key(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

pub fn random_token(len: usize) -> String {
    use rand::distr::{Alphanumeric, SampleString};
    Alphanumeric.sample_string(&mut rand::rng(), len)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub player: i64,
    pub token_id: i64,
    pub scopes: Vec<String>,
    pub admin: bool,
    pub manager: bool,
    /// When the token expires, Unix ms.
    pub expires_ms: i64,
}

impl Identity {
    pub fn has(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }

    pub fn has_any(&self, scopes: &[&str]) -> bool {
        scopes.iter().any(|s| self.has(s))
    }
}

type Cache = Arc<Mutex<HashMap<String, (Option<Identity>, Instant)>>>;

#[derive(Clone)]
pub struct Auth {
    db: Db,
    cache: Cache,
    /// The host site's sign-in (its session cookie) is accepted.
    login: bool,
}

pub fn bearer(h: &HeaderMap) -> Option<&str> {
    h.get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

pub fn cookie(h: &HeaderMap, name: &str) -> Option<String> {
    h.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(
        |kv| {
            let (k, v) = kv.trim().split_once('=')?;
            (k == name).then(|| v.to_owned())
        },
    )
}

impl Auth {
    /// `login`: accept the host site's session cookie ([`Auth::session`]).
    pub fn new(db: Db, login: bool) -> Auth {
        Auth { db, cache: Arc::default(), login }
    }

    /// The identity behind a token, if it is valid now.
    pub async fn token(&self, token: &str) -> Result<Option<Identity>> {
        if !token.starts_with(TOKEN_PREFIX) {
            return Ok(None);
        }
        let key = hash_key(token);
        if let Ok(c) = self.cache.lock()
            && let Some((id, at)) = c.get(&key)
        {
            let ttl = if id.is_some() { CACHE_TTL } else { NEGATIVE_TTL };
            if at.elapsed() < ttl {
                return Ok(id.clone());
            }
        }
        let c = self.db.conn().await?;
        let r = c
            .query_opt(
                &self.db.sql(
                    "UPDATE site.api_tokens t
                        SET last_used_at = CASE WHEN t.last_used_at IS NULL OR t.last_used_at < now() - interval '1 minute'
                                                THEN now() ELSE t.last_used_at END
                       FROM site.players p
                      WHERE t.token_hash = $1 AND p.id = t.player_id AND t.revoked_at IS NULL
                        AND t.expires_at > now() AND p.disabled_at IS NULL
                     RETURNING t.id, t.player_id, t.scopes, p.role, p.roles,
                               (extract(epoch from t.expires_at) * 1000)::bigint",
                ),
                &[&key],
            )
            .await?;
        let id = r.map(|r| {
            let role: String = r.get(3);
            let roles: Vec<String> = r.get(4);
            Identity {
                token_id: r.get(0),
                player: r.get(1),
                scopes: r.get(2),
                admin: role == "admin",
                manager: roles.iter().any(|x| x == "manager"),
                expires_ms: r.get(5),
            }
        });
        if let Ok(mut c) = self.cache.lock() {
            if c.len() > 10_000 {
                c.clear();
            }
            c.insert(key, (id.clone(), Instant::now()));
        }
        Ok(id)
    }

    /// Forget a token at once (after revoking it here).
    pub fn forget(&self, token: &str) {
        if let Ok(mut c) = self.cache.lock() {
            c.remove(&hash_key(token));
        }
    }

    /// The player signed in to the site with this session cookie (works
    /// when the chess server shares the site's origin, e.g. under /chess).
    pub async fn session(&self, h: &HeaderMap) -> Result<Option<i64>> {
        if !self.login {
            return Ok(None);
        }
        let Some(token) = cookie(h, SESSION_COOKIE) else {
            return Ok(None);
        };
        let c = self.db.conn().await?;
        Ok(c.query_opt(
            &self.db.sql(
                "SELECT s.player_id FROM site.sessions s JOIN site.players p ON p.id = s.player_id
                  WHERE s.token_hash = $1 AND s.revoked_at IS NULL AND s.expires_at > now() AND p.disabled_at IS NULL",
            ),
            &[&hash_key(&token)],
        )
        .await?
        .map(|r| r.get(0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn hashes_like_the_site() {
        // sha256("abc")
        assert_eq!(hash_key("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let t = random_token(36);
        assert_eq!(t.len(), 36);
        assert!(t.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn reads_bearer_and_cookie() {
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer esl_abc"));
        h.insert(header::COOKIE, HeaderValue::from_static("a=1; esl_session=xyz; b=2"));
        assert_eq!(bearer(&h), Some("esl_abc"));
        assert_eq!(cookie(&h, SESSION_COOKIE).as_deref(), Some("xyz"));
        assert_eq!(cookie(&h, "none"), None);
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer "));
        assert_eq!(bearer(&h), None);
    }

    #[test]
    fn identity_scopes() {
        let id = Identity {
            player: 1,
            token_id: 2,
            scopes: vec!["board:play".into()],
            admin: false,
            manager: false,
            expires_ms: 0,
        };
        assert!(id.has("board:play"));
        assert!(id.has_any(&["bot:play", "board:play"]));
        assert!(!id.has("bot:play"));
    }
}
