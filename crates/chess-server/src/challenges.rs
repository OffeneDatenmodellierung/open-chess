//! Challenges: create (optionally accepted at once with the opponent's
//! token), accept, decline, cancel, list, and lapse.

use std::collections::HashMap;

use anyhow::Result;
use chess_core::{Game, Side};
use chess_proto::ChallengeEvent;
use tokio_postgres::Transaction;

use crate::app::Ctx;
use crate::auth::Identity;
use crate::db::{Note, notify};
use crate::error::{ApiError, ApiResult};
use crate::model::{self, Account, ChallengeRow};
use crate::play;

pub const RULES: &[&str] = &["noAbort", "noRematch", "noGiveTime", "noClaimWin", "noEarlyDraw"];

/// Lichess's real-time clock limits: 0, ¼, ½, ¾, 1, 1½ minutes, then any
/// whole number of minutes up to three hours.
pub fn valid_limit(secs: u32) -> bool {
    matches!(secs, 0 | 15 | 30 | 45 | 60 | 90) || (secs.is_multiple_of(60) && secs <= 10_800)
}

/// A parsed `POST /api/challenge/{username}` body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub rated: bool,
    pub clock: Option<(u32, u32)>,
    pub color: String,
    pub variant: String,
    pub fen: Option<String>,
    pub rules: Vec<String>,
    pub keep_alive: bool,
    pub accept_by_token: Option<String>,
}

fn truthy(v: &str) -> bool {
    matches!(v, "true" | "1" | "on" | "yes")
}

impl Request {
    pub fn parse(form: &HashMap<String, String>) -> ApiResult<Request> {
        let get = |k: &str| form.get(k).map(|s| s.trim()).filter(|s| !s.is_empty());
        if get("days").is_some() {
            return Err(ApiError::bad("Correspondence games aren't supported on this server"));
        }
        let clock = match (get("clock.limit"), get("clock.increment")) {
            (None, None) => None,
            (Some(l), Some(i)) => {
                let l: u32 = l.parse().map_err(|_| ApiError::bad("clock.limit is a number of seconds"))?;
                let i: u32 =
                    i.parse().map_err(|_| ApiError::bad("clock.increment is a number of seconds"))?;
                if !valid_limit(l) {
                    return Err(ApiError::bad(
                        "clock.limit: 0, 15, 30, 45, 60, 90 or whole minutes up to 10800",
                    ));
                }
                if i > 60 {
                    return Err(ApiError::bad("clock.increment is at most 60"));
                }
                if l == 0 && i == 0 {
                    return Err(ApiError::bad("a clock needs time or an increment"));
                }
                Some((l, i))
            }
            _ => return Err(ApiError::bad("send both clock.limit and clock.increment, or neither")),
        };
        let color = get("color").unwrap_or("random").to_owned();
        if !matches!(color.as_str(), "white" | "black" | "random") {
            return Err(ApiError::bad("color is white, black or random"));
        }
        let fen = get("fen").map(str::to_owned);
        let variant = match get("variant").unwrap_or(if fen.is_some() { "fromPosition" } else { "standard" })
        {
            "standard" if fen.is_none() => "standard",
            "standard" | "fromPosition" => "fromPosition",
            other => {
                return Err(ApiError::bad(format!("The {other} variant isn't supported on this server")));
            }
        }
        .to_owned();
        if variant == "fromPosition" {
            let f = fen.as_deref().ok_or_else(|| ApiError::bad("fromPosition needs a fen"))?;
            Game::new(Some(f)).map_err(|e| ApiError::bad(e.to_string()))?;
        }
        let rated = get("rated").is_some_and(truthy);
        if rated && variant == "fromPosition" {
            return Err(ApiError::bad("Games from a position can't be rated"));
        }
        if rated && clock.is_none() {
            return Err(ApiError::bad("Unlimited games can't be rated"));
        }
        let rules: Vec<String> = get("rules")
            .map(|r| r.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default();
        if let Some(bad) = rules.iter().find(|r| !RULES.contains(&r.as_str())) {
            return Err(ApiError::bad(format!("Unknown rule {bad}")));
        }
        Ok(Request {
            rated,
            clock,
            color,
            variant,
            fen,
            rules,
            keep_alive: get("keepAliveStream").is_some_and(truthy),
            accept_by_token: get("acceptByToken").map(str::to_owned),
        })
    }

    fn final_color(&self) -> Side {
        match self.color.as_str() {
            "white" => Side::White,
            "black" => Side::Black,
            _ => {
                if rand::random::<bool>() {
                    Side::White
                } else {
                    Side::Black
                }
            }
        }
    }
}

async fn announce(ctx: &Ctx, tx: &Transaction<'_>, ch: &ChallengeRow, kind: &str, to: &[i64]) -> Result<()> {
    let ratings = model::challenge_ratings(&ctx.db, tx, ch).await?;
    for &p in to {
        let json = ch.json(ctx.cfg.public_url(), Some(p), ratings);
        let mut ev = serde_json::json!({"type": kind, "challenge": json});
        if kind == ChallengeEvent::CREATED {
            ev["compat"] = serde_json::json!({"bot": true, "board": true});
        }
        notify(tx, &ctx.db, &Note::User { player: p, event: ev }).await?;
    }
    Ok(())
}

fn can_play(ctx: &Ctx, a: &Account) -> ApiResult<()> {
    if ctx.cfg.block_marked && a.marked {
        return Err(ApiError::forbidden("This account is marked for a fair-play violation"));
    }
    Ok(())
}

/// Create a challenge. With `acceptByToken`, the opponent's token accepts
/// it in the same transaction and the game starts at once.
pub async fn create(
    ctx: &Ctx,
    who: &Identity,
    dest_name: &str,
    req: &Request,
) -> ApiResult<(ChallengeRow, (i32, i32))> {
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let me = model::ensure_account(&ctx.db, &tx, who.player).await?;
    let dest =
        model::account_by_name(&ctx.db, &tx, dest_name).await?.ok_or_else(|| ApiError::not_found("user"))?;
    if dest.player_id == me.player_id {
        return Err(ApiError::bad("You can't challenge yourself"));
    }
    can_play(ctx, &me)?;
    can_play(ctx, &dest)?;
    // Accepting on the opponent's behalf needs their token, with a scope
    // that lets them play.
    let acceptor = match &req.accept_by_token {
        Some(t) => {
            let id =
                ctx.auth.token(t).await?.ok_or_else(|| ApiError::bad("acceptByToken isn't a valid token"))?;
            if id.player != dest.player_id {
                return Err(ApiError::bad("acceptByToken belongs to someone else"));
            }
            if !id.has_any(&["challenge:write", "board:play", "bot:play"]) {
                return Err(ApiError::bad("acceptByToken lacks the challenge:write scope"));
            }
            true
        }
        None => false,
    };
    let id = model::new_id();
    let ttl_secs = if req.clock.is_some() { ctx.cfg.challenge_ttl().as_secs_f64() } else { 7.0 * 86_400.0 };
    tx.execute(
        &ctx.db.sql(
            "INSERT INTO chess.challenges (id, challenger_id, dest_id, variant, initial_fen, rated, clock_limit,
                    clock_increment, color, final_color, rules, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, now() + make_interval(secs => $12))",
        ),
        &[
            &id,
            &me.player_id,
            &dest.player_id,
            &req.variant,
            &req.fen,
            &req.rated,
            &req.clock.map(|c| c.0 as i32),
            &req.clock.map(|c| c.1 as i32),
            &req.color,
            &req.final_color().name(),
            &req.rules,
            &ttl_secs,
        ],
    )
    .await?;
    ctx.metrics.challenges.with_label_values(&["created"]).inc();
    let mut ch =
        model::challenge(&ctx.db, &tx, &id, true).await?.ok_or_else(|| ApiError::not_found("challenge"))?;
    if acceptor {
        accept_locked(ctx, &tx, &mut ch).await?;
    } else {
        announce(ctx, &tx, &ch, ChallengeEvent::CREATED, &[dest.player_id, me.player_id]).await?;
    }
    let ratings = model::challenge_ratings(&ctx.db, &tx, &ch).await?;
    tx.commit().await?;
    Ok((ch, ratings))
}

async fn accept_locked(ctx: &Ctx, tx: &Transaction<'_>, ch: &mut ChallengeRow) -> ApiResult<()> {
    tx.execute(&ctx.db.sql("UPDATE chess.challenges SET status = 'accepted' WHERE id = $1"), &[&ch.id])
        .await?;
    "accepted".clone_into(&mut ch.status);
    play::start_game(ctx, tx, ch).await?;
    ctx.metrics.challenges.with_label_values(&["accepted"]).inc();
    Ok(())
}

async fn lock_open(ctx: &Ctx, tx: &Transaction<'_>, id: &str) -> ApiResult<ChallengeRow> {
    let ch =
        model::challenge(&ctx.db, tx, id, true).await?.ok_or_else(|| ApiError::not_found("challenge"))?;
    let lapsed: bool = tx
        .query_one(&ctx.db.sql("SELECT expires_at < now() FROM chess.challenges WHERE id = $1"), &[&id])
        .await?
        .get(0);
    if ch.status != "created" || lapsed {
        return Err(ApiError::not_found("challenge"));
    }
    Ok(ch)
}

pub async fn accept(ctx: &Ctx, who: &Identity, id: &str) -> ApiResult<()> {
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let me = model::ensure_account(&ctx.db, &tx, who.player).await?;
    let mut ch = lock_open(ctx, &tx, id).await?;
    if ch.dest.player_id != me.player_id {
        return Err(ApiError::not_found("challenge"));
    }
    can_play(ctx, &me)?;
    can_play(ctx, &ch.challenger)?;
    accept_locked(ctx, &tx, &mut ch).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn decline(ctx: &Ctx, who: &Identity, id: &str, reason: Option<&str>) -> ApiResult<()> {
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let mut ch = lock_open(ctx, &tx, id).await?;
    if ch.dest.player_id != who.player {
        return Err(ApiError::not_found("challenge"));
    }
    let key = reason
        .map(str::to_lowercase)
        .filter(|r| chess_proto::DECLINE_REASONS.iter().any(|(k, _)| k == r))
        .unwrap_or_else(|| "generic".into());
    tx.execute(
        &ctx.db.sql("UPDATE chess.challenges SET status = 'declined', decline_reason = $2 WHERE id = $1"),
        &[&ch.id, &key],
    )
    .await?;
    "declined".clone_into(&mut ch.status);
    ch.decline_reason = Some(key);
    announce(ctx, &tx, &ch, ChallengeEvent::DECLINED, &[ch.challenger.player_id]).await?;
    ctx.metrics.challenges.with_label_values(&["declined"]).inc();
    tx.commit().await?;
    Ok(())
}

pub async fn cancel(ctx: &Ctx, who: &Identity, id: &str) -> ApiResult<()> {
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let mut ch = lock_open(ctx, &tx, id).await?;
    if ch.challenger.player_id != who.player {
        return Err(ApiError::not_found("challenge"));
    }
    tx.execute(&ctx.db.sql("UPDATE chess.challenges SET status = 'canceled' WHERE id = $1"), &[&ch.id])
        .await?;
    "canceled".clone_into(&mut ch.status);
    announce(ctx, &tx, &ch, ChallengeEvent::CANCELED, &[ch.dest.player_id, ch.challenger.player_id]).await?;
    ctx.metrics.challenges.with_label_values(&["canceled"]).inc();
    tx.commit().await?;
    Ok(())
}

/// Keep a challenge from lapsing while its challenger's stream is open.
pub async fn keep_alive(ctx: &Ctx, id: &str) -> Result<()> {
    let c = ctx.db.conn().await?;
    c.execute(
        &ctx.db.sql(
            "UPDATE chess.challenges SET expires_at = now() + make_interval(secs => $2)
              WHERE id = $1 AND status = 'created'",
        ),
        &[&id, &ctx.cfg.challenge_ttl().as_secs_f64()],
    )
    .await?;
    Ok(())
}

/// The current status of a challenge.
pub async fn status(ctx: &Ctx, id: &str) -> Result<Option<String>> {
    let c = ctx.db.conn().await?;
    Ok(c.query_opt(&ctx.db.sql("SELECT status FROM chess.challenges WHERE id = $1"), &[&id])
        .await?
        .map(|r| r.get(0)))
}

/// Pending challenges to and from a player: (in, out).
pub async fn list(
    ctx: &Ctx,
    player: i64,
) -> Result<(Vec<chess_proto::Challenge>, Vec<chess_proto::Challenge>)> {
    let c = ctx.db.conn().await?;
    let rows = c
        .query(
            &ctx.db.sql(&format!(
                "{} WHERE c.status = 'created' AND c.expires_at > now() AND (c.dest_id = $1 OR c.challenger_id = $1)
                 ORDER BY c.created_at",
                model::CHALLENGE_SELECT
            )),
            &[&player],
        )
        .await?;
    let (mut incoming, mut outgoing) = (Vec::new(), Vec::new());
    for r in rows {
        let ch = ChallengeRow::from_row(&r);
        let ratings = model::challenge_ratings(&ctx.db, &*c, &ch).await?;
        let j = ch.json(ctx.cfg.public_url(), Some(player), ratings);
        if ch.dest.player_id == player { incoming.push(j) } else { outgoing.push(j) }
    }
    Ok((incoming, outgoing))
}

/// Lapse challenges past their time (sweeper).
pub async fn expire(ctx: &Ctx) -> Result<usize> {
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let ids: Vec<String> = tx
        .query(
            &ctx.db.sql(
                "UPDATE chess.challenges SET status = 'canceled'
                  WHERE id IN (SELECT id FROM chess.challenges WHERE status = 'created' AND expires_at < now()
                               LIMIT 100 FOR UPDATE SKIP LOCKED)
                 RETURNING id",
            ),
            &[],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    for id in &ids {
        if let Some(ch) = model::challenge(&ctx.db, &tx, id, false).await? {
            announce(ctx, &tx, &ch, ChallengeEvent::CANCELED, &[ch.dest.player_id, ch.challenger.player_id])
                .await?;
            ctx.metrics.challenges.with_label_values(&["lapsed"]).inc();
        }
    }
    tx.commit().await?;
    Ok(ids.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn limits() {
        for ok in [0, 15, 30, 45, 60, 90, 120, 180, 600, 10_800] {
            assert!(valid_limit(ok), "{ok}");
        }
        for bad in [10, 75, 100, 10_860] {
            assert!(!valid_limit(bad), "{bad}");
        }
    }

    #[test]
    fn parses_the_sites_challenge() {
        // What a host site's Lichess backend sends.
        let r = Request::parse(&form(&[
            ("rated", "false"),
            ("clock.limit", "180"),
            ("clock.increment", "2"),
            ("color", "random"),
            ("variant", "standard"),
            ("keepAliveStream", "false"),
            ("acceptByToken", "esl_tok"),
        ]))
        .unwrap();
        assert_eq!(r.clock, Some((180, 2)));
        assert!(!r.rated && !r.keep_alive);
        assert_eq!(r.accept_by_token.as_deref(), Some("esl_tok"));
        assert_eq!(r.variant, "standard");
    }

    #[test]
    fn rejects_what_we_dont_play() {
        assert!(Request::parse(&form(&[("days", "3")])).is_err());
        assert!(Request::parse(&form(&[("variant", "chess960")])).is_err());
        assert!(Request::parse(&form(&[("clock.limit", "180")])).is_err());
        assert!(Request::parse(&form(&[("clock.limit", "100"), ("clock.increment", "0")])).is_err());
        assert!(Request::parse(&form(&[("clock.limit", "0"), ("clock.increment", "0")])).is_err());
        assert!(Request::parse(&form(&[("rated", "true")])).is_err(), "unlimited can't be rated");
        assert!(Request::parse(&form(&[("rules", "noAbort,bogus")])).is_err());
        assert!(Request::parse(&form(&[("fen", "nonsense")])).is_err());
    }

    #[test]
    fn from_position() {
        let r = Request::parse(&form(&[("fen", "4k3/8/8/8/8/8/8/R3K3 w - - 0 1")])).unwrap();
        assert_eq!(r.variant, "fromPosition");
        assert!(r.clock.is_none());
        let r = Request::parse(&form(&[("rules", "noAbort, noClaimWin")])).unwrap();
        assert_eq!(r.rules, ["noAbort", "noClaimWin"]);
    }
}
