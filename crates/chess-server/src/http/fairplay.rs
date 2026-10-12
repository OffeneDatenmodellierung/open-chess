//! Fair-play review (open-chess endpoints, not Lichess's): the
//! queue of flagged games, a game's reports, a player's record, and
//! reviewers' verdicts. Needs a token with `chess:fairplay` held by a
//! manager or an admin.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::app::{Ctx, Who};
use crate::auth::Identity;
use crate::error::{ApiError, ApiResult};
use crate::http::ok;
use crate::model;

fn reviewer(who: &Who) -> ApiResult<&Identity> {
    let id = who.need(&["chess:fairplay"])?;
    if !(id.admin || id.manager) {
        return Err(ApiError::forbidden("fair-play review is for managers and admins"));
    }
    Ok(id)
}

const REPORT_COLS: &str =
    "r.game_id, r.side, a.username, r.score, r.flagged, r.moves, r.accuracy, r.acpl, r.top1, r.top3,
    r.time_cv, r.time_corr, r.reasons, r.engine, r.depth, (extract(epoch from r.created_at) * 1000)::bigint,
    r.app_disagree, r.app_picked, r.app_picked_reference, r.declared_assist";

fn report(r: &tokio_postgres::Row) -> Value {
    json!({
        "gameId": r.get::<_, String>(0),
        "side": r.get::<_, String>(1),
        "user": r.get::<_, String>(2),
        "score": r.get::<_, f32>(3),
        "flagged": r.get::<_, bool>(4),
        "moves": r.get::<_, i32>(5),
        "accuracy": r.get::<_, f32>(6),
        "acpl": r.get::<_, f32>(7),
        "top1": r.get::<_, f32>(8),
        "top3": r.get::<_, f32>(9),
        "timeCv": r.get::<_, Option<f32>>(10),
        "timeCorrelation": r.get::<_, Option<f32>>(11),
        "reasons": r.get::<_, Vec<String>>(12),
        "engine": r.get::<_, String>(13),
        "depth": r.get::<_, i32>(14),
        "createdAt": r.get::<_, i64>(15),
        // Where Stockfish and the training app's engine disagreed, which
        // the player chose.
        "appFingerprint": {
            "disagreements": r.get::<_, Option<i32>>(16),
            "pickedApp": r.get::<_, Option<i32>>(17),
            "pickedStockfish": r.get::<_, Option<i32>>(18),
        },
        // The app said its engine help was on during this game.
        "declaredAssist": r.get::<_, bool>(19),
    })
}

#[derive(Deserialize)]
pub struct QueueQuery {
    limit: Option<i64>,
    /// Include games that have a verdict already.
    all: Option<bool>,
}

/// Flagged reports, newest first (unreviewed only unless `all=true`).
pub async fn queue(State(ctx): State<Ctx>, who: Who, Query(q): Query<QueueQuery>) -> ApiResult<Response> {
    reviewer(&who)?;
    let c = ctx.db.conn().await?;
    let rows = c
        .query(
            &ctx.db.sql(&format!(
                "SELECT {REPORT_COLS} FROM chess.fairplay_reports r JOIN chess.accounts a ON a.player_id = r.player_id
                  WHERE r.flagged AND ($2 OR NOT EXISTS (
                        SELECT 1 FROM chess.fairplay_reviews v WHERE v.player_id = r.player_id AND v.game_id = r.game_id))
                  ORDER BY r.created_at DESC LIMIT $1"
            )),
            &[&q.limit.unwrap_or(50).clamp(1, 500), &q.all.unwrap_or(false)],
        )
        .await?;
    Ok(Json(rows.iter().map(report).collect::<Vec<_>>()).into_response())
}

/// Both sides' reports for one game, with the per-move evaluations.
pub async fn game(State(ctx): State<Ctx>, who: Who, Path(id): Path<String>) -> ApiResult<Response> {
    reviewer(&who)?;
    let c = ctx.db.conn().await?;
    let rows = c
        .query(
            &ctx.db.sql(&format!(
                "SELECT {REPORT_COLS}, r.evals FROM chess.fairplay_reports r JOIN chess.accounts a ON a.player_id = r.player_id
                  WHERE r.game_id = $1 ORDER BY r.side DESC"
            )),
            &[&id],
        )
        .await?;
    let analysis: Option<String> = c
        .query_opt(&ctx.db.sql("SELECT analysis FROM chess.games WHERE id = $1"), &[&id])
        .await?
        .map(|r| r.get(0));
    let Some(analysis) = analysis else { return Err(ApiError::not_found("game")) };
    let reports: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut v = report(r);
            v["evals"] = r.get::<_, Value>(20);
            v
        })
        .collect();
    Ok(Json(json!({"gameId": id, "analysis": analysis, "reports": reports})).into_response())
}

/// A player's screening record and the verdicts on them.
pub async fn player(State(ctx): State<Ctx>, who: Who, Path(username): Path<String>) -> ApiResult<Response> {
    reviewer(&who)?;
    let c = ctx.db.conn().await?;
    let a =
        model::account_by_name(&ctx.db, &*c, &username).await?.ok_or_else(|| ApiError::not_found("user"))?;
    let s = c
        .query_one(
            &ctx.db.sql(
                "SELECT count(*), count(*) FILTER (WHERE flagged), coalesce(avg(score), 0)::real,
                        coalesce(avg(score) FILTER (WHERE created_at > now() - interval '30 days'), 0)::real
                   FROM chess.fairplay_reports WHERE player_id = $1",
            ),
            &[&a.player_id],
        )
        .await?;
    let recent = c
        .query(
            &ctx.db.sql(&format!(
                "SELECT {REPORT_COLS} FROM chess.fairplay_reports r JOIN chess.accounts a ON a.player_id = r.player_id
                  WHERE r.player_id = $1 ORDER BY r.created_at DESC LIMIT 20"
            )),
            &[&a.player_id],
        )
        .await?;
    let reviews: Vec<Value> = c
        .query(
            &ctx.db.sql(
                "SELECT v.game_id, v.verdict, v.note, p.name, (extract(epoch from v.at) * 1000)::bigint
                   FROM chess.fairplay_reviews v LEFT JOIN site.players p ON p.id = v.reviewer_id
                  WHERE v.player_id = $1 ORDER BY v.at DESC LIMIT 50",
            ),
            &[&a.player_id],
        )
        .await?
        .iter()
        .map(|r| {
            json!({"gameId": r.get::<_, Option<String>>(0), "verdict": r.get::<_, String>(1),
                   "note": r.get::<_, Option<String>>(2), "reviewer": r.get::<_, Option<String>>(3),
                   "at": r.get::<_, i64>(4)})
        })
        .collect();
    Ok(Json(json!({
        "user": a.username,
        "bot": a.bot,
        "marked": a.marked,
        "screened": s.get::<_, i64>(0),
        "flagged": s.get::<_, i64>(1),
        "meanScore": s.get::<_, f32>(2),
        "meanScore30d": s.get::<_, f32>(3),
        "recent": recent.iter().map(report).collect::<Vec<_>>(),
        "reviews": reviews,
    }))
    .into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Review {
    username: String,
    game_id: Option<String>,
    /// clean | engine | unclear
    verdict: String,
    note: Option<String>,
}

/// Record a verdict. `engine` marks the account (shown as tosViolation;
/// it can't start games while `CHESS_BLOCK_MARKED` is on); `clean` lifts a
/// mark.
pub async fn review(State(ctx): State<Ctx>, who: Who, Json(r): Json<Review>) -> ApiResult<Response> {
    let me = reviewer(&who)?;
    if !matches!(r.verdict.as_str(), "clean" | "engine" | "unclear") {
        return Err(ApiError::bad("verdict is clean, engine or unclear"));
    }
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let a = model::account_by_name(&ctx.db, &tx, &r.username)
        .await?
        .ok_or_else(|| ApiError::not_found("user"))?;
    tx.execute(
        &ctx.db.sql(
            "INSERT INTO chess.fairplay_reviews (player_id, game_id, verdict, note, reviewer_id) VALUES ($1, $2, $3, $4, $5)",
        ),
        &[&a.player_id, &r.game_id, &r.verdict, &r.note, &me.player],
    )
    .await?;
    match r.verdict.as_str() {
        "engine" => {
            tx.execute(
                &ctx.db
                    .sql("UPDATE chess.accounts SET marked_at = now(), marked_by = $2 WHERE player_id = $1"),
                &[&a.player_id, &me.player],
            )
            .await?;
        }
        "clean" => {
            tx.execute(
                &ctx.db
                    .sql("UPDATE chess.accounts SET marked_at = NULL, marked_by = NULL WHERE player_id = $1"),
                &[&a.player_id],
            )
            .await?;
        }
        _ => {}
    }
    tx.commit().await?;
    Ok(ok())
}
