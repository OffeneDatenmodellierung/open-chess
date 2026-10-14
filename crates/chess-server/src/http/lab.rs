//! open-chess additions to the Lichess API, used by the chess
//! training app:
//!
//! - `POST /api/board/game/{id}/assist`: the app declares that its engine
//!   help was switched on or off during one of our games (ground truth for
//!   the fair-play model);
//! - `GET /api/esl/position?fen=`: the position dictionary — the deepest
//!   stored evaluation of a position, with win/draw/loss odds, and the
//!   deepest analysis a training app sent in (a hint);
//! - `POST /api/esl/position`: send in the app's own analysis of a position;
//! - `GET /api/esl/position/moves?fen=`: every legal move with its value,
//!   where the dictionary knows it;
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

/// The dictionary key for a position (see `chess_analysis::dictionary`).
pub fn position_key(game: &Game) -> String {
    chess_analysis::dictionary::position_key(game)
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

/// White's win/draw/loss odds for a score from the side to move's view.
fn wdl_for(game: &Game, cp: i32) -> chess_analysis::Wdl {
    let white_cp = if game.turn() == chess_core::Side::White { cp } else { -cp };
    let phase = chess_analysis::eval::phase(game.board());
    chess_analysis::Wdl::from_cp(white_cp, chess_analysis::wdl::Params::for_phase(phase))
}

fn parse_fen(fen: &str) -> Result<Game, ApiError> {
    Game::new(Some(fen.trim())).map_err(|e| ApiError::bad(format!("bad FEN: {e:?}")))
}

/// A stored evaluation is engine help: not while you're playing here.
async fn not_playing(ctx: &Ctx, c: &impl tokio_postgres::GenericClient, player: i64) -> ApiResult<()> {
    if !model::ongoing_games(&ctx.db, c, player).await?.is_empty() {
        return Err(ApiError::forbidden("position lookups are off while you have a game in progress"));
    }
    Ok(())
}

pub async fn position(
    State(ctx): State<Ctx>,
    who: Who,
    Query(q): Query<PositionQuery>,
) -> ApiResult<Response> {
    let game = parse_fen(&q.fen)?;
    let c = ctx.db.conn().await?;
    not_playing(&ctx, &*c, who.0.player).await?;
    let key = position_key(&game);
    let row = c
        .query_opt(
            &ctx.db.sql(
                "SELECT best, score_cp, depth, engine, seen, photos FROM chess.positions WHERE key = $1",
            ),
            &[&key],
        )
        .await?;
    let hint = c
        .query_opt(
            &ctx.db.sql(
                "SELECT best, score_cp, depth, engine FROM chess.position_hints WHERE key = $1
                  ORDER BY depth DESC, at DESC LIMIT 1",
            ),
            &[&key],
        )
        .await?
        .map(|h| {
            let cp: i32 = h.get(1);
            json!({"best": h.get::<_, Option<String>>(0), "scoreCp": cp, "depth": h.get::<_, i32>(2),
                   "engine": h.get::<_, String>(3), "wdl": wdl_for(&game, cp)})
        });
    let Some(r) = row else {
        return Ok(Json(json!({ "key": key, "known": false, "hint": hint })).into_response());
    };
    let photos: i32 = r.get(5);
    let Some(score) = r.get::<_, Option<i32>>(1) else {
        // Confirmed from a photo, waiting for the worker.
        return Ok(Json(
            json!({ "key": key, "known": false, "queued": true, "photos": photos, "hint": hint }),
        )
        .into_response());
    };
    // Stored from the side to move's view; odds are given from White's.
    Ok(Json(json!({
        "key": key,
        "known": true,
        "best": r.get::<_, Option<String>>(0),
        "scoreCp": score,
        "depth": r.get::<_, i32>(2),
        "engine": r.get::<_, Option<String>>(3),
        "seen": r.get::<_, i32>(4),
        "photos": photos,
        "wdl": wdl_for(&game, score),
        "hint": hint,
    }))
    .into_response())
}

/// One move's value as the app sends it.
#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HintMove {
    uci: String,
    score_cp: i32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HintBody {
    fen: String,
    #[serde(default)]
    best: Option<String>,
    /// From the side to move's view.
    score_cp: i32,
    depth: i32,
    #[serde(default)]
    engine: Option<String>,
    /// Every move it scored (from the side to move's view).
    #[serde(default)]
    moves: Vec<HintMove>,
}

/// `POST /api/esl/position`: the training app's analysis of a position, a
/// hint for others looking it up. One per player and position (the deepest
/// kept); never mixed with the worker's evaluations. Refused while you're
/// playing a game here.
pub async fn position_hint(State(ctx): State<Ctx>, who: Who, Json(b): Json<HintBody>) -> ApiResult<Response> {
    let me = who.0.player;
    let game = parse_fen(&b.fen)?;
    if game.outcome().is_some() {
        return Err(ApiError::bad("the game is over in that position"));
    }
    if !(1..=99).contains(&b.depth) || b.score_cp.abs() > 32_000 || b.moves.len() > 256 {
        return Err(ApiError::bad("depth is 1–99, scores within ±32000, at most 256 moves"));
    }
    let legal = game.legal_moves();
    if b.best.as_ref().is_some_and(|m| !legal.contains(m)) || b.moves.iter().any(|m| !legal.contains(&m.uci))
    {
        return Err(ApiError::bad("a move that isn't legal there"));
    }
    let engine: String = b.engine.as_deref().unwrap_or("open-chess app").chars().take(60).collect();
    let moves =
        if b.moves.is_empty() { None } else { Some(serde_json::to_value(&b.moves).unwrap_or_default()) };
    let c = ctx.db.conn().await?;
    not_playing(&ctx, &*c, me).await?;
    c.execute(
        &ctx.db.sql(
            "INSERT INTO chess.position_hints (key, player_id, best, score_cp, depth, engine, moves)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (key, player_id) DO UPDATE SET best = EXCLUDED.best, score_cp = EXCLUDED.score_cp,
                 depth = EXCLUDED.depth, engine = EXCLUDED.engine, moves = EXCLUDED.moves, at = now()
               WHERE EXCLUDED.depth >= chess.position_hints.depth",
        ),
        &[&position_key(&game), &me, &b.best, &b.score_cp, &b.depth, &engine, &moves],
    )
    .await?;
    Ok(ok())
}

/// `GET /api/esl/position/moves?fen=`: every legal move, each with the
/// best value the dictionary has for it, from the mover's view, and
/// White's odds after it: the worker's scoring of this position's moves
/// first, then the evaluation stored for the position the move leads to,
/// then an app's hint. `null` where nothing is known.
pub async fn position_moves(
    State(ctx): State<Ctx>,
    who: Who,
    Query(q): Query<PositionQuery>,
) -> ApiResult<Response> {
    let game = parse_fen(&q.fen)?;
    let c = ctx.db.conn().await?;
    not_playing(&ctx, &*c, who.0.player).await?;
    let key = position_key(&game);
    let legal = game.legal_moves();
    // (uci, the position after it)
    let mut children = Vec::with_capacity(legal.len());
    for m in &legal {
        let mut g = game.clone();
        if g.play_uci(m).is_ok() {
            children.push((m.clone(), g));
        }
    }
    let child_keys: Vec<String> = children.iter().map(|(_, g)| position_key(g)).collect();
    let mut known: std::collections::HashMap<String, (i32, i32)> = std::collections::HashMap::new();
    for r in c
        .query(
            &ctx.db.sql("SELECT key, score_cp, depth FROM chess.positions WHERE key = ANY($1) AND score_cp IS NOT NULL"),
            &[&child_keys],
        )
        .await?
    {
        known.insert(r.get(0), (r.get(1), r.get(2)));
    }
    let moves_of = |row: Option<tokio_postgres::Row>| {
        row.and_then(|r| r.get::<_, Option<serde_json::Value>>(0))
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
    };
    let worker = moves_of(
        c.query_opt(&ctx.db.sql("SELECT moves FROM chess.positions WHERE key = $1"), &[&key]).await?,
    );
    let hints = moves_of(
        c.query_opt(
            &ctx.db.sql(
                "SELECT moves FROM chess.position_hints WHERE key = $1 AND moves IS NOT NULL ORDER BY depth DESC LIMIT 1",
            ),
            &[&key],
        )
        .await?,
    );
    let find = |list: &[serde_json::Value], uci: &str, field: &str| {
        list.iter().find(|m| m["uci"] == uci).and_then(|m| m[field].as_i64()).map(|v| v as i32)
    };
    let moves: Vec<serde_json::Value> = children
        .iter()
        .zip(&child_keys)
        .map(|((uci, after), ck)| {
            let (cp, depth, source) = if let Some(cp) = find(&worker, uci, "cp") {
                (Some(cp), find(&worker, uci, "depth"), "engine")
            } else if let Some(&(cp, d)) = known.get(ck) {
                // Stored for the side to move after it: the other side.
                (Some(-cp), Some(d), "after")
            } else if let Some(cp) = find(&hints, uci, "scoreCp") {
                (Some(cp), None, "hint")
            } else {
                (None, None, "unknown")
            };
            let wdl = cp.map(|v| wdl_for(after, -v));
            json!({"uci": uci, "san": chess_analysis::explain_san(game.board(), uci).map(|e| e.san), "scoreCp": cp,
                   "depth": depth, "source": source, "wdl": wdl})
        })
        .collect();
    Ok(Json(json!({"key": key, "moves": moves})).into_response())
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

/// The photo licence's version (docs/photo-licence.md). A photo is kept
/// for review, and trained on, only under terms its sender accepted.
pub const PHOTO_TERMS: &str = "open-chess-photo-licence-1";

/// The photo terms a player accepted, if the current ones: (terms, public).
pub(crate) async fn accepted_terms(ctx: &Ctx, player: i64) -> anyhow::Result<Option<bool>> {
    let c = ctx.db.conn().await?;
    let r = c
        .query_opt(
            &ctx.db.sql("SELECT photo_terms, photo_public FROM chess.players WHERE id = $1"),
            &[&player],
        )
        .await?;
    Ok(r.and_then(|r| (r.get::<_, Option<String>>(0).as_deref() == Some(PHOTO_TERMS)).then(|| r.get(1))))
}

#[derive(Debug, Deserialize)]
pub struct TermsBody {
    /// Accept the photo licence (false withdraws, for photos sent later).
    accept: bool,
    /// Also release my photos as CC0 for a public dataset.
    #[serde(default)]
    public: bool,
}

fn terms_json(accepted: Option<bool>) -> serde_json::Value {
    json!({
        "terms": PHOTO_TERMS,
        "accepted": accepted.is_some(),
        "public": accepted.unwrap_or(false),
        "text": "/docs/photo-licence",
    })
}

/// `GET /api/esl/photo-terms`: the photo licence, and whether I accepted it.
pub async fn photo_terms(State(ctx): State<Ctx>, who: Who) -> ApiResult<Response> {
    Ok(Json(terms_json(accepted_terms(&ctx, who.0.player).await?)).into_response())
}

/// `POST /api/esl/photo-terms` (JSON `{accept, public}`): accept (or
/// withdraw from) the photo licence for photos I send from now on, and
/// whether they're also CC0.
pub async fn set_photo_terms(
    State(ctx): State<Ctx>,
    who: Who,
    Json(b): Json<TermsBody>,
) -> ApiResult<Response> {
    let c = ctx.db.conn().await?;
    let terms = b.accept.then_some(PHOTO_TERMS);
    c.execute(
        &ctx.db.sql(
            "UPDATE chess.players SET photo_terms = $2, photo_terms_at = now(), photo_public = $3 WHERE id = $1",
        ),
        &[&who.0.player, &terms, &(b.accept && b.public)],
    )
    .await?;
    Ok(Json(terms_json(b.accept.then_some(b.public))).into_response())
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
    // Kept for review (and training) only under the photo licence.
    let terms = if ctx.cfg.vision_review { accepted_terms(&ctx, me).await? } else { None };
    let terms_needed = ctx.cfg.vision_review && terms.is_none();
    let photo_id = match terms {
        Some(public) => {
            let r = local.as_ref();
            keep_photo(&ctx, me, &bytes, &mime, reason, r, model_name.as_deref(), llm.as_ref(), public)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!("keeping a photo for review: {e:#}");
                    None
                })
        }
        None => None,
    };
    let llm_answer = |l: &vision::Reading| {
        let mut v = llm_json(l, &ctx.cfg.vision_model);
        v["source"] = json!("llm");
        v["photoId"] = json!(photo_id);
        v["review"] = json!(photo_id.is_some());
        v["termsNeeded"] = json!(terms_needed);
        v
    };
    match (local, llm) {
        // The reader's reading can't be a game position but the model's
        // can: give the model's.
        (Some(r), Some(Ok(l))) if !r.plausible => Ok(Json(llm_answer(&l)).into_response()),
        // Otherwise the reader's (if unsure), with the model's alongside.
        (Some(r), llm) => {
            let mut v = local_answer(&r, model_name.as_deref(), photo_id);
            v["termsNeeded"] = json!(terms_needed);
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
    public: bool,
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
                "INSERT INTO chess.board_photos (player_id, image, mime, reason, reading, suggestion, suggestion_failed, terms, public)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
            ),
            &[&player, &bytes, &mime, &reason, &reading, &suggestion, &failed, &PHOTO_TERMS, &public],
        )
        .await?;
    Ok(Some(row.get(0)))
}

#[derive(Deserialize)]
pub struct ConfirmBody {
    /// The position the photo shows, as FEN (only the placement is used,
    /// from White's side).
    placement: String,
    /// The full position, when the app knows it (side to move, castling):
    /// what goes into the position dictionary. Without it, both sides to
    /// move are queued.
    #[serde(default)]
    fen: Option<String>,
}

/// Queue a confirmed photo's position(s) in the dictionary for the
/// fair-play worker to evaluate (counting the photos of each).
pub async fn queue_photo_positions(
    db: &crate::db::Db,
    c: &impl tokio_postgres::GenericClient,
    games: &[Game],
) -> anyhow::Result<()> {
    let sql = db.sql(
        "INSERT INTO chess.positions (key, score_cp, depth, engine, seen, photos) VALUES ($1, NULL, 0, NULL, 0, 1)
         ON CONFLICT (key) DO UPDATE SET photos = chess.positions.photos + 1",
    );
    for g in games.iter().filter(|g| g.outcome().is_none()) {
        c.execute(&sql, &[&position_key(g)]).await?;
    }
    Ok(())
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
    // The sender's word is one vote: another person agreeing labels it.
    vote(&ctx, &*c, id, who.0.player, Some(&placement), false).await?;
    // The position joins the dictionary: the given one if it matches the
    // placement, else each side to move.
    let given = b
        .fen
        .as_deref()
        .and_then(|f| Game::new(Some(f.trim())).ok())
        .filter(|g| g.fen().split_whitespace().next() == Some(placement.as_str()));
    let games = match given {
        Some(g) => vec![g],
        None => chess_analysis::dictionary::photo_positions(&grid, None),
    };
    if let Err(e) = queue_photo_positions(&ctx.db, &*c, &games).await {
        tracing::warn!("queueing a photo's position: {e:#}");
    }
    Ok(ok())
}

// ─── photo review (reviewers: `chess:fairplay`, managers and admins) ───

/// Admins and managers: their label is final, and they see every photo.
fn staff(who: &Who) -> bool {
    who.0.admin || who.0.manager
}

/// Record what `player` says photo `id` shows, then settle it: two people
/// agreeing (the sender counts, by confirming it) label it, unless an
/// admin already has.
async fn vote(
    ctx: &Ctx,
    c: &impl tokio_postgres::GenericClient,
    id: i64,
    player: i64,
    placement: Option<&str>,
    unreadable: bool,
) -> anyhow::Result<()> {
    c.execute(
        &ctx.db.sql(
            "INSERT INTO chess.photo_votes (photo_id, player_id, placement, unreadable) VALUES ($1, $2, $3, $4)
             ON CONFLICT (photo_id, player_id) DO UPDATE SET placement = EXCLUDED.placement,
                 unreadable = EXCLUDED.unreadable, voted_at = now()",
        ),
        &[&id, &player, &placement, &unreadable],
    )
    .await?;
    // The answer two or more people gave, if any.
    let agreed = c
        .query_opt(
            &ctx.db.sql(
                "SELECT placement, unreadable FROM chess.photo_votes WHERE photo_id = $1
                  GROUP BY placement, unreadable HAVING count(*) >= 2 ORDER BY count(*) DESC LIMIT 1",
            ),
            &[&id],
        )
        .await?;
    if let Some(r) = agreed {
        let (placement, unreadable): (Option<String>, bool) = (r.get(0), r.get(1));
        c.execute(
            &ctx.db.sql(
                "UPDATE chess.board_photos SET label = $2, unreadable = $3, label_source = CASE WHEN $3 THEN NULL ELSE 'consensus' END,
                        labelled_by = NULL, labelled_at = now()
                  WHERE id = $1 AND coalesce(label_source, '') NOT IN ('admin', 'reviewer')
                    AND NOT (unreadable AND labelled_by IS NOT NULL)",
            ),
            &[&id, &placement, &unreadable],
        )
        .await?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct PhotoList {
    #[serde(default)]
    all: bool,
}

/// `GET /api/esl/photos`: board photos waiting for people to say what they
/// show, for anyone signed in: not their own, not ones they've answered,
/// not ones already settled (two agreeing, or an admin), newest first,
/// without the image bytes. Admins and managers see disputed photos
/// first, and with `?all=true` every photo.
pub async fn photos(State(ctx): State<Ctx>, who: Who, Query(q): Query<PhotoList>) -> ApiResult<Response> {
    let me = who.0.player;
    let staff = staff(&who);
    let all = q.all && staff;
    let c = ctx.db.conn().await?;
    let rows = c
        .query(
            &ctx.db.sql(
                "SELECT p.id, p.created_at, p.reason, p.reading, p.suggestion, p.suggestion_failed, p.label, p.label_source,
                        p.unreadable, p.labelled_at, p.public, p.player_id = $2,
                        (SELECT count(*) FROM chess.photo_votes v WHERE v.photo_id = p.id),
                        (SELECT row_to_json(v) FROM (SELECT placement, unreadable FROM chess.photo_votes
                          WHERE photo_id = p.id AND player_id = $2) v)
                   FROM chess.board_photos p
                  WHERE $1 OR (
                        coalesce(p.label_source, 'player') = 'player' AND NOT p.unreadable
                        AND ($3 OR (p.terms IS NOT NULL AND coalesce(p.player_id, 0) <> $2
                                    AND NOT EXISTS (SELECT 1 FROM chess.photo_votes v WHERE v.photo_id = p.id AND v.player_id = $2))))
                  ORDER BY CASE WHEN $3 THEN (SELECT count(*) FROM chess.photo_votes v WHERE v.photo_id = p.id) ELSE 0 END DESC, p.id DESC
                  LIMIT 200",
            ),
            &[&all, &me, &staff],
        )
        .await?;
    let list: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<_, i64>(0),
                "at": r.get::<_, chrono::DateTime<chrono::Utc>>(1),
                "reason": r.get::<_, String>(2),
                "reading": r.get::<_, Option<serde_json::Value>>(3),
                "suggestion": r.get::<_, Option<serde_json::Value>>(4),
                "suggestion_failed": r.get::<_, Option<String>>(5),
                "label": r.get::<_, Option<String>>(6),
                "label_source": r.get::<_, Option<String>>(7),
                "unreadable": r.get::<_, bool>(8),
                "labelled_at": r.get::<_, Option<chrono::DateTime<chrono::Utc>>>(9),
                "public": r.get::<_, bool>(10),
                "mine": r.get::<_, Option<bool>>(11).unwrap_or(false),
                "votes": r.get::<_, i64>(12),
                "my_vote": r.get::<_, Option<serde_json::Value>>(13),
                // Three or more people, and still no two agree: for an admin.
                "disputed": r.get::<_, i64>(12) >= 3 && r.get::<_, Option<String>>(7).as_deref() != Some("consensus"),
            })
        })
        .collect();
    Ok(Json(json!({ "photos": list, "staff": staff, "terms": PHOTO_TERMS })).into_response())
}

/// `GET /api/esl/photos/{id}/image`: a photo's bytes.
pub async fn photo_image(State(ctx): State<Ctx>, _who: Who, Path(id): Path<i64>) -> ApiResult<Response> {
    let c = ctx.db.conn().await?;
    let r = c
        .query_opt(&ctx.db.sql("SELECT image, mime FROM chess.board_photos WHERE id = $1"), &[&id])
        .await?
        .ok_or_else(|| ApiError::not_found("photo"))?;
    let (bytes, mime): (Vec<u8>, String) = (r.get(0), r.get(1));
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, mime),
            (axum::http::header::CACHE_CONTROL, "private, max-age=3600".into()),
        ],
        bytes,
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
pub struct PhotoLabel {
    /// FEN piece placement from White's side (the first field is used).
    #[serde(default)]
    placement: Option<String>,
    /// Not a board, or not one anybody could read.
    #[serde(default)]
    unreadable: bool,
}

/// `POST /api/esl/photos/{id}/label` (JSON `{placement}` or
/// `{unreadable: true}`): what I say the photo shows. Anyone signed in may
/// answer; two agreeing answers label it. An admin's or manager's answer is
/// the label at once, whatever others said. A labelled position joins the
/// dictionary (each side to move: a photo doesn't say).
pub async fn label_photo(
    State(ctx): State<Ctx>,
    who: Who,
    Path(id): Path<i64>,
    Json(b): Json<PhotoLabel>,
) -> ApiResult<Response> {
    let player = who.0.player;
    let grid = match (&b.placement, b.unreadable) {
        (_, true) => None,
        (Some(p), false) => Some(
            chess_analysis::photo::parse_placement(p.split_whitespace().next().unwrap_or(""))
                .ok_or_else(|| ApiError::bad("that isn't a board: eight ranks of eight squares"))?,
        ),
        (None, false) => return Err(ApiError::bad("say what the board shows, or mark it unreadable")),
    };
    let placement = grid.as_ref().map(chess_analysis::photo::placement);
    let c = ctx.db.conn().await?;
    let exists = c.query_opt(&ctx.db.sql("SELECT 1 FROM chess.board_photos WHERE id = $1"), &[&id]).await?;
    if exists.is_none() {
        return Err(ApiError::not_found("photo"));
    }
    vote(&ctx, &*c, id, player, placement.as_deref(), b.unreadable).await?;
    if staff(&who) {
        c.execute(
            &ctx.db.sql(
                "UPDATE chess.board_photos SET label = $2, unreadable = $3, labelled_by = $4, labelled_at = now(),
                        label_source = CASE WHEN $3 THEN NULL ELSE 'admin' END
                  WHERE id = $1",
            ),
            &[&id, &placement, &b.unreadable, &player],
        )
        .await?;
    }
    let row = c
        .query_one(
            &ctx.db.sql("SELECT label, label_source, unreadable FROM chess.board_photos WHERE id = $1"),
            &[&id],
        )
        .await?;
    let settled =
        matches!(row.get::<_, Option<String>>(1).as_deref(), Some("consensus" | "admin" | "reviewer"))
            || row.get::<_, bool>(2);
    if let (Some(grid), true) = (&grid, settled && row.get::<_, Option<String>>(0) == placement) {
        let games = chess_analysis::dictionary::photo_positions(grid, None);
        if let Err(e) = queue_photo_positions(&ctx.db, &*c, &games).await {
            tracing::warn!("queueing a photo's position: {e:#}");
        }
    }
    Ok(Json(json!({ "ok": true, "settled": settled, "label": row.get::<_, Option<String>>(0) }))
        .into_response())
}
