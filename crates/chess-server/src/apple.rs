//! Sign in with Apple, for the iOS app.
//!
//! The app asks Apple to sign the player in and sends what it gets back:
//! an identity token (a JWT Apple signs with RS256), a one-time
//! authorization code, and the nonce it gave Apple. Here the token is
//! checked against Apple's published keys (issuer, audience = one of our
//! client ids, expiry, and the nonce's SHA-256), then the code is traded
//! with Apple for a refresh token. That trade is what makes a sign-in
//! single-use (a code works once, for a few minutes) and gives the token
//! needed to revoke the link when the player unlinks or closes their
//! account, as Apple requires.
//!
//! The client secret for those calls is a short ES256 JWT signed with the
//! team's Sign in with Apple key (`CHESS_APPLE_KEY`).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use aws_lc_rs::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::Config;

/// Apple's keys are fetched again after this, or at once for an unknown key id.
const KEYS_LIFE: Duration = Duration::from_hours(1);
/// Clock skew allowed on the token's times.
const SKEW_SECS: i64 = 120;

/// What a checked identity token says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppleId {
    /// Apple's stable id for this person and our team.
    pub sub: String,
    /// Which of our client ids it was issued for.
    pub client_id: String,
    pub email: Option<String>,
    /// The email is Apple's private relay address.
    pub private_email: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct Jwk {
    kid: String,
    #[serde(default)]
    kty: String,
    n: String,
    e: String,
}

#[derive(Deserialize)]
struct Keys {
    keys: Vec<Jwk>,
}

pub struct Apple {
    key: SigningKey,
    key_id: String,
    team_id: String,
    client_ids: Vec<String>,
    /// Apple's sign-in service: `https://appleid.apple.com`, which is also
    /// the tokens' issuer and the client secret's audience.
    url: String,
    http: reqwest::Client,
    keys: Mutex<Option<(Arc<Vec<Jwk>>, Instant)>>,
}

impl Apple {
    /// From the config, when it names a key.
    pub fn from_config(cfg: &Config) -> Result<Option<Arc<Apple>>> {
        let (Some(key), Some(key_id)) = (&cfg.apple_key, &cfg.apple_key_id) else {
            return Ok(None);
        };
        let team_id = cfg
            .apple_team_id
            .clone()
            .or_else(|| cfg.apns_team_id.clone())
            .context("Sign in with Apple needs CHESS_APPLE_TEAM_ID (or CHESS_APNS_TEAM_ID)")?;
        let pem = if key.contains("BEGIN PRIVATE KEY") {
            key.clone()
        } else {
            std::fs::read_to_string(key).with_context(|| format!("reading CHESS_APPLE_KEY {key}"))?
        };
        let key = SigningKey::from_pkcs8_pem(&pem).map_err(|e| anyhow::anyhow!("CHESS_APPLE_KEY: {e}"))?;
        let mut client_ids: Vec<String> =
            cfg.apple_client_ids.iter().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect();
        if client_ids.is_empty() {
            client_ids.push(cfg.apns_topic.clone());
        }
        let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
        Ok(Some(Arc::new(Apple {
            key,
            key_id: key_id.clone(),
            team_id,
            client_ids,
            url: cfg.apple_url.trim_end_matches('/').to_owned(),
            http,
            keys: Mutex::new(None),
        })))
    }

    /// Check an identity token from the app and the nonce it sent Apple
    /// (raw here; Apple holds its SHA-256, hex).
    pub async fn verify(&self, token: &str, nonce: &str) -> Result<AppleId> {
        let (kid, signed, sig, claims) = split(token)?;
        let keys = self.keys(Some(&kid)).await?;
        let jwk = keys.iter().find(|k| k.kid == kid).context("Apple doesn't know the token's key")?;
        ensure!(jwk.kty.is_empty() || jwk.kty == "RSA", "Apple's key isn't RSA");
        let n = B64.decode(&jwk.n).context("Apple's key")?;
        let e = B64.decode(&jwk.e).context("Apple's key")?;
        RsaPublicKeyComponents { n: &n[..], e: &e[..] }
            .verify(&RSA_PKCS1_2048_8192_SHA256, signed.as_bytes(), &sig)
            .map_err(|_| anyhow::anyhow!("the identity token's signature is wrong"))?;
        check_claims(&claims, &self.url, &self.client_ids, nonce, chrono::Utc::now().timestamp())
    }

    async fn keys(&self, want: Option<&str>) -> Result<Arc<Vec<Jwk>>> {
        if let Ok(k) = self.keys.lock()
            && let Some((keys, at)) = k.as_ref()
            && at.elapsed() < KEYS_LIFE
            && want.is_none_or(|w| keys.iter().any(|k| k.kid == w))
        {
            return Ok(Arc::clone(keys));
        }
        let r = self.http.get(format!("{}/auth/keys", self.url)).send().await.context("Apple's keys")?;
        ensure!(r.status().is_success(), "Apple's keys: {}", r.status());
        let keys = Arc::new(r.json::<Keys>().await.context("Apple's keys")?.keys);
        if let Ok(mut k) = self.keys.lock() {
            *k = Some((Arc::clone(&keys), Instant::now()));
        }
        Ok(keys)
    }

    /// The client secret Apple wants with a code or a revocation.
    fn client_secret(&self, client_id: &str, now: i64) -> String {
        let header = B64.encode(json!({"alg": "ES256", "kid": self.key_id}).to_string());
        let claims = B64.encode(
            json!({"iss": self.team_id, "iat": now, "exp": now + 300, "aud": self.url, "sub": client_id})
                .to_string(),
        );
        let input = format!("{header}.{claims}");
        let sig: Signature = self.key.sign(input.as_bytes());
        format!("{input}.{}", B64.encode(sig.to_bytes()))
    }

    /// Trade the app's authorization code for a refresh token, checking it
    /// was issued to the same person. `Ok(None)`: Apple gave no refresh
    /// token (it doesn't always).
    pub async fn redeem(&self, code: &str, id: &AppleId) -> Result<Option<String>> {
        let secret = self.client_secret(&id.client_id, chrono::Utc::now().timestamp());
        let r = self
            .http
            .post(format!("{}/auth/token", self.url))
            .form(&[
                ("client_id", id.client_id.as_str()),
                ("client_secret", secret.as_str()),
                ("code", code),
                ("grant_type", "authorization_code"),
            ])
            .send()
            .await
            .context("Apple's token service")?;
        let status = r.status();
        let body: Value = r.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            bail!("Apple refused the sign-in code ({status} {})", body["error"].as_str().unwrap_or_default());
        }
        // Apple's answer comes straight from Apple, so its id token's
        // subject is trusted without checking the signature again.
        if let Some(t) = body["id_token"].as_str() {
            let (_, _, _, claims) = split(t)?;
            ensure!(claims["sub"].as_str() == Some(id.sub.as_str()), "the sign-in code is someone else's");
        }
        Ok(body["refresh_token"].as_str().map(str::to_owned))
    }

    /// Ask Apple to end the link (unlinking, or the account closed).
    pub async fn revoke(&self, client_id: &str, refresh_token: &str) -> Result<()> {
        let secret = self.client_secret(client_id, chrono::Utc::now().timestamp());
        let r = self
            .http
            .post(format!("{}/auth/revoke", self.url))
            .form(&[
                ("client_id", client_id),
                ("client_secret", secret.as_str()),
                ("token", refresh_token),
                ("token_type_hint", "refresh_token"),
            ])
            .send()
            .await
            .context("Apple's revoke service")?;
        ensure!(r.status().is_success(), "Apple's revoke service: {}", r.status());
        Ok(())
    }
}

/// A JWT's key id, signed part, signature and claims (not yet checked).
fn split(token: &str) -> Result<(String, String, Vec<u8>, Value)> {
    let mut parts = token.trim().split('.');
    let (Some(h), Some(c), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        bail!("the identity token isn't a JWT");
    };
    let header: Value = serde_json::from_slice(&B64.decode(h).context("the token's header")?)
        .context("the token's header")?;
    let claims: Value = serde_json::from_slice(&B64.decode(c).context("the token's claims")?)
        .context("the token's claims")?;
    let sig = B64.decode(s).context("the token's signature")?;
    let kid = header["kid"].as_str().unwrap_or_default().to_owned();
    if header["alg"].as_str() != Some("RS256") {
        bail!("the identity token isn't RS256");
    }
    Ok((kid, format!("{h}.{c}"), sig, claims))
}

/// SHA-256 of the raw nonce, hex: what the app gives Apple.
pub fn nonce_hash(raw: &str) -> String {
    hex::encode(Sha256::digest(raw.as_bytes()))
}

fn truthy(v: &Value) -> bool {
    v.as_bool().unwrap_or_else(|| v.as_str() == Some("true"))
}

fn check_claims(c: &Value, issuer: &str, client_ids: &[String], nonce: &str, now: i64) -> Result<AppleId> {
    ensure!(c["iss"].as_str() == Some(issuer), "the identity token isn't Apple's");
    let aud = c["aud"].as_str().unwrap_or_default();
    let client_id =
        client_ids.iter().find(|id| *id == aud).context("the identity token is for another app")?.clone();
    let exp = c["exp"].as_i64().context("the identity token has no expiry")?;
    ensure!(exp + SKEW_SECS > now, "the identity token has expired: sign in again");
    if let Some(iat) = c["iat"].as_i64() {
        ensure!(iat - SKEW_SECS <= now, "the identity token is from the future");
    }
    ensure!(!nonce.is_empty() && nonce.len() <= 200, "the nonce is missing");
    ensure!(c["nonce"].as_str() == Some(nonce_hash(nonce).as_str()), "the nonce doesn't match");
    let sub = c["sub"].as_str().filter(|s| !s.is_empty() && s.len() <= 200).context("no subject")?;
    let email = c["email"].as_str().filter(|e| e.contains('@')).map(str::to_owned);
    let verified = c.get("email_verified").is_none_or(truthy);
    Ok(AppleId {
        sub: sub.to_owned(),
        client_id,
        email: email.filter(|_| verified),
        private_email: c.get("is_private_email").is_some_and(truthy),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(nonce: &str) -> Value {
        json!({
            "iss": "https://appleid.apple.com", "aud": "org.example.app", "exp": 2_000, "iat": 1_000,
            "sub": "001234.abcd", "nonce": nonce_hash(nonce), "email": "x@privaterelay.appleid.com",
            "email_verified": "true", "is_private_email": "true",
        })
    }

    #[test]
    fn checks_the_claims() {
        let ids = vec!["org.example.app".to_owned()];
        let iss = "https://appleid.apple.com";
        let id = check_claims(&claims("n0nce"), iss, &ids, "n0nce", 1_500).unwrap();
        assert_eq!(id.sub, "001234.abcd");
        assert_eq!(id.client_id, "org.example.app");
        assert_eq!(id.email.as_deref(), Some("x@privaterelay.appleid.com"));
        assert!(id.private_email);
        assert!(check_claims(&claims("n0nce"), iss, &ids, "other", 1_500).is_err(), "nonce");
        assert!(check_claims(&claims("n0nce"), iss, &ids, "", 1_500).is_err(), "no nonce");
        assert!(check_claims(&claims("n0nce"), iss, &ids, "n0nce", 3_000).is_err(), "expired");
        assert!(check_claims(&claims("n0nce"), iss, &ids, "n0nce", 500).is_err(), "from the future");
        assert!(check_claims(&claims("n0nce"), iss, &["other".into()], "n0nce", 1_500).is_err(), "audience");
        assert!(check_claims(&claims("n0nce"), "https://evil.test", &ids, "n0nce", 1_500).is_err(), "issuer");
        let mut c = claims("n0nce");
        c["email_verified"] = json!(false);
        assert_eq!(check_claims(&c, iss, &ids, "n0nce", 1_500).unwrap().email, None, "unverified email");
    }

    #[test]
    fn nonce_is_sha256_hex() {
        assert_eq!(nonce_hash("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn splits_only_rs256_jwts() {
        let h = B64.encode(r#"{"alg":"RS256","kid":"k1"}"#);
        let c = B64.encode(r#"{"sub":"s"}"#);
        let (kid, signed, sig, claims) = split(&format!("{h}.{c}.AAEC")).unwrap();
        assert_eq!((kid.as_str(), sig), ("k1", vec![0, 1, 2]));
        assert_eq!(signed, format!("{h}.{c}"));
        assert_eq!(claims["sub"], "s");
        let none = B64.encode(r#"{"alg":"none"}"#);
        assert!(split(&format!("{none}.{c}.")).is_err());
        assert!(split("a.b").is_err());
    }
}
