//! Apple push notifications for the iOS app: "your move" in correspondence
//! games, a result to confirm. Sent over APNs' HTTP/2 API with a token
//! signed by the team's `.p8` key (ES256), which is kept for 50 minutes as
//! Apple asks. Off unless CHESS_APNS_KEY, _KEY_ID and _TEAM_ID are set.
//! A device APNs reports gone is forgotten.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::app::Ctx;
use crate::config::Config;

/// How long a signed provider token is used (Apple: between 20 and 60 minutes).
const TOKEN_LIFE: Duration = Duration::from_mins(50);

/// The signing key and the token made with it.
pub struct Apns {
    key: SigningKey,
    key_id: String,
    team_id: String,
    topic: String,
    http: reqwest::Client,
    token: Mutex<Option<(String, Instant)>>,
}

impl Apns {
    /// From the config, when it names a key.
    pub fn from_config(cfg: &Config) -> Result<Option<Arc<Apns>>> {
        let (Some(key), Some(key_id), Some(team_id)) = (&cfg.apns_key, &cfg.apns_key_id, &cfg.apns_team_id)
        else {
            return Ok(None);
        };
        let pem = if key.contains("BEGIN PRIVATE KEY") {
            key.clone()
        } else {
            std::fs::read_to_string(key).with_context(|| format!("reading CHESS_APNS_KEY {key}"))?
        };
        let key = SigningKey::from_pkcs8_pem(&pem).map_err(|e| anyhow::anyhow!("CHESS_APNS_KEY: {e}"))?;
        let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
        Ok(Some(Arc::new(Apns {
            key,
            key_id: key_id.clone(),
            team_id: team_id.clone(),
            topic: cfg.apns_topic.clone(),
            http,
            token: Mutex::new(None),
        })))
    }

    fn token(&self) -> String {
        let mut t = self.token.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((tok, at)) = t.as_ref()
            && at.elapsed() < TOKEN_LIFE
        {
            return tok.clone();
        }
        let tok = provider_token(&self.key, &self.key_id, &self.team_id, chrono::Utc::now().timestamp());
        *t = Some((tok.clone(), Instant::now()));
        tok
    }

    /// Send one alert. `Ok(false)`: the device is gone (forget it).
    async fn send(
        &self,
        device: &str,
        sandbox: bool,
        payload: &Value,
        collapse: Option<&str>,
    ) -> Result<bool> {
        let host = if sandbox { "api.sandbox.push.apple.com" } else { "api.push.apple.com" };
        let mut req = self
            .http
            .post(format!("https://{host}/3/device/{device}"))
            .bearer_auth(self.token())
            .header("apns-topic", &self.topic)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            .json(payload);
        if let Some(c) = collapse {
            req = req.header("apns-collapse-id", c);
        }
        let r = req.send().await?;
        let status = r.status();
        if status.is_success() {
            return Ok(true);
        }
        let body: Value = r.json().await.unwrap_or(Value::Null);
        let reason = body["reason"].as_str().unwrap_or_default();
        if status.as_u16() == 410
            || matches!(reason, "BadDeviceToken" | "Unregistered" | "DeviceTokenNotForTopic")
        {
            return Ok(false);
        }
        anyhow::bail!("APNs answered {status} {reason}")
    }
}

/// The JWT APNs wants: ES256 over `{alg, kid}.{iss, iat}`.
fn provider_token(key: &SigningKey, key_id: &str, team_id: &str, now: i64) -> String {
    let header = B64.encode(json!({"alg": "ES256", "kid": key_id}).to_string());
    let claims = B64.encode(json!({"iss": team_id, "iat": now}).to_string());
    let input = format!("{header}.{claims}");
    let sig: Signature = key.sign(input.as_bytes());
    format!("{input}.{}", B64.encode(sig.to_bytes()))
}

/// Tell `player`'s devices something, in the background (never fails the
/// caller). `collapse`: a later alert with the same id replaces this one.
pub fn notify(
    ctx: &Ctx,
    player: i64,
    title: String,
    body: String,
    game: Option<String>,
    collapse: Option<String>,
) {
    let Some(apns) = ctx.apns.clone() else { return };
    let ctx = ctx.clone();
    tokio::spawn(async move {
        if let Err(e) =
            deliver(&ctx, &apns, player, &title, &body, game.as_deref(), collapse.as_deref()).await
        {
            warn!(player, "push: {e:#}");
        }
    });
}

async fn deliver(
    ctx: &Ctx,
    apns: &Apns,
    player: i64,
    title: &str,
    body: &str,
    game: Option<&str>,
    collapse: Option<&str>,
) -> Result<()> {
    let devices: Vec<(String, bool)> = {
        let c = ctx.db.conn().await?;
        c.query(&ctx.db.sql("SELECT token, sandbox FROM chess.push_devices WHERE player_id = $1"), &[&player])
            .await?
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect()
    };
    let payload = json!({
        "aps": {"alert": {"title": title, "body": body}, "sound": "default", "thread-id": game.unwrap_or("open-chess")},
        "gameId": game,
    });
    for (token, sandbox) in devices {
        match apns.send(&token, sandbox, &payload, collapse).await {
            Ok(true) => debug!(player, "push sent"),
            Ok(false) => {
                let c = ctx.db.conn().await?;
                c.execute(&ctx.db.sql("DELETE FROM chess.push_devices WHERE token = $1"), &[&token]).await?;
            }
            Err(e) => warn!(player, "push: {e:#}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::VerifyingKey;
    use p256::ecdsa::signature::Verifier;

    #[test]
    fn signs_a_verifiable_provider_token() {
        let key = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let t = provider_token(&key, "ABC123DEFG", "TEAM456789", 1_700_000_000);
        let parts: Vec<&str> = t.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: Value = serde_json::from_slice(&B64.decode(parts[0]).unwrap()).unwrap();
        assert_eq!((header["alg"].as_str(), header["kid"].as_str()), (Some("ES256"), Some("ABC123DEFG")));
        let claims: Value = serde_json::from_slice(&B64.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(
            (claims["iss"].as_str(), claims["iat"].as_i64()),
            (Some("TEAM456789"), Some(1_700_000_000))
        );
        let sig = Signature::from_slice(&B64.decode(parts[2]).unwrap()).unwrap();
        let input = format!("{}.{}", parts[0], parts[1]);
        VerifyingKey::from(&key).verify(input.as_bytes(), &sig).unwrap();
    }
}
