//! Who is calling: this server's API tokens (bearer) and session cookies.
//!
//! Players, tokens and sessions are the server's own (`chess.players`,
//! `chess.api_tokens`, `chess.sessions`). Tokens are hashed with SHA-256,
//! with Lichess's scope names (`board:play`, `bot:play`, `challenge:read`,
//! `challenge:write`, `email:read`) plus `chess:fairplay` for reviewers.
//! People make and revoke them on the Account page ([`crate::http::web`]),
//! through the OAuth flow, or (for a host site's players) through the
//! site's server ([`crate::http::web::esports_token`]). New tokens start
//! `ocp_`; tokens imported from a host site keep its `esl_`. A short cache
//! keeps a busy game from hitting the database for every move; a revoked
//! token stops working within [`CACHE_TTL`].
//!
//! **Staff.** A player's roles here (admin; manager) are this server's
//! alone. Their rights need a passkey check as well, made in the session or
//! with the token in hand ([`crate::passkeys`]), which lasts
//! `CHESS_ADMIN_PASSKEY_HOURS`; until then the identity is only `pending`.
//!
//! **Partners.** Other services (a host site) call with a partner token
//! (`ocs_`), which an admin makes; its scopes say what it may do:
//! [`PARTNER_READ`] (games, players, the feed) and [`PARTNER_SITE`] (the host
//! site's endpoints: its players' tokens and links). The host site's shared
//! secret (`ESPORTS_SECRET`) still works as a partner with both, so a site
//! can move to a partner token at its own pace.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::http::{HeaderMap, header};
use sha2::{Digest, Sha256};

use crate::db::Db;

/// New tokens' prefix.
pub const TOKEN_PREFIX: &str = "ocp_";
/// Tokens imported from a host site (esports-lab) keep its prefix.
pub const SITE_TOKEN_PREFIX: &str = "esl_";
/// Partner tokens' prefix.
pub const PARTNER_PREFIX: &str = "ocs_";
/// Partner scope: read games, players and the feed.
pub const PARTNER_READ: &str = "read";
/// Partner scope: the host site's endpoints (`/api/esports/…`).
pub const PARTNER_SITE: &str = "site";
pub const PARTNER_SCOPES: &[(&str, &str)] = &[
    (PARTNER_READ, "Read games, players and the game feed"),
    (PARTNER_SITE, "Act as the host site: its players' tokens and account links"),
];
/// Signed in to this server (its own sign-in, or with the host site).
pub const SESSION_COOKIE: &str = "oc_session";
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

/// Compare secrets in constant time.
pub fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn random_token(len: usize) -> String {
    use rand::distr::{Alphanumeric, SampleString};
    Alphanumeric.sample_string(&mut rand::rng(), len)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    pub player: i64,
    pub token_id: i64,
    pub scopes: Vec<String>,
    /// An admin here, with a fresh passkey check.
    pub admin: bool,
    /// A manager here, with a fresh passkey check.
    pub manager: bool,
    /// An admin or manager here whose rights wait on a passkey check.
    pub pending: bool,
    /// When the token expires, Unix ms.
    pub expires_ms: i64,
}

/// A partner calling (another service), or the host site's shared secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partner {
    /// None: the host site's shared secret.
    pub id: Option<i64>,
    pub name: String,
    pub scopes: Vec<String>,
}

impl Partner {
    pub fn has(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }
}

/// An admin's or manager's roles, as stored.
pub fn staff_roles(role: &str, roles: &[String]) -> (bool, bool) {
    (role == "admin", roles.iter().any(|x| x == "manager"))
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
    staff_hours: i32,
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
    pub fn new(db: Db, staff_hours: i32) -> Auth {
        Auth { db, cache: Arc::default(), staff_hours: staff_hours.max(1) }
    }

    /// The identity behind a token, if it is valid now.
    pub async fn token(&self, token: &str) -> Result<Option<Identity>> {
        if !(token.starts_with(TOKEN_PREFIX) || token.starts_with(SITE_TOKEN_PREFIX)) {
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
                    "UPDATE chess.api_tokens t
                        SET last_used_at = CASE WHEN t.last_used_at IS NULL OR t.last_used_at < now() - interval '1 minute'
                                                THEN now() ELSE t.last_used_at END
                       FROM chess.players p
                      WHERE t.token_hash = $1 AND p.id = t.player_id AND t.revoked_at IS NULL
                        AND t.expires_at > now() AND p.disabled_at IS NULL
                     RETURNING t.id, t.player_id, t.scopes, p.role, p.roles,
                               (extract(epoch from t.expires_at) * 1000)::bigint,
                               coalesce(t.staff_verified_at > now() - make_interval(hours => $2), false)",
                ),
                &[&key, &self.staff_hours],
            )
            .await?;
        let id = r.map(|r| {
            let role: String = r.get(3);
            let roles: Vec<String> = r.get(4);
            let (admin, manager) = staff_roles(&role, &roles);
            let checked: bool = r.get(6);
            Identity {
                token_id: r.get(0),
                player: r.get(1),
                scopes: r.get(2),
                admin: admin && checked,
                manager: manager && checked,
                pending: (admin || manager) && !checked,
                expires_ms: r.get(5),
            }
        });
        // A pending staff check isn't cached: once done (perhaps through
        // another replica), it counts at once.
        if !id.as_ref().is_some_and(|i| i.pending)
            && let Ok(mut c) = self.cache.lock()
        {
            if c.len() > 10_000 {
                c.clear();
            }
            c.insert(key, (id.clone(), Instant::now()));
        }
        Ok(id)
    }

    /// A partner token's partner (or the host site's shared secret).
    pub async fn partner(&self, token: &str, site_secret: Option<&str>) -> Result<Option<Partner>> {
        if let Some(secret) = site_secret
            && same(token.as_bytes(), secret.as_bytes())
        {
            return Ok(Some(Partner {
                id: None,
                name: "host site".into(),
                scopes: vec![PARTNER_READ.into(), PARTNER_SITE.into()],
            }));
        }
        if !token.starts_with(PARTNER_PREFIX) {
            return Ok(None);
        }
        let c = self.db.conn().await?;
        Ok(c.query_opt(
            &self.db.sql(
                "UPDATE chess.partners
                    SET last_used_at = CASE WHEN last_used_at IS NULL OR last_used_at < now() - interval '1 minute'
                                            THEN now() ELSE last_used_at END
                  WHERE token_hash = $1 AND revoked_at IS NULL
                 RETURNING id, name, scopes",
            ),
            &[&hash_key(token)],
        )
        .await?
        .map(|r| Partner { id: Some(r.get(0)), name: r.get(1), scopes: r.get(2) }))
    }

    /// Forget every cached token (after changing someone's roles).
    pub fn forget_all(&self) {
        if let Ok(mut c) = self.cache.lock() {
            c.clear();
        }
    }

    /// Forget a token at once (after revoking it here).
    pub fn forget(&self, token: &str) {
        if let Ok(mut c) = self.cache.lock() {
            c.remove(&hash_key(token));
        }
    }

    /// Forget every cached token of a player's (their account closed).
    pub fn forget_player(&self, player: i64) {
        if let Ok(mut c) = self.cache.lock() {
            c.retain(|_, (id, _)| id.as_ref().is_none_or(|i| i.player != player));
        }
    }

    /// The player signed in with this session cookie.
    pub async fn session(&self, h: &HeaderMap) -> Result<Option<i64>> {
        let Some(token) = cookie(h, SESSION_COOKIE) else {
            return Ok(None);
        };
        let c = self.db.conn().await?;
        Ok(c.query_opt(
            &self.db.sql(
                "SELECT s.player_id FROM chess.sessions s JOIN chess.players p ON p.id = s.player_id
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
        h.insert(header::COOKIE, HeaderValue::from_static("a=1; oc_session=xyz; b=2"));
        assert_eq!(bearer(&h), Some("esl_abc"));
        assert_eq!(cookie(&h, SESSION_COOKIE).as_deref(), Some("xyz"));
        assert_eq!(cookie(&h, "none"), None);
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer "));
        assert_eq!(bearer(&h), None);
    }

    #[test]
    fn identity_scopes() {
        let id =
            Identity { player: 1, token_id: 2, scopes: vec!["board:play".into()], ..Identity::default() };
        assert!(id.has("board:play"));
        assert!(id.has_any(&["bot:play", "board:play"]));
        assert!(!id.has("bot:play"));
    }
}
