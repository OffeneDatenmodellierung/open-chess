//! Passkeys (WebAuthn): registering them, signing in (or making an account)
//! with one instead of a password, and the passkey check that switches on
//! an admin's or manager's rights.
//!
//! The server is the relying party itself. Only what passkeys need: "none"-style trust
//! (attestation statements aren't checked: the key is trusted because the
//! signed-in person registered it), user verification required, and the
//! two algorithms every platform authenticator offers, ES256 (P-256) and
//! EdDSA (Ed25519). Signatures are checked with aws-lc-rs.

use anyhow::{Context, Result, bail, ensure};
use aws_lc_rs::signature::{ECDSA_P256_SHA256_ASN1, ED25519, UnparsedPublicKey};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ciborium::Value as Cbor;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use tokio_postgres::GenericClient;

use crate::db::Db;

pub const ES256: i64 = -7;
pub const EDDSA: i64 = -8;

/// Challenges are good for this long.
const CHALLENGE_SECS: i64 = 300;

/// This app as a WebAuthn relying party.
#[derive(Clone, Debug)]
pub struct Rp {
    /// The host name (`esports.example.com`, `localhost`).
    pub id: String,
    /// `https://esports.example.com` (or `http://localhost:8090`).
    pub origin: String,
}

impl Rp {
    pub fn from_public_url(url: &str) -> Result<Self> {
        let u = url::Url::parse(url).context("public_url isn't a URL")?;
        let id = u.host_str().context("public_url has no host")?.to_owned();
        Ok(Self { id, origin: u.origin().ascii_serialization() })
    }
}

/// The user handle passkeys carry: the player id, as 8 bytes.
pub fn user_handle(player: i64) -> Vec<u8> {
    player.to_be_bytes().to_vec()
}

pub fn player_of_handle(h: &[u8]) -> Option<i64> {
    Some(i64::from_be_bytes(h.try_into().ok()?))
}

/// `navigator.credentials.create` options (binary fields base64url).
pub fn creation_options(rp: &Rp, handle: &[u8], name: &str, challenge: &[u8], exclude: &[Vec<u8>]) -> Value {
    json!({
        "challenge": B64.encode(challenge),
        "rp": {"id": rp.id, "name": "open-chess"},
        "user": {"id": B64.encode(handle), "name": name, "displayName": name},
        "pubKeyCredParams": [{"type": "public-key", "alg": ES256}, {"type": "public-key", "alg": EDDSA}],
        "timeout": CHALLENGE_SECS * 1000,
        "attestation": "none",
        "authenticatorSelection": {"residentKey": "required", "requireResidentKey": true, "userVerification": "required"},
        "excludeCredentials": exclude.iter().map(|id| json!({"type": "public-key", "id": B64.encode(id)})).collect::<Vec<_>>(),
    })
}

/// `navigator.credentials.get` options. An empty `allow` lets the browser
/// offer any of this site's passkeys (signing in without typing a name).
pub fn request_options(rp: &Rp, challenge: &[u8], allow: &[Vec<u8>]) -> Value {
    json!({
        "challenge": B64.encode(challenge),
        "rpId": rp.id,
        "timeout": CHALLENGE_SECS * 1000,
        "userVerification": "required",
        "allowCredentials": allow.iter().map(|id| json!({"type": "public-key", "id": B64.encode(id)})).collect::<Vec<_>>(),
    })
}

/// A passkey just registered.
#[derive(Debug, PartialEq, Eq)]
pub struct NewCredential {
    pub id: Vec<u8>,
    pub alg: i64,
    /// ES256: the uncompressed point (65 bytes); EdDSA: 32 bytes.
    pub public_key: Vec<u8>,
    pub sign_count: u32,
}

fn check_client_data(rp: &Rp, kind: &str, challenge: &[u8], client_data_json: &[u8]) -> Result<()> {
    let c: Value = serde_json::from_slice(client_data_json).context("clientDataJSON")?;
    ensure!(c["type"] == kind, "not a {kind} response");
    let got = B64.decode(c["challenge"].as_str().unwrap_or_default()).context("challenge")?;
    ensure!(got == challenge, "challenge doesn't match");
    ensure!(c["origin"] == rp.origin.as_str(), "made for another site ({})", c["origin"]);
    ensure!(c["crossOrigin"] != true, "cross-origin use isn't allowed");
    Ok(())
}

/// Parsed authenticator data.
struct AuthData<'a> {
    flags: u8,
    sign_count: u32,
    /// Attested credential data (registration only).
    rest: &'a [u8],
}

const FLAG_UP: u8 = 0x01;
const FLAG_UV: u8 = 0x04;
const FLAG_AT: u8 = 0x40;

fn parse_auth_data<'a>(rp: &Rp, d: &'a [u8]) -> Result<AuthData<'a>> {
    ensure!(d.len() >= 37, "authenticator data too short");
    ensure!(d[..32] == Sha256::digest(rp.id.as_bytes())[..], "for another site");
    let flags = d[32];
    ensure!(flags & FLAG_UP != 0, "user not present");
    ensure!(flags & FLAG_UV != 0, "user not verified (PIN, fingerprint or face needed)");
    let sign_count = u32::from_be_bytes(d[33..37].try_into()?);
    Ok(AuthData { flags, sign_count, rest: &d[37..] })
}

fn cbor_int(v: &Cbor) -> Option<i64> {
    v.as_integer().and_then(|i| i64::try_from(i).ok())
}

fn cose_get(map: &[(Cbor, Cbor)], key: i64) -> Option<&Cbor> {
    map.iter().find(|(k, _)| cbor_int(k) == Some(key)).map(|(_, v)| v)
}

/// A COSE public key → (algorithm, raw key).
fn parse_cose_key(map: &[(Cbor, Cbor)]) -> Result<(i64, Vec<u8>)> {
    let kty = cose_get(map, 1).and_then(cbor_int).context("key type")?;
    let alg = cose_get(map, 3).and_then(cbor_int).context("algorithm")?;
    let bytes = |k| cose_get(map, k).and_then(Cbor::as_bytes).cloned();
    match (kty, alg) {
        (2, ES256) => {
            ensure!(cose_get(map, -1).and_then(cbor_int) == Some(1), "not a P-256 key");
            let (x, y) = (bytes(-2).context("x")?, bytes(-3).context("y")?);
            ensure!(x.len() == 32 && y.len() == 32, "bad P-256 key");
            Ok((ES256, [&[4u8][..], &x, &y].concat()))
        }
        (1, EDDSA) => {
            ensure!(cose_get(map, -1).and_then(cbor_int) == Some(6), "not an Ed25519 key");
            let x = bytes(-2).context("x")?;
            ensure!(x.len() == 32, "bad Ed25519 key");
            Ok((EDDSA, x))
        }
        _ => bail!("unsupported key (type {kty}, algorithm {alg}): use a passkey with ES256 or Ed25519"),
    }
}

/// Check a registration (`navigator.credentials.create`) response.
pub fn verify_registration(
    rp: &Rp,
    challenge: &[u8],
    client_data_json: &[u8],
    attestation_object: &[u8],
) -> Result<NewCredential> {
    check_client_data(rp, "webauthn.create", challenge, client_data_json)?;
    let att: Cbor = ciborium::from_reader(attestation_object).context("attestationObject")?;
    let att = att.as_map().context("attestationObject isn't a map")?;
    let auth_data = att
        .iter()
        .find(|(k, _)| k.as_text() == Some("authData"))
        .and_then(|(_, v)| v.as_bytes())
        .context("no authData")?;
    let a = parse_auth_data(rp, auth_data)?;
    ensure!(a.flags & FLAG_AT != 0, "no credential in the response");
    let r = a.rest;
    ensure!(r.len() >= 18, "credential data too short");
    let id_len = usize::from(u16::from_be_bytes([r[16], r[17]]));
    ensure!(r.len() >= 18 + id_len && id_len > 0 && id_len <= 1023, "bad credential id");
    let id = r[18..18 + id_len].to_vec();
    let key: Cbor = ciborium::from_reader(&r[18 + id_len..]).context("credential public key")?;
    let (alg, public_key) = parse_cose_key(key.as_map().context("public key isn't a map")?)?;
    Ok(NewCredential { id, alg, public_key, sign_count: a.sign_count })
}

/// Check a sign-in (`navigator.credentials.get`) response against a stored
/// passkey; returns the new signature counter.
/// What the browser returns from `navigator.credentials.get`.
pub struct Assertion<'a> {
    pub client_data_json: &'a [u8],
    pub authenticator_data: &'a [u8],
    pub signature: &'a [u8],
}

pub fn verify_assertion(
    rp: &Rp,
    challenge: &[u8],
    a: &Assertion<'_>,
    alg: i64,
    public_key: &[u8],
    stored_count: u32,
) -> Result<u32> {
    check_client_data(rp, "webauthn.get", challenge, a.client_data_json)?;
    let signed = [a.authenticator_data, &Sha256::digest(a.client_data_json)[..]].concat();
    let signature = a.signature;
    let a = parse_auth_data(rp, a.authenticator_data)?;
    let algorithm: &dyn aws_lc_rs::signature::VerificationAlgorithm = match alg {
        ES256 => &ECDSA_P256_SHA256_ASN1,
        EDDSA => &ED25519,
        _ => bail!("unsupported algorithm {alg}"),
    };
    UnparsedPublicKey::new(algorithm, public_key)
        .verify(&signed, signature)
        .map_err(|_| anyhow::anyhow!("passkey signature doesn't match"))?;
    // Synced passkeys keep 0; a counter that goes backwards means a copy.
    if (a.sign_count != 0 || stored_count != 0) && a.sign_count <= stored_count {
        bail!("this passkey's counter went backwards: it may have been copied");
    }
    Ok(a.sign_count)
}

// ─────────────────────────── storage ───────────────────────────

/// A stored passkey, for checking a sign-in.
pub struct StoredKey {
    pub id: i64,
    pub player: i64,
    pub handle: Vec<u8>,
    pub alg: i64,
    pub public_key: Vec<u8>,
    pub sign_count: u32,
}

/// What a challenge was made for.
pub const REGISTER: &str = "register";
pub const SIGNIN: &str = "signin";
pub const SIGNUP: &str = "signup";
pub const STAFF: &str = "staff";

/// A new challenge: (id, bytes). `data` rides along (a new account's name).
pub async fn new_challenge(
    db: &Db,
    c: &impl GenericClient,
    purpose: &str,
    player: Option<i64>,
    data: Option<&Value>,
) -> Result<(i64, Vec<u8>)> {
    let challenge: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
    c.execute(
        &db.sql("DELETE FROM chess.webauthn_challenges WHERE created_at < now() - interval '1 hour'"),
        &[],
    )
    .await?;
    let id: i64 = c
        .query_one(
            &db.sql(
                "INSERT INTO chess.webauthn_challenges (challenge, purpose, player_id, data)
                 VALUES ($1, $2, $3, $4) RETURNING id",
            ),
            &[&challenge, &purpose, &player, &data],
        )
        .await?
        .get(0);
    Ok((id, challenge))
}

/// A challenge taken (once, within five minutes): (bytes, player, data).
pub struct Taken {
    pub challenge: Vec<u8>,
    pub player: Option<i64>,
    pub data: Option<Value>,
}

pub async fn take_challenge(db: &Db, c: &impl GenericClient, id: i64, purpose: &str) -> Result<Taken> {
    let r = c
        .query_opt(
            &db.sql(&format!(
                "DELETE FROM chess.webauthn_challenges WHERE id = $1 AND purpose = $2
                   AND created_at > now() - interval '{CHALLENGE_SECS} seconds'
                 RETURNING challenge, player_id, data"
            )),
            &[&id, &purpose],
        )
        .await?
        .context("that passkey request has expired: try again")?;
    Ok(Taken { challenge: r.get(0), player: r.get(1), data: r.get(2) })
}

pub async fn add(
    db: &Db,
    c: &impl GenericClient,
    player: i64,
    handle: &[u8],
    name: &str,
    k: &NewCredential,
) -> Result<i64> {
    let r = c
        .query_one(
            &db.sql(
                "INSERT INTO chess.passkeys (player_id, user_handle, credential_id, public_key, alg, sign_count, name)
                 VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
            ),
            &[&player, &handle, &k.id, &k.public_key, &(k.alg as i32), &i64::from(k.sign_count), &name],
        )
        .await
        .map_err(|e| {
            if e.code() == Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION) {
                anyhow::anyhow!("that passkey is already registered")
            } else {
                anyhow::Error::new(e)
            }
        })?;
    Ok(r.get(0))
}

/// A player's passkeys' credential ids.
pub async fn ids(db: &Db, c: &impl GenericClient, player: i64) -> Result<Vec<Vec<u8>>> {
    Ok(c.query(&db.sql("SELECT credential_id FROM chess.passkeys WHERE player_id = $1"), &[&player])
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect())
}

pub async fn by_credential(db: &Db, c: &impl GenericClient, credential: &[u8]) -> Result<Option<StoredKey>> {
    Ok(c.query_opt(
        &db.sql(
            "SELECT k.id, k.player_id, k.user_handle, k.alg, k.public_key, k.sign_count
               FROM chess.passkeys k JOIN chess.players p ON p.id = k.player_id
              WHERE k.credential_id = $1 AND p.disabled_at IS NULL",
        ),
        &[&credential],
    )
    .await?
    .map(|r| StoredKey {
        id: r.get(0),
        player: r.get(1),
        handle: r.get(2),
        alg: i64::from(r.get::<_, i32>(3)),
        public_key: r.get(4),
        sign_count: u32::try_from(r.get::<_, i64>(5)).unwrap_or(u32::MAX),
    }))
}

pub async fn used(db: &Db, c: &impl GenericClient, id: i64, count: u32) -> Result<()> {
    c.execute(
        &db.sql("UPDATE chess.passkeys SET sign_count = $2, last_used_at = now() WHERE id = $1"),
        &[&id, &i64::from(count)],
    )
    .await?;
    Ok(())
}

/// What the browser sent back from `navigator.credentials.get`, base64url.
#[derive(Debug, serde::Deserialize)]
pub struct AssertionJson {
    pub challenge_id: i64,
    pub id: String,
    pub client_data_json: String,
    pub authenticator_data: String,
    pub signature: String,
    #[serde(default)]
    pub user_handle: Option<String>,
}

/// What the browser sent back from `navigator.credentials.create`.
#[derive(Debug, serde::Deserialize)]
pub struct AttestationJson {
    pub challenge_id: i64,
    pub client_data_json: String,
    pub attestation_object: String,
    #[serde(default)]
    pub name: Option<String>,
}

pub fn b64(what: &str, s: &str) -> Result<Vec<u8>> {
    B64.decode(s.trim()).with_context(|| format!("{what} isn't base64url"))
}

/// Check a sign-in against the challenge it answers: the passkey's id and
/// player. `want` limits it to one player's passkeys (the staff check).
pub async fn check_assertion(
    db: &Db,
    c: &impl GenericClient,
    rp: &Rp,
    purpose: &str,
    a: &AssertionJson,
    want: Option<i64>,
) -> Result<(i64, i64)> {
    let taken = take_challenge(db, c, a.challenge_id, purpose).await?;
    let cred = b64("id", &a.id)?;
    let key = by_credential(db, c, &cred).await?.context("that passkey isn't registered here")?;
    if let Some(p) = want.or(taken.player) {
        ensure!(key.player == p, "that passkey belongs to someone else");
    }
    if let Some(h) = &a.user_handle
        && !h.is_empty()
    {
        ensure!(b64("user_handle", h)? == key.handle, "that passkey belongs to someone else");
    }
    let count = verify_assertion(
        rp,
        &taken.challenge,
        &Assertion {
            client_data_json: &b64("client_data_json", &a.client_data_json)?,
            authenticator_data: &b64("authenticator_data", &a.authenticator_data)?,
            signature: &b64("signature", &a.signature)?,
        },
        key.alg,
        &key.public_key,
        key.sign_count,
    )?;
    used(db, c, key.id, count).await?;
    Ok((key.player, key.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::rand::SystemRandom;
    use aws_lc_rs::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, Ed25519KeyPair, KeyPair as _};

    fn rp() -> Rp {
        Rp::from_public_url("https://esports.example.com").unwrap()
    }

    fn client_data(kind: &str, challenge: &[u8], origin: &str) -> Vec<u8> {
        json!({"type": kind, "challenge": B64.encode(challenge), "origin": origin}).to_string().into_bytes()
    }

    fn auth_data(rp_id: &str, flags: u8, count: u32, attested: &[u8]) -> Vec<u8> {
        [&Sha256::digest(rp_id.as_bytes())[..], &[flags], &count.to_be_bytes(), attested].concat()
    }

    fn cbor(v: &Cbor) -> Vec<u8> {
        let mut out = Vec::new();
        ciborium::into_writer(v, &mut out).unwrap();
        out
    }

    fn i(n: i64) -> Cbor {
        Cbor::Integer(n.into())
    }

    /// A fake platform authenticator with a P-256 key.
    fn register_es256(rp: &Rp, challenge: &[u8]) -> (EcdsaKeyPair, Vec<u8>, Vec<u8>, Vec<u8>) {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let kp = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref()).unwrap();
        let point = kp.public_key().as_ref().to_vec(); // 0x04 || x || y
        let cose = Cbor::Map(vec![
            (i(1), i(2)),
            (i(3), i(ES256)),
            (i(-1), i(1)),
            (i(-2), Cbor::Bytes(point[1..33].to_vec())),
            (i(-3), Cbor::Bytes(point[33..].to_vec())),
        ]);
        let cred_id = b"cred-1".to_vec();
        let attested =
            [&[0u8; 16][..], &(cred_id.len() as u16).to_be_bytes(), &cred_id, &cbor(&cose)].concat();
        let ad = auth_data(&rp.id, FLAG_UP | FLAG_UV | FLAG_AT, 0, &attested);
        let att = cbor(&Cbor::Map(vec![
            (Cbor::Text("fmt".into()), Cbor::Text("none".into())),
            (Cbor::Text("attStmt".into()), Cbor::Map(vec![])),
            (Cbor::Text("authData".into()), Cbor::Bytes(ad)),
        ]));
        (kp, cred_id, client_data("webauthn.create", challenge, &rp.origin), att)
    }

    #[test]
    fn es256_register_and_sign_in() {
        let rp = rp();
        let (kp, cred_id, cd, att) = register_es256(&rp, b"reg-challenge");
        let k = verify_registration(&rp, b"reg-challenge", &cd, &att).unwrap();
        assert_eq!((k.id.clone(), k.alg, k.public_key.len()), (cred_id, ES256, 65));
        // Wrong challenge, wrong site.
        assert!(verify_registration(&rp, b"other", &cd, &att).is_err());
        let evil = Rp::from_public_url("https://evil.example").unwrap();
        assert!(verify_registration(&evil, b"reg-challenge", &cd, &att).is_err());

        let rng = SystemRandom::new();
        let ad = auth_data(&rp.id, FLAG_UP | FLAG_UV, 5, &[]);
        let cd = client_data("webauthn.get", b"login", &rp.origin);
        let sig = kp.sign(&rng, &[&ad[..], &Sha256::digest(&cd)[..]].concat()).unwrap();
        assert_eq!(
            verify_assertion(
                &rp,
                b"login",
                &Assertion { client_data_json: &cd, authenticator_data: &ad, signature: sig.as_ref() },
                ES256,
                &k.public_key,
                4
            )
            .unwrap(),
            5
        );
        // Replayed counter, tampered data, no user verification.
        assert!(
            verify_assertion(
                &rp,
                b"login",
                &Assertion { client_data_json: &cd, authenticator_data: &ad, signature: sig.as_ref() },
                ES256,
                &k.public_key,
                5
            )
            .is_err()
        );
        let mut bad = ad.clone();
        bad[36] ^= 1;
        assert!(
            verify_assertion(
                &rp,
                b"login",
                &Assertion { client_data_json: &cd, authenticator_data: &bad, signature: sig.as_ref() },
                ES256,
                &k.public_key,
                0
            )
            .is_err()
        );
        let up_only = auth_data(&rp.id, FLAG_UP, 6, &[]);
        let sig2 = kp.sign(&rng, &[&up_only[..], &Sha256::digest(&cd)[..]].concat()).unwrap();
        assert!(
            verify_assertion(
                &rp,
                b"login",
                &Assertion { client_data_json: &cd, authenticator_data: &up_only, signature: sig2.as_ref() },
                ES256,
                &k.public_key,
                0
            )
            .is_err()
        );
    }

    #[test]
    fn eddsa_sign_in_with_synced_counter() {
        let rp = Rp::from_public_url("http://localhost:8090").unwrap();
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let kp = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let cose = Cbor::Map(vec![
            (i(1), i(1)),
            (i(3), i(EDDSA)),
            (i(-1), i(6)),
            (i(-2), Cbor::Bytes(kp.public_key().as_ref().to_vec())),
        ]);
        let (alg, pk) = parse_cose_key(cose.as_map().unwrap()).unwrap();
        let ad = auth_data("localhost", FLAG_UP | FLAG_UV, 0, &[]);
        let cd = client_data("webauthn.get", b"c", "http://localhost:8090");
        let sig = kp.sign(&[&ad[..], &Sha256::digest(&cd)[..]].concat());
        // Counter 0 stays 0 for synced passkeys.
        assert_eq!(
            verify_assertion(
                &rp,
                b"c",
                &Assertion { client_data_json: &cd, authenticator_data: &ad, signature: sig.as_ref() },
                alg,
                &pk,
                0
            )
            .unwrap(),
            0
        );
        assert_eq!(player_of_handle(&user_handle(42)), Some(42));
    }
}
