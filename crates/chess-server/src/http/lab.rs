//! open-chess additions to the Lichess API, used by the chess
//! training app:
//!
//! - `POST /api/board/game/{id}/assist`: the app declares that its engine
//!   help was switched on or off during one of our games (ground truth for
//!   the fair-play model);
//! - `GET /api/esl/position?fen=`: the position dictionary — the deepest
//!   stored evaluation of a position, with win/draw/loss odds;
//! - `POST /api/esl/recognise`: read a board from a photo;
//! - `POST /api/esl/photos/{id}/confirm`: say what a photo kept for review
//!   showed.
//! - `GET /api/esl/tablebase?fen=`: the exact result of a position with up to
//!   seven pieces, from the tablebase server, kept once fetched.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use chess_core::Game;
use serde::Deserialize;
use serde_json::json;

use crate::app::{Ctx, Who};
use crate::error::{ApiError, ApiResult};
use crate::http::{form, ok};
use crate::{model, vision};

/// The dictionary key for a position: the FEN without the move counters,
/// and with an en-passant square only when an en-passant capture is
/// actually possible, so the same position is always one entry.
pub fn position_key(game: &Game) -> String {
    let fen = game.fen();
    let mut f: Vec<&str> = fen.split_whitespace().take(4).collect();
    if let Some(ep) = f.get(3).copied().filter(|e| *e != "-") {
        let board = game.board();
        let possible = game.legal_moves().iter().any(|m| {
            m.get(2..4) == Some(ep)
                && m.get(0..2)
                    .and_then(|s| s.parse::<cozy_chess::Square>().ok())
                    .and_then(|s| board.piece_on(s))
                    == Some(cozy_chess::Piece::Pawn)
        });
        if !possible {
            f[3] = "-";
        }
    }
    f.join(" ")
}

pub async fn assist(
    State(ctx): State<Ctx>,
    who: Who,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let me = who.need(&["board:play"])?.player;
    let f = form(&body);
    let on = match f.get("on").map(String::as_str) {
        Some("true" | "1" | "yes") => true,
        Some("false" | "0" | "no") => false,
        _ => return Err(ApiError::bad("say on=true or on=false")),
    };
    let c = ctx.db.conn().await?;
    let row = model::game(&ctx.db, &*c, &id).await?.ok_or_else(|| ApiError::not_found("game"))?;
    if row.white.player_id != me && row.black.player_id != me {
        return Err(ApiError::forbidden("not your game"));
    }
    c.execute(
        &ctx.db
            .sql("INSERT INTO chess.assist_marks (game_id, player_id, ply, assist) VALUES ($1, $2, $3, $4)"),
        &[&id, &me, &row.ply, &on],
    )
    .await?;
    Ok(ok())
}

#[derive(Deserialize)]
pub struct PositionQuery {
    fen: String,
}

pub async fn position(
    State(ctx): State<Ctx>,
    who: Who,
    Query(q): Query<PositionQuery>,
) -> ApiResult<Response> {
    let game = Game::new(Some(q.fen.trim())).map_err(|e| ApiError::bad(format!("bad FEN: {e:?}")))?;
    let c = ctx.db.conn().await?;
    // A stored evaluation is engine help: not while you're playing here.
    if !model::ongoing_games(&ctx.db, &*c, who.0.player).await?.is_empty() {
        return Err(ApiError::forbidden("position lookups are off while you have a game in progress"));
    }
    let key = position_key(&game);
    let row = c
        .query_opt(
            &ctx.db.sql("SELECT best, score_cp, depth, engine, seen FROM chess.positions WHERE key = $1"),
            &[&key],
        )
        .await?;
    let Some(r) = row else {
        return Ok(Json(json!({ "key": key, "known": false })).into_response());
    };
    let score: i32 = r.get(1);
    // Stored from the side to move's view; odds are given from White's.
    let white_cp = if game.turn() == chess_core::Side::White { score } else { -score };
    let phase = chess_analysis::eval::phase(game.board());
    let wdl = chess_analysis::Wdl::from_cp(white_cp, chess_analysis::wdl::Params::for_phase(phase));
    Ok(Json(json!({
        "key": key,
        "known": true,
        "best": r.get::<_, Option<String>>(0),
        "scoreCp": score,
        "depth": r.get::<_, i32>(2),
        "engine": r.get::<_, String>(3),
        "seen": r.get::<_, i32>(4),
        "wdl": wdl,
    }))
    .into_response())
}

/// `GET /api/esl/tablebase?fen=`: Lichess's tablebase answer (category,
/// distance to zeroing and to mate, and every move's result), cached.
/// Like the position dictionary, refused while you're playing here.
pub async fn tablebase(
    State(ctx): State<Ctx>,
    who: Who,
    Query(q): Query<PositionQuery>,
) -> ApiResult<Response> {
    let game = Game::new(Some(q.fen.trim())).map_err(|e| ApiError::bad(format!("bad FEN: {e:?}")))?;
    let pieces = game.board().occupied().len();
    if pieces > 7 {
        return Err(ApiError::bad("tablebases cover positions with seven pieces or fewer"));
    }
    let base = ctx.cfg.tablebase_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(ApiError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "tablebases are off on this server",
        ));
    }
    let c = ctx.db.conn().await?;
    if !model::ongoing_games(&ctx.db, &*c, who.0.player).await?.is_empty() {
        return Err(ApiError::forbidden("tablebase lookups are off while you have a game in progress"));
    }
    let key = position_key(&game);
    if let Some(r) =
        c.query_opt(&ctx.db.sql("SELECT answer FROM chess.tablebase WHERE key = $1"), &[&key]).await?
    {
        let mut v: serde_json::Value = r.get(0);
        v["cached"] = true.into();
        return Ok(Json(v).into_response());
    }
    // The tablebase wants a full FEN; the key's counters don't matter.
    let fen = format!("{key} 0 1");
    let resp = crate::vision::http()
        .get(format!("{base}/standard"))
        .query(&[("fen", fen.as_str())])
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| {
            ApiError::new(axum::http::StatusCode::BAD_GATEWAY, format!("tablebase unreachable: {e}"))
        })?;
    if !resp.status().is_success() {
        return Err(ApiError::new(
            axum::http::StatusCode::BAD_GATEWAY,
            format!("tablebase answered {}", resp.status()),
        ));
    }
    let answer: serde_json::Value = resp.json().await.map_err(|e| {
        ApiError::new(axum::http::StatusCode::BAD_GATEWAY, format!("tablebase answer unreadable: {e}"))
    })?;
    c.execute(
        &ctx.db.sql("INSERT INTO chess.tablebase (key, answer) VALUES ($1, $2) ON CONFLICT (key) DO NOTHING"),
        &[&key, &answer],
    )
    .await?;
    Ok(Json(answer).into_response())
}

#[derive(Deserialize)]
pub struct RecogniseBody {
    /// A `data:image/...;base64,` URL or bare base64.
    image: String,
}

/// Read a board from a photo: with our board reader if there is one,
/// asking the vision language model only when it isn't sure (or there is
/// no reader). A photo that wasn't read for sure is kept for the Review
/// page (`photoId` in the answer) unless CHESS_VISION_REVIEW is off.
pub async fn recognise(
    State(ctx): State<Ctx>,
    who: Who,
    Json(b): Json<RecogniseBody>,
) -> ApiResult<Response> {
    let me = who.0.player;
    if ctx.reader.is_none() && !vision::enabled(&ctx.cfg) {
        return Err(ApiError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "photo reading isn't set up on this server (CHESS_VISION_LOCAL_MODEL or CHESS_VISION_ENDPOINT)",
        ));
    }
    if b.image.len() > 8 * 1024 * 1024 {
        return Err(ApiError::bad("the photo is too large: send it at 1600 pixels or less"));
    }
    let (bytes, mime) = vision::decode_image(&b.image).map_err(|e| ApiError::bad(format!("{e:#}")))?;

    // Our reader first.
    let local = match ctx.reader.clone() {
        Some(reader) => {
            let photo = bytes.clone();
            let started = std::time::Instant::now();
            let r = tokio::task::spawn_blocking(move || reader.read_encoded(&photo))
                .await
                .map_err(|e| ApiError::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
                .map_err(|e| ApiError::new(axum::http::StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
            tracing::debug!(
                ms = started.elapsed().as_millis() as u64,
                confidence = r.confidence,
                "photo read"
            );
            Some(r)
        }
        None => None,
    };
    let model_name = ctx.reader.as_ref().map(|r| r.card().name.clone()).filter(|n| !n.is_empty());
    if let Some(r) = &local
        && r.is_sure(ctx.cfg.vision_min_confidence)
    {
        return Ok(Json(local_answer(r, model_name.as_deref(), None)).into_response());
    }

    // Not sure (or no reader): the vision language model's opinion.
    let llm =
        if vision::enabled(&ctx.cfg) { Some(vision::read_board(&ctx.cfg, &b.image).await) } else { None };
    let reason = match &local {
        None => "no_model",
        Some(r) if !r.plausible => "implausible",
        Some(_) => "unsure",
    };
    let photo_id = if ctx.cfg.vision_review {
        keep_photo(&ctx, me, &bytes, &mime, reason, local.as_ref(), model_name.as_deref(), llm.as_ref())
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("keeping a photo for review: {e:#}");
                None
            })
    } else {
        None
    };
    let llm_answer = |l: &vision::Reading| {
        let mut v = llm_json(l, &ctx.cfg.vision_model);
        v["source"] = json!("llm");
        v["photoId"] = json!(photo_id);
        v["review"] = json!(photo_id.is_some());
        v
    };
    match (local, llm) {
        // The reader's reading can't be a game position but the model's
        // can: give the model's.
        (Some(r), Some(Ok(l))) if !r.plausible => Ok(Json(llm_answer(&l)).into_response()),
        // Otherwise the reader's (if unsure), with the model's alongside.
        (Some(r), llm) => {
            let mut v = local_answer(&r, model_name.as_deref(), photo_id);
            if let Some(Ok(l)) = llm {
                v["llm"] = llm_json(&l, &ctx.cfg.vision_model);
            }
            Ok(Json(v).into_response())
        }
        (None, Some(Ok(l))) => Ok(Json(llm_answer(&l)).into_response()),
        (None, Some(Err(e))) => {
            Err(ApiError::new(axum::http::StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")))
        }
        (None, None) => {
            Err(ApiError::new(axum::http::StatusCode::UNPROCESSABLE_ENTITY, "the photo couldn't be read"))
        }
    }
}

fn local_answer(
    r: &chess_vision::BoardReading,
    model: Option<&str>,
    photo_id: Option<i64>,
) -> serde_json::Value {
    json!({
        "source": "model",
        "placement": r.placement,
        // The reader always reads from White's side; it doesn't say where
        // the camera was.
        "whiteAtBottom": null,
        "sideToMove": null,
        "confidence": r.confidence,
        "unsureSquares": r.unsure_squares,
        "plausible": r.plausible,
        "squares": r.squares,
        "model": model,
        "photoId": photo_id,
        "review": photo_id.is_some(),
    })
}

fn llm_json(l: &vision::Reading, model: &str) -> serde_json::Value {
    json!({
        "placement": l.placement,
        "whiteAtBottom": l.white_at_bottom,
        "sideToMove": l.side_to_move,
        "confidence": l.confidence,
        "unsureSquares": l.unsure_squares,
        "model": model,
    })
}

/// Keep a photo for the Review page; its id, or `None` when the review
/// queue is full.
#[allow(clippy::too_many_arguments)]
async fn keep_photo(
    ctx: &Ctx,
    player: i64,
    bytes: &[u8],
    mime: &str,
    reason: &str,
    local: Option<&chess_vision::BoardReading>,
    model: Option<&str>,
    llm: Option<&anyhow::Result<vision::Reading>>,
) -> anyhow::Result<Option<i64>> {
    let c = ctx.db.conn().await?;
    let waiting: i64 = c
        .query_one(
            &ctx.db.sql("SELECT count(*) FROM chess.board_photos WHERE label IS NULL AND NOT unreadable"),
            &[],
        )
        .await?
        .get(0);
    if waiting >= ctx.cfg.vision_review_max {
        return Ok(None);
    }
    let reading = local.map(|r| {
        json!({"placement": r.placement, "confidence": r.confidence, "unsure_squares": r.unsure_squares,
               "plausible": r.plausible, "model": model})
    });
    let (suggestion, failed) = match llm {
        Some(Ok(l)) => (
            Some(json!({"placement": l.placement, "white_at_bottom": l.white_at_bottom,
            "side_to_move": l.side_to_move, "confidence": l.confidence, "unsure_squares": l.unsure_squares,
            "model": ctx.cfg.vision_model, "at": chrono::Utc::now()})),
            None,
        ),
        Some(Err(e)) => (None, Some(format!("{e:#}").chars().take(300).collect::<String>())),
        None => (None, None),
    };
    let row = c
        .query_one(
            &ctx.db.sql(
                "INSERT INTO chess.board_photos (player_id, image, mime, reason, reading, suggestion, suggestion_failed)
                 VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
            ),
            &[&player, &bytes, &mime, &reason, &reading, &suggestion, &failed],
        )
        .await?;
    Ok(Some(row.get(0)))
}

#[derive(Deserialize)]
pub struct ConfirmBody {
    /// The position the photo shows, as FEN (only the placement is used,
    /// from White's side).
    placement: String,
}

/// `POST /api/esl/photos/{id}/confirm`: the player who sent a photo says
/// what it showed (they picked the move it matched, or finished setting the
/// position up in the editor). A label for training, marked as the
/// player's: a reviewer's label always wins.
pub async fn confirm_photo(
    State(ctx): State<Ctx>,
    who: Who,
    Path(id): Path<i64>,
    Json(b): Json<ConfirmBody>,
) -> ApiResult<Response> {
    let grid = chess_analysis::photo::parse_placement(&b.placement)
        .ok_or_else(|| ApiError::bad("that isn't a board: eight ranks of eight squares"))?;
    let placement = chess_analysis::photo::placement(&grid);
    let c = ctx.db.conn().await?;
    let n = c
        .execute(
            &ctx.db.sql(
                "UPDATE chess.board_photos SET label = $3, label_source = 'player', labelled_by = $2, labelled_at = now()
                  WHERE id = $1 AND player_id = $2 AND NOT unreadable AND coalesce(label_source, 'player') = 'player'",
            ),
            &[&id, &who.0.player, &placement],
        )
        .await?;
    if n == 0 {
        return Err(ApiError::not_found("photo"));
    }
    Ok(ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_drops_move_counters_and_idle_en_passant() {
        let typed = Game::new(Some("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1")).unwrap();
        let played = Game::from_moves(None, "e2e4").unwrap();
        assert_eq!(position_key(&typed), "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq -");
        assert_eq!(position_key(&played), position_key(&typed));
        // A real en-passant chance is part of the position.
        let ep = Game::from_moves(None, "e2e4 a7a6 e4e5 d7d5").unwrap();
        assert!(position_key(&ep).ends_with(" d6"), "{}", position_key(&ep));
    }
}
