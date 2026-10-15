//! What the iOS app needs beyond the Lichess API (docs/ios.md).
//!
//! * `POST /api/games/offline`: games played offline, uploaded later.
//!   Against the computer they're recorded (and rated, at the level's fixed
//!   rating, when no help was used) at once; between two people on one
//!   device they wait for the other player to confirm.
//! * `POST /api/games/{id}/confirm`: the other player accepts or declines
//!   such a result.
//! * `GET /api/sync?since=`: one small answer with everything the app shows
//!   between syncs, gzipped, for slow and metered links.
//! * `POST /api/photos`: a board photo with the position the player
//!   confirmed, under the photo licence.
//! * `POST /api/push/devices`, `DELETE /api/push/devices/{token}`: where
//!   to send "your move".
//! * `DELETE /api/account`: closing the account from the app (the App
//!   Store requires it).

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use chess_core::{Game, Side, Speed, Status};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_postgres::Transaction;

use crate::app::{Ctx, Who};
use crate::error::{ApiError, ApiResult};
use crate::model::{self, Account};
use crate::play;

/// Games per upload.
const MAX_GAMES: usize = 50;
/// A photo's largest size (bytes, decoded).
const MAX_PHOTO: usize = 4 * 1024 * 1024;

/// One finished game as the app's core records it
/// (`GameSession::offline_record_json`).
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfflineRecord {
    pub client_id: String,
    /// "computer" or "passAndPlay".
    pub mode: String,
    #[serde(default)]
    pub bot_level: Option<u8>,
    pub white: String,
    pub black: String,
    pub speed: String,
    #[serde(default)]
    pub initial_fen: Option<String>,
    pub moves: Vec<String>,
    #[serde(default)]
    pub move_times_ms: Vec<i64>,
    pub started_ms: i64,
    pub status: String,
    #[serde(default)]
    pub winner: Option<String>,
    #[serde(default)]
    pub assistance: Assistance,
    #[serde(default)]
    pub ratable: bool,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, serde::Serialize)]
pub struct Assistance {
    #[serde(default)]
    pub hints: u32,
    #[serde(default)]
    pub takebacks: u32,
    #[serde(default)]
    pub analysed: bool,
}

impl Assistance {
    fn any(self) -> bool {
        self.hints > 0 || self.takebacks > 0 || self.analysed
    }
}

/// A record checked against the rules: the replayed game and its result.
#[derive(Debug)]
struct Checked {
    game: Game,
    status: Status,
    winner: Option<Side>,
    speed: Speed,
    /// Why it can't be rated, when it can't.
    unrated: Option<&'static str>,
}

/// Replay and check a record. `Err`: not a game we'll keep.
fn check(r: &OfflineRecord, now_ms: i64) -> Result<Checked, String> {
    if r.client_id.is_empty() || r.client_id.len() > 64 {
        return Err("clientId is 1 to 64 characters".into());
    }
    if r.moves.len() > 1000 {
        return Err("too many moves".into());
    }
    let fen = r.initial_fen.as_deref().filter(|f| !f.trim().is_empty());
    let game =
        Game::from_moves(fen, &r.moves.join(" ")).map_err(|e| format!("the moves don't replay: {e}"))?;
    let speed = Speed::from_key(&r.speed).ok_or_else(|| format!("unknown speed {}", r.speed))?;
    let claimed_winner = match r.winner.as_deref() {
        None | Some("") => None,
        Some(w) => Some(Side::from_name(w).ok_or_else(|| format!("winner {w}?"))?),
    };
    // The board decides when it can (mate, stalemate, repetition…); the
    // record otherwise (resignation, agreed draw, time).
    let (status, winner) = if let Some(o) = game.outcome() {
        (o.status, o.winner)
    } else {
        match Status::from_name(&r.status) {
            Some(Status::Resign | Status::Outoftime) if claimed_winner.is_some() => {
                let s = Status::from_name(&r.status).expect("matched");
                let w = claimed_winner.expect("checked");
                if s == Status::Outoftime && game.cannot_win(w) { (s, None) } else { (s, Some(w)) }
            }
            Some(Status::Draw) => (Status::Draw, None),
            _ => return Err(format!("not a finished game ({})", r.status)),
        }
    };
    let mut unrated = None;
    if r.assistance.any() {
        unrated = Some("help was used");
    } else if fen.is_some() {
        unrated = Some("it started from a set position");
    } else if game.ply() < 2 {
        unrated = Some("fewer than two moves");
    }
    // Move times: one per move, in order, not in the future.
    if !r.move_times_ms.is_empty() {
        let ordered = r.move_times_ms.windows(2).all(|w| w[0] <= w[1]);
        let fits = r.move_times_ms.len() == r.moves.len()
            && r.move_times_ms.first().is_some_and(|&t| t >= r.started_ms)
            && r.move_times_ms.last().is_some_and(|&t| t <= now_ms + 300_000);
        if !(ordered && fits) && unrated.is_none() {
            unrated = Some("the move times don't add up");
        }
    } else if unrated.is_none() {
        unrated = Some("no move times");
    }
    Ok(Checked { game, status, winner, speed, unrated })
}

#[derive(Deserialize)]
pub struct Upload {
    games: Vec<Value>,
}

/// `POST /api/games/offline`.
pub async fn upload(State(ctx): State<Ctx>, who: Who, Json(body): Json<Upload>) -> ApiResult<Response> {
    let id = who.need(&["board:play"])?.clone();
    if body.games.len() > MAX_GAMES {
        return Err(ApiError::bad(format!("at most {MAX_GAMES} games at a time")));
    }
    let mut results = Vec::with_capacity(body.games.len());
    for g in body.games {
        let client_id = g["clientId"].as_str().unwrap_or_default().to_owned();
        let r = match serde_json::from_value::<OfflineRecord>(g) {
            Ok(r) => upload_one(&ctx, id.player, r).await?,
            Err(e) => json!({"clientId": client_id, "status": "rejected", "reason": e.to_string()}),
        };
        results.push(r);
    }
    Ok(Json(json!({ "results": results })).into_response())
}

async fn upload_one(ctx: &Ctx, player: i64, r: OfflineRecord) -> ApiResult<Value> {
    let reject = |reason: String| json!({"clientId": r.client_id, "status": "rejected", "reason": reason});
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let me = model::ensure_account(&ctx.db, &tx, player).await?;
    if let Some(row) = tx
        .query_opt(
            &ctx.db.sql(
                "SELECT game_id, status FROM chess.offline_games WHERE player_id = $1 AND client_id = $2",
            ),
            &[&player, &r.client_id],
        )
        .await?
    {
        let (game_id, status): (String, String) = (row.get(0), row.get(1));
        return Ok(
            json!({"clientId": r.client_id, "gameId": game_id, "status": "duplicate", "state": status}),
        );
    }
    let now = chrono::Utc::now().timestamp_millis();
    let checked = match check(&r, now) {
        Ok(c) => c,
        Err(e) => return Ok(reject(e)),
    };
    let my_side = if me.username.eq_ignore_ascii_case(&r.white) {
        Side::White
    } else if me.username.eq_ignore_ascii_case(&r.black) {
        Side::Black
    } else {
        return Ok(reject(format!("you ({}) aren't white or black", me.username)));
    };
    let other_name = if my_side == Side::White { &r.black } else { &r.white };
    let game_id = model::new_id();
    let result = match r.mode.as_str() {
        "computer" => {
            let level = r.bot_level.unwrap_or(0);
            if !(1..=8).contains(&level) || crate::ai::level_of(other_name) != Some(level) {
                return Ok(reject(format!(
                    "the computer is ai-level-1 to 8 (got {other_name}, level {level})"
                )));
            }
            let bot = crate::ai::ensure(ctx, level).await?;
            let rated = checked.unrated.is_none();
            let (white, black) = if my_side == Side::White { (&me, &bot) } else { (&bot, &me) };
            let row = record(ctx, &tx, &game_id, white, black, &r, &checked, rated).await?;
            remember(ctx, &tx, player, &r, &game_id, None, "recorded", rated).await?;
            let diff = if my_side == Side::White { row.white_rating_diff } else { row.black_rating_diff };
            json!({"clientId": r.client_id, "gameId": game_id, "status": if rated { "rated" } else { "unrated" },
                   "reason": checked.unrated, "ratingDiff": diff})
        }
        "passAndPlay" => {
            let Some(other) = model::account_by_name(&ctx.db, &tx, other_name).await? else {
                return Ok(reject(format!("no player called {other_name}")));
            };
            if other.player_id == player || other.bot {
                return Ok(reject(format!("{other_name} can't confirm this game")));
            }
            remember(
                ctx,
                &tx,
                player,
                &r,
                &game_id,
                Some(other.player_id),
                "pending",
                checked.unrated.is_none(),
            )
            .await?;
            crate::push::notify(
                ctx,
                other.player_id,
                "A result to confirm".into(),
                format!("{} recorded a game against you. Confirm it in the app.", me.username),
                Some(game_id.clone()),
                None,
            );
            json!({"clientId": r.client_id, "gameId": game_id, "status": "pending", "reason": checked.unrated})
        }
        m => return Ok(reject(format!("mode {m}?"))),
    };
    tx.commit().await?;
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
async fn remember(
    ctx: &Ctx,
    tx: &Transaction<'_>,
    player: i64,
    r: &OfflineRecord,
    game_id: &str,
    opponent: Option<i64>,
    status: &str,
    rated: bool,
) -> anyhow::Result<()> {
    tx.execute(
        &ctx.db.sql(
            "INSERT INTO chess.offline_games (player_id, client_id, game_id, opponent_id, record, status, rated, decided_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, CASE WHEN $6 = 'pending' THEN NULL ELSE now() END)",
        ),
        &[&player, &r.client_id, &game_id, &opponent, &serde_json::to_value(r)?, &status, &rated],
    )
    .await?;
    Ok(())
}

/// Record a checked offline game as a finished game here: rated as any
/// other (play::finish), queued for fair-play screening, and in the feed
/// from now (its feed time is the upload, its finish time the last move).
#[allow(clippy::too_many_arguments)]
async fn record(
    ctx: &Ctx,
    tx: &Transaction<'_>,
    id: &str,
    white: &Account,
    black: &Account,
    r: &OfflineRecord,
    c: &Checked,
    rated: bool,
) -> anyhow::Result<model::GameRow> {
    let perf = c.speed.key();
    let (wr, _, _) = model::rating(&ctx.db, tx, white.player_id, perf).await?;
    let (br, _, _) = model::rating(&ctx.db, tx, black.player_id, perf).await?;
    let times: Vec<i64> = r.move_times_ms.clone();
    let move_ms: Vec<i64> = times
        .iter()
        .scan(r.started_ms, |prev, &t| {
            let d = (t - *prev).max(0);
            *prev = t;
            Some(d)
        })
        .collect();
    let clock_ms: Vec<i64> = vec![model::NO_CLOCK_MS; move_ms.len()];
    let last = times.last().copied().unwrap_or(r.started_ms);
    let fen = r.initial_fen.as_deref().filter(|f| !f.trim().is_empty());
    #[allow(clippy::cast_precision_loss)]
    let (started, ended) = (r.started_ms as f64 / 1000.0, last as f64 / 1000.0);
    tx.execute(
        &ctx.db.sql(
            "INSERT INTO chess.games (id, white_id, black_id, variant, initial_fen, rated, source, speed,
                    status, moves, ply, fen, white_rating, black_rating, white_provisional, black_provisional,
                    move_ms, clock_ms, created_at, turn_started_at, last_move_at)
             VALUES ($1, $2, $3, $4, $5, $6, 'offline', $7, 20, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                     to_timestamp($17), to_timestamp($18), to_timestamp($18))",
        ),
        &[
            &id,
            &white.player_id,
            &black.player_id,
            &(if fen.is_some() { "fromPosition" } else { "standard" }),
            &c.game.initial_fen(),
            &rated,
            &perf,
            &r.moves.join(" "),
            &i32::try_from(c.game.ply()).unwrap_or(i32::MAX),
            &c.game.fen(),
            &wr.int(),
            &br.int(),
            &wr.provisional(),
            &br.provisional(),
            &move_ms,
            &clock_ms,
            &started,
            &ended,
        ],
    )
    .await?;
    let row = model::game(&ctx.db, tx, id).await?.ok_or_else(|| anyhow::anyhow!("offline game vanished"))?;
    play::finish(ctx, tx, &row, c.status, c.winner).await?;
    tx.execute(
        &ctx.db.sql("UPDATE chess.games SET finished_at = to_timestamp($2) WHERE id = $1"),
        &[&id, &ended],
    )
    .await?;
    model::game(&ctx.db, tx, id).await?.ok_or_else(|| anyhow::anyhow!("offline game vanished"))
}

#[derive(Deserialize)]
pub struct Confirm {
    accept: bool,
}

/// `POST /api/games/{id}/confirm`: the other player of a game played on
/// one device accepts (it's recorded, and rated if it can be) or declines.
pub async fn confirm(
    State(ctx): State<Ctx>,
    who: Who,
    Path(game_id): Path<String>,
    Json(body): Json<Confirm>,
) -> ApiResult<Response> {
    let id = who.need(&["board:play"])?.clone();
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    let row = tx
        .query_opt(
            &ctx.db.sql(
                "SELECT player_id, record, status, rated, opponent_id FROM chess.offline_games
                  WHERE game_id = $1 FOR UPDATE",
            ),
            &[&game_id],
        )
        .await?
        .ok_or_else(|| ApiError::not_found("game to confirm"))?;
    let (uploader, stored, status, rated, opponent): (i64, Value, String, bool, Option<i64>) =
        (row.get(0), row.get(1), row.get(2), row.get(3), row.get(4));
    if opponent != Some(id.player) {
        return Err(ApiError::not_found("game to confirm"));
    }
    if status != "pending" {
        return Err(ApiError::conflict(format!("already {status}")));
    }
    if !body.accept {
        tx.execute(
            &ctx.db.sql(
                "UPDATE chess.offline_games SET status = 'declined', decided_at = now() WHERE game_id = $1",
            ),
            &[&game_id],
        )
        .await?;
        tx.commit().await?;
        return Ok(Json(json!({"gameId": game_id, "status": "declined"})).into_response());
    }
    let r: OfflineRecord =
        serde_json::from_value(stored).map_err(|e| anyhow::anyhow!("stored record: {e}"))?;
    let checked = check(&r, chrono::Utc::now().timestamp_millis()).map_err(ApiError::bad)?;
    let me = model::ensure_account(&ctx.db, &tx, id.player).await?;
    let them = model::account(&ctx.db, &tx, uploader).await?.ok_or_else(|| ApiError::not_found("player"))?;
    let (white, black) =
        if them.username.eq_ignore_ascii_case(&r.white) { (&them, &me) } else { (&me, &them) };
    let rated = rated && checked.unrated.is_none();
    let row = record(&ctx, &tx, &game_id, white, black, &r, &checked, rated).await?;
    tx.execute(
        &ctx.db
            .sql("UPDATE chess.offline_games SET status = 'recorded', decided_at = now() WHERE game_id = $1"),
        &[&game_id],
    )
    .await?;
    tx.commit().await?;
    let diff = if row.white.player_id == id.player { row.white_rating_diff } else { row.black_rating_diff };
    Ok(Json(
        json!({"gameId": game_id, "status": if rated { "rated" } else { "unrated" }, "ratingDiff": diff}),
    )
    .into_response())
}

/// `GET /api/sync?since=<Unix ms>`: ratings, games in progress, results to
/// confirm, challenges, and games finished since `since`.
pub async fn sync(
    State(ctx): State<Ctx>,
    who: Who,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let player = who.0.player;
    let since: i64 = q.get("since").and_then(|s| s.parse().ok()).unwrap_or(0);
    let c = ctx.db.conn().await?;
    let me = model::ensure_account(&ctx.db, &*c, player).await?;
    let mut ratings = serde_json::Map::new();
    for r in c
        .query(
            &ctx.db.sql(
                "SELECT perf, rating, deviation, volatility, games FROM chess.ratings WHERE player_id = $1",
            ),
            &[&player],
        )
        .await?
    {
        let deviation: f64 = r.get(2);
        ratings.insert(
            r.get(0),
            json!({"rating": r.get::<_, f64>(1), "deviation": deviation, "volatility": r.get::<_, f64>(3),
                   "provisional": deviation > chess_core::Rating::PROVISIONAL_DEVIATION, "games": r.get::<_, i32>(4)}),
        );
    }
    let playing: Vec<Value> = model::ongoing_games(&ctx.db, &*c, player)
        .await?
        .iter()
        .filter_map(|g| {
            let side = g.side_of(player)?;
            let opp = g.player(side.opposite());
            let (orating, _, _) = g.rating(side.opposite());
            Some(json!({
                "gameId": g.id, "color": side.name(), "fen": g.fen, "lastMove": g.last_move(),
                "isMyTurn": g.status.is_ongoing() && g.turn() == side, "speed": g.perf_key(), "rated": g.rated,
                "daysPerTurn": g.days_per_turn,
                "secondsLeft": if g.has_clock() { Some(g.live_ms(side) / 1000) } else { g.days_left_ms().map(|m| m / 1000) },
                "opponent": {"username": opp.username, "rating": orating},
            }))
        })
        .collect();
    let confirmations: Vec<Value> = c
        .query(
            &ctx.db.sql(
                "SELECT o.game_id, o.record, a.username, (extract(epoch from o.created_at) * 1000)::bigint
                   FROM chess.offline_games o JOIN chess.accounts a ON a.player_id = o.player_id
                  WHERE o.opponent_id = $1 AND o.status = 'pending' ORDER BY o.created_at",
            ),
            &[&player],
        )
        .await?
        .iter()
        .map(|r| {
            let rec: Value = r.get(1);
            json!({"id": r.get::<_, String>(0), "white": rec["white"], "black": rec["black"], "winner": rec["winner"],
                   "status": rec["status"], "speed": rec["speed"], "moves": rec["moves"].as_array().map_or(0, Vec::len),
                   "uploadedBy": r.get::<_, String>(2), "createdAt": r.get::<_, i64>(3)})
        })
        .collect();
    let challenges: Vec<Value> = c
        .query(
            &ctx.db.sql(
                "SELECT ch.id, a.username, ch.rated, ch.clock_limit, ch.clock_increment, ch.days
                   FROM chess.challenges ch JOIN chess.accounts a ON a.player_id = ch.challenger_id
                  WHERE ch.dest_id = $1 AND ch.status = 'created' AND ch.expires_at > now()
                  ORDER BY ch.created_at",
            ),
            &[&player],
        )
        .await?
        .iter()
        .map(|r| {
            let (limit, inc, days): (Option<i32>, Option<i32>, Option<i32>) = (r.get(3), r.get(4), r.get(5));
            let speed = match (limit, inc) {
                (Some(l), Some(i)) => Speed::from_clock(l.unsigned_abs(), i.unsigned_abs()),
                _ => Speed::Correspondence,
            };
            json!({"id": r.get::<_, String>(0), "from": r.get::<_, String>(1), "rated": r.get::<_, bool>(2),
                   "speed": speed.key(), "daysPerTurn": days})
        })
        .collect();
    #[allow(clippy::cast_precision_loss)]
    let since_s = since as f64 / 1000.0;
    let finished: Vec<Value> = c
        .query(
            &ctx.db.sql(&format!(
                "{} WHERE (g.white_id = $1 OR g.black_id = $1) AND g.feed_at > to_timestamp($2)
                  ORDER BY g.feed_at LIMIT 100",
                model::GAME_SELECT
            )),
            &[&player, &since_s],
        )
        .await?
        .iter()
        .map(model::GameRow::from_row)
        .filter_map(|g| {
            let side = g.side_of(player)?;
            let diff = if side == Side::White { g.white_rating_diff } else { g.black_rating_diff };
            Some(json!({"gameId": g.id, "status": g.status.name(), "winner": g.winner.map(Side::name),
                        "speed": g.perf_key(), "rated": g.rated, "ratingDiff": diff, "source": g.source}))
        })
        .collect();
    Ok(Json(json!({
        "now": chrono::Utc::now().timestamp_millis(),
        "username": me.username,
        "ratings": ratings,
        "playing": playing,
        "confirmations": confirmations,
        "challenges": challenges,
        "finished": finished,
    }))
    .into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Photo {
    client_id: String,
    /// Base64 JPEG.
    image: String,
    /// The position the player confirmed (FEN placement, White's side).
    placement: String,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    model: Option<String>,
}

/// `POST /api/photos`: a photo with its confirmed position, kept as
/// training data under the photo licence the player accepted.
pub async fn photo(State(ctx): State<Ctx>, who: Who, Json(p): Json<Photo>) -> ApiResult<Response> {
    let player = who.0.player;
    if p.client_id.is_empty() || p.client_id.len() > 64 {
        return Err(ApiError::bad("clientId is 1 to 64 characters"));
    }
    let Some(public) = crate::http::lab::accepted_terms(&ctx, player).await? else {
        return Err(ApiError::forbidden("accept the photo licence first (/api/esl/photo-terms)"));
    };
    let placement = p.placement.split_whitespace().next().unwrap_or_default().to_owned();
    if chess_analysis::photo::parse_placement(&placement).is_none() {
        return Err(ApiError::bad("placement isn't a FEN placement"));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(p.image.trim())
        .map_err(|_| ApiError::bad("image isn't base64"))?;
    if bytes.len() > MAX_PHOTO || !bytes.starts_with(&[0xff, 0xd8]) {
        return Err(ApiError::bad("image is a JPEG of at most 4 MB"));
    }
    let reading =
        json!({"placement": placement, "confidence": p.confidence, "model": p.model, "from": "ios"});
    let c = ctx.db.conn().await?;
    let row = c
        .query_opt(
            &ctx.db.sql(
                "INSERT INTO chess.board_photos (player_id, image, mime, reason, reading, label, label_source,
                        labelled_by, labelled_at, terms, public, client_id)
                 VALUES ($1, $2, 'image/jpeg', 'contributed', $3, $4, 'player', $1, now(), $5, $6, $7)
                 ON CONFLICT (player_id, client_id) WHERE client_id IS NOT NULL DO NOTHING RETURNING id",
            ),
            &[&player, &bytes, &reading, &placement, &crate::http::lab::PHOTO_TERMS, &public, &p.client_id],
        )
        .await?;
    let id: i64 = match row {
        Some(r) => r.get(0),
        None => c
            .query_one(
                &ctx.db.sql("SELECT id FROM chess.board_photos WHERE player_id = $1 AND client_id = $2"),
                &[&player, &p.client_id],
            )
            .await?
            .get(0),
    };
    Ok(Json(json!({"id": id})).into_response())
}

#[derive(Deserialize)]
pub struct Device {
    token: String,
    #[serde(default)]
    sandbox: bool,
}

/// `POST /api/push/devices`.
pub async fn add_device(State(ctx): State<Ctx>, who: Who, Json(d): Json<Device>) -> ApiResult<Response> {
    let token = d.token.trim().to_ascii_lowercase();
    if !(32..=200).contains(&token.len()) || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::bad("token is the device token in hex"));
    }
    let c = ctx.db.conn().await?;
    c.execute(
        &ctx.db.sql(
            "INSERT INTO chess.push_devices (token, player_id, sandbox) VALUES ($1, $2, $3)
             ON CONFLICT (token) DO UPDATE SET player_id = excluded.player_id, sandbox = excluded.sandbox",
        ),
        &[&token, &who.0.player, &d.sandbox],
    )
    .await?;
    Ok(crate::http::ok())
}

/// `DELETE /api/push/devices/{token}`.
pub async fn remove_device(
    State(ctx): State<Ctx>,
    who: Who,
    Path(token): Path<String>,
) -> ApiResult<Response> {
    let c = ctx.db.conn().await?;
    c.execute(
        &ctx.db.sql("DELETE FROM chess.push_devices WHERE token = $1 AND player_id = $2"),
        &[&token.to_ascii_lowercase(), &who.0.player],
    )
    .await?;
    Ok(crate::http::ok())
}

/// `DELETE /api/account`: close the account. Games in progress are
/// resigned; finished games stay (the other players' history) under a
/// placeholder name; tokens, sessions, devices and personal details go.
/// Photos already given under the photo licence stay, without the name.
pub async fn delete_account(State(ctx): State<Ctx>, who: Who) -> ApiResult<Response> {
    let player = who.0.player;
    let ongoing = {
        let c = ctx.db.conn().await?;
        model::ongoing_games(&ctx.db, &*c, player).await?
    };
    for g in ongoing {
        let api = if g.side_of(player).is_some_and(|s| g.player(s).bot) {
            play::Api::Bot
        } else {
            play::Api::Board
        };
        if let Err(e) = play::resign(&ctx, &g.id, player, api).await {
            tracing::warn!(game = g.id, "closing account: {e:?}");
        }
    }
    let mut c = ctx.db.conn().await?;
    let tx = c.transaction().await?;
    for q in [
        "UPDATE chess.accounts SET username = 'deleted-' || player_id WHERE player_id = $1",
        "UPDATE chess.players SET name = 'deleted', email = NULL, key_hash = NULL, password_hash = NULL,
                esports_id = NULL, disabled_at = now() WHERE id = $1",
        "UPDATE chess.api_tokens SET revoked_at = now(), revoked_by = 'account closed'
          WHERE player_id = $1 AND revoked_at IS NULL",
        "UPDATE chess.sessions SET revoked_at = now() WHERE player_id = $1 AND revoked_at IS NULL",
        "DELETE FROM chess.push_devices WHERE player_id = $1",
        "DELETE FROM chess.passkeys WHERE player_id = $1",
        "UPDATE chess.offline_games SET status = 'declined', decided_at = now()
          WHERE (player_id = $1 OR opponent_id = $1) AND status = 'pending'",
        "UPDATE chess.challenges SET status = 'canceled' WHERE (challenger_id = $1 OR dest_id = $1) AND status = 'created'",
        "UPDATE chess.board_photos SET player_id = NULL WHERE player_id = $1",
    ] {
        tx.execute(&ctx.db.sql(q), &[&player]).await?;
    }
    tx.commit().await?;
    ctx.auth.forget_player(player);
    tracing::info!(player, "account closed");
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(moves: &[&str], status: &str, winner: Option<&str>) -> OfflineRecord {
        OfflineRecord {
            client_id: "g1".into(),
            mode: "computer".into(),
            bot_level: Some(4),
            white: "ann".into(),
            black: "ai-level-4".into(),
            speed: "rapid".into(),
            initial_fen: None,
            moves: moves.iter().map(|m| (*m).to_owned()).collect(),
            move_times_ms: (1..=moves.len() as i64).map(|i| 1_000 + i * 1_000).collect(),
            started_ms: 1_000,
            status: status.into(),
            winner: winner.map(str::to_owned),
            assistance: Assistance::default(),
            ratable: true,
        }
    }

    #[test]
    fn the_board_decides_when_it_can() {
        let c = check(&rec(&["f2f3", "e7e5", "g2g4", "d8h4"], "resign", Some("white")), 10_000_000).unwrap();
        assert_eq!((c.status, c.winner), (Status::Mate, Some(Side::Black)), "mate, whatever the record says");
        assert!(c.unrated.is_none());
        let r = check(&rec(&["e2e4", "e7e5"], "resign", Some("black")), 10_000_000).unwrap();
        assert_eq!((r.status, r.winner), (Status::Resign, Some(Side::Black)));
    }

    #[test]
    fn rejects_and_unrates() {
        assert!(check(&rec(&["e2e4", "e7e5"], "started", None), 10_000_000).is_err(), "not over");
        assert!(check(&rec(&["e2e5"], "resign", Some("black")), 10_000_000).is_err(), "illegal");
        let mut helped = rec(&["e2e4", "e7e5"], "draw", None);
        helped.assistance.hints = 1;
        assert_eq!(check(&helped, 10_000_000).unwrap().unrated, Some("help was used"));
        let mut future = rec(&["e2e4", "e7e5"], "draw", None);
        future.move_times_ms = vec![2_000, 99_000_000];
        assert_eq!(check(&future, 10_000_000).unwrap().unrated, Some("the move times don't add up"));
        let short = rec(&["e2e4"], "resign", Some("white"));
        assert_eq!(check(&short, 10_000_000).unwrap().unrated, Some("fewer than two moves"));
    }

    #[test]
    fn reads_the_cores_record() {
        let json = r#"{"clientId":"g1","mode":"computer","botLevel":4,"botRating":1400,"white":"ann",
            "black":"ai-level-4","speed":"rapid","moves":["f2f3","e7e5","g2g4","d8h4"],
            "moveTimesMs":[1000,2000,3000,4000],"startedMs":500,"status":"mate","winner":"black",
            "assistance":{"hints":0,"takebacks":0,"analysed":false},"ratable":true}"#;
        let r: OfflineRecord = serde_json::from_str(json).unwrap();
        assert_eq!(r.bot_level, Some(4));
        assert!(check(&r, 10_000).unwrap().unrated.is_none());
    }
}
