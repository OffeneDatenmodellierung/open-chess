//! Running the server, for its admins (with a fresh passkey check): the
//! accounts here and their roles, and partners (other services' API access).
//!
//! * `GET  /api/admin/players?q=` — accounts, by name or email.
//! * `GET  /api/admin/players/{username}` — one, with its tokens.
//! * `POST /api/admin/players/{username}` — `{admin?, manager?, disabled?}`.
//! * `POST /api/admin/players/{username}/tokens/revoke` — `{id?}` (all without).
//! * `POST /api/admin/players/{username}/passkeys/reset` — remove its passkeys.
//! * `GET  /api/admin/partners`, `POST /api/admin/partners` — `{name, scopes}`
//!   makes one and answers its token (shown once).
//! * `POST /api/admin/partners/{id}/revoke`.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_postgres::Row;

use crate::app::{Ctx, Who};
use crate::auth::{PARTNER_PREFIX, PARTNER_SCOPES, hash_key, random_token, staff_roles};
use crate::error::{ApiError, ApiResult};

const PLAYER_SELECT: &str = "SELECT p.id, a.username, p.email, p.email_verified_at IS NOT NULL, p.role, p.roles,
        (extract(epoch from a.created_at) * 1000)::bigint, (extract(epoch from a.seen_at) * 1000)::bigint,
        (extract(epoch from p.disabled_at) * 1000)::bigint, p.esports_id IS NOT NULL, a.kind = 'bot',
        a.marked_at IS NOT NULL, p.password_hash IS NOT NULL,
        (SELECT count(*) FROM chess.passkeys k WHERE k.player_id = p.id),
        (SELECT count(*) FROM chess.api_tokens t WHERE t.player_id = p.id AND t.revoked_at IS NULL AND t.expires_at > now())
   FROM chess.players p JOIN chess.accounts a ON a.player_id = p.id";

fn player_json(r: &Row) -> Value {
    let (admin, manager) = staff_roles(&r.get::<_, String>(4), &r.get::<_, Vec<String>>(5));
    json!({
        "id": r.get::<_, i64>(0),
        "username": r.get::<_, String>(1),
        "email": r.get::<_, Option<String>>(2),
        "emailConfirmed": r.get::<_, bool>(3),
        "admin": admin,
        "manager": manager,
        "createdAt": r.get::<_, Option<i64>>(6),
        "seenAt": r.get::<_, Option<i64>>(7),
        "disabledAt": r.get::<_, Option<i64>>(8),
        "linked": r.get::<_, bool>(9),
        "bot": r.get::<_, bool>(10),
        "marked": r.get::<_, bool>(11),
        "password": r.get::<_, bool>(12),
        "passkeys": r.get::<_, i64>(13),
        "tokens": r.get::<_, i64>(14),
    })
}

#[derive(Deserialize)]
pub struct Search {
    q: Option<String>,
    /// "staff": admins and managers only.
    only: Option<String>,
}

pub async fn players(State(ctx): State<Ctx>, who: Who, Query(s): Query<Search>) -> ApiResult<Response> {
    who.admin()?;
    let q = s.q.unwrap_or_default().trim().to_lowercase();
    let like = format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    let staff = s.only.as_deref() == Some("staff");
    let c = ctx.db.conn().await?;
    let rows = c
        .query(
            &ctx.db.sql(&format!(
                "{PLAYER_SELECT}
                  WHERE ($1 = '%%' OR lower(a.username) LIKE $1 OR lower(coalesce(p.email, '')) LIKE $1)
                    AND (NOT $2 OR p.role = 'admin' OR 'manager' = ANY(p.roles))
                  ORDER BY a.seen_at DESC NULLS LAST, lower(a.username) LIMIT 100"
            )),
            &[&like, &staff],
        )
        .await?;
    Ok(Json(json!({ "players": rows.iter().map(player_json).collect::<Vec<_>>() })).into_response())
}

async fn find(ctx: &Ctx, c: &impl tokio_postgres::GenericClient, username: &str) -> ApiResult<Row> {
    c.query_opt(&ctx.db.sql(&format!("{PLAYER_SELECT} WHERE lower(a.username) = lower($1)")), &[&username])
        .await?
        .ok_or_else(|| ApiError::not_found("player"))
}

pub async fn player(State(ctx): State<Ctx>, who: Who, Path(username): Path<String>) -> ApiResult<Response> {
    who.admin()?;
    let c = ctx.db.conn().await?;
    let r = find(&ctx, &*c, &username).await?;
    let id: i64 = r.get(0);
    let tokens = c
        .query(
            &ctx.db.sql(
                "SELECT id, name, prefix, scopes, (extract(epoch from expires_at) * 1000)::bigint,
                        (extract(epoch from last_used_at) * 1000)::bigint
                   FROM chess.api_tokens WHERE player_id = $1 AND revoked_at IS NULL AND expires_at > now()
                  ORDER BY created_at DESC",
            ),
            &[&id],
        )
        .await?;
    let mut v = player_json(&r);
    v["tokenList"] = tokens
        .iter()
        .map(|t| {
            json!({
                "id": t.get::<_, i64>(0),
                "name": t.get::<_, String>(1),
                "prefix": t.get::<_, String>(2),
                "scopes": t.get::<_, Vec<String>>(3),
                "expiresAt": t.get::<_, i64>(4),
                "lastUsedAt": t.get::<_, Option<i64>>(5),
            })
        })
        .collect();
    Ok(Json(v).into_response())
}

#[derive(Deserialize)]
pub struct Change {
    admin: Option<bool>,
    manager: Option<bool>,
    disabled: Option<bool>,
}

pub async fn change(
    State(ctx): State<Ctx>,
    who: Who,
    Path(username): Path<String>,
    Json(ch): Json<Change>,
) -> ApiResult<Response> {
    let me = who.admin()?;
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let r = find(&ctx, &tx, &username).await?;
    let id: i64 = r.get(0);
    if id == me.player && (ch.admin == Some(false) || ch.disabled == Some(true)) {
        return Err(ApiError::bad("you can't take away your own admin rights or disable yourself"));
    }
    // A role goes to someone who signs in with a passkey: their rights then
    // need it, so a password or session alone never carries them.
    if (ch.admin == Some(true) || ch.manager == Some(true)) && r.get::<_, i64>(13) == 0 {
        return Err(ApiError::bad(format!(
            "{} needs a passkey first: they add one on their account page",
            r.get::<_, String>(1)
        )));
    }
    if let Some(a) = ch.admin {
        tx.execute(
            &ctx.db.sql(
                "UPDATE chess.players SET role = CASE WHEN $2 THEN 'admin' ELSE 'player' END WHERE id = $1",
            ),
            &[&id, &a],
        )
        .await?;
    }
    if let Some(m) = ch.manager {
        tx.execute(
            &ctx.db.sql(
                "UPDATE chess.players SET roles = CASE WHEN $2 THEN array_append(array_remove(roles, 'manager'), 'manager')
                                                       ELSE array_remove(roles, 'manager') END WHERE id = $1",
            ),
            &[&id, &m],
        )
        .await?;
    }
    if let Some(d) = ch.disabled {
        tx.execute(
            &ctx.db.sql(
                "UPDATE chess.players SET disabled_at = CASE WHEN $2 THEN coalesce(disabled_at, now()) END WHERE id = $1",
            ),
            &[&id, &d],
        )
        .await?;
        if d {
            tx.execute(
                &ctx.db.sql("UPDATE chess.sessions SET revoked_at = now() WHERE player_id = $1 AND revoked_at IS NULL"),
                &[&id],
            )
            .await?;
        }
    }
    tx.commit().await?;
    ctx.auth.forget_all();
    tracing::info!(
        player = id,
        by = me.player,
        admin = ?ch.admin,
        manager = ?ch.manager,
        disabled = ?ch.disabled,
        "account changed by an admin"
    );
    let c = ctx.db.conn().await?;
    Ok(Json(player_json(&find(&ctx, &*c, &username).await?)).into_response())
}

#[derive(Deserialize, Default)]
pub struct Which {
    id: Option<i64>,
}

pub async fn revoke_tokens(
    State(ctx): State<Ctx>,
    who: Who,
    Path(username): Path<String>,
    body: Option<Json<Which>>,
) -> ApiResult<Response> {
    let me = who.admin()?;
    let w = body.map(|b| b.0).unwrap_or_default();
    let c = ctx.db.conn().await?;
    let id: i64 = find(&ctx, &*c, &username).await?.get(0);
    let n = c
        .execute(
            &ctx.db.sql(
                "UPDATE chess.api_tokens SET revoked_at = now(), revoked_by = 'admin ' || $3
                  WHERE player_id = $1 AND revoked_at IS NULL AND ($2::bigint IS NULL OR id = $2)",
            ),
            &[&id, &w.id, &me.player.to_string()],
        )
        .await?;
    ctx.auth.forget_all();
    Ok(Json(json!({ "revoked": n })).into_response())
}

pub async fn reset_passkeys(
    State(ctx): State<Ctx>,
    who: Who,
    Path(username): Path<String>,
) -> ApiResult<Response> {
    let me = who.admin()?;
    let c = ctx.db.conn().await?;
    let r = find(&ctx, &*c, &username).await?;
    let id: i64 = r.get(0);
    if id == me.player {
        return Err(ApiError::bad("remove your own passkeys on your account page"));
    }
    let n = c.execute(&ctx.db.sql("DELETE FROM chess.passkeys WHERE player_id = $1"), &[&id]).await?;
    c.execute(&ctx.db.sql("UPDATE chess.sessions SET staff_verified_at = NULL WHERE player_id = $1"), &[&id])
        .await?;
    c.execute(
        &ctx.db.sql("UPDATE chess.api_tokens SET staff_verified_at = NULL WHERE player_id = $1"),
        &[&id],
    )
    .await?;
    ctx.auth.forget_all();
    Ok(Json(json!({ "removed": n, "password": r.get::<_, bool>(12), "linked": r.get::<_, bool>(9) }))
        .into_response())
}

// ─── partners ───

pub async fn partners(State(ctx): State<Ctx>, who: Who) -> ApiResult<Response> {
    who.admin()?;
    let c = ctx.db.conn().await?;
    let rows = c
        .query(
            &ctx.db.sql(
                "SELECT id, name, prefix, scopes, (extract(epoch from created_at) * 1000)::bigint,
                        (extract(epoch from last_used_at) * 1000)::bigint
                   FROM chess.partners WHERE revoked_at IS NULL ORDER BY lower(name)",
            ),
            &[],
        )
        .await?;
    let list: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<_, i64>(0),
                "name": r.get::<_, String>(1),
                "prefix": r.get::<_, String>(2),
                "scopes": r.get::<_, Vec<String>>(3),
                "createdAt": r.get::<_, i64>(4),
                "lastUsedAt": r.get::<_, Option<i64>>(5),
            })
        })
        .collect();
    let scopes: Vec<Value> = PARTNER_SCOPES.iter().map(|(k, d)| json!({"scope": k, "about": d})).collect();
    Ok(Json(json!({
        "partners": list,
        "scopes": scopes,
        "sharedSecret": ctx.cfg.site_secret().is_some(),
    }))
    .into_response())
}

#[derive(Deserialize)]
pub struct NewPartner {
    name: String,
    scopes: Vec<String>,
}

/// A partner and its token (`ocs_…`), shown once.
pub async fn make_partner(
    db: &crate::db::Db,
    c: &impl tokio_postgres::GenericClient,
    name: &str,
    scopes: &[String],
    by: Option<i64>,
) -> anyhow::Result<(i64, String)> {
    let name: String = name.trim().chars().take(60).collect();
    anyhow::ensure!(!name.is_empty(), "a partner needs a name");
    let scopes: Vec<String> =
        scopes.iter().filter(|s| PARTNER_SCOPES.iter().any(|(k, _)| k == s)).cloned().collect();
    anyhow::ensure!(
        !scopes.is_empty(),
        "choose what it may do: {}",
        PARTNER_SCOPES.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ")
    );
    let token = format!("{PARTNER_PREFIX}{}", random_token(40));
    let prefix: String = token.chars().take(PARTNER_PREFIX.len() + 6).collect();
    let id: i64 = c
        .query_one(
            &db.sql(
                "INSERT INTO chess.partners (name, token_hash, prefix, scopes, created_by)
                 VALUES ($1, $2, $3, $4, $5) RETURNING id",
            ),
            &[&name, &hash_key(&token), &prefix, &scopes, &by],
        )
        .await?
        .get(0);
    Ok((id, token))
}

pub async fn new_partner(State(ctx): State<Ctx>, who: Who, Json(p): Json<NewPartner>) -> ApiResult<Response> {
    let me = who.admin()?;
    let c = ctx.db.conn().await?;
    let (id, token) = make_partner(&ctx.db, &*c, &p.name, &p.scopes, Some(me.player))
        .await
        .map_err(|e| ApiError::bad(format!("{e:#}")))?;
    tracing::info!(partner = id, name = p.name, by = me.player, "partner made");
    Ok(Json(json!({ "id": id, "token": token })).into_response())
}

pub async fn revoke_partner(State(ctx): State<Ctx>, who: Who, Path(id): Path<i64>) -> ApiResult<Response> {
    let me = who.admin()?;
    let c = ctx.db.conn().await?;
    let n = c
        .execute(
            &ctx.db.sql("UPDATE chess.partners SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL"),
            &[&id],
        )
        .await?;
    tracing::info!(partner = id, by = me.player, "partner revoked");
    Ok(Json(json!({ "revoked": n })).into_response())
}
