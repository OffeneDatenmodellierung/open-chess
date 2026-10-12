//! The fair-play worker (`open-chess analyse`): takes finished games off the
//! queue, has Stockfish evaluate every position, and stores a screening
//! report per human player. With no game to screen, it evaluates positions
//! people confirmed from board photos for the position dictionary, scoring
//! every legal move. Run as many as you like (games are claimed with
//! SKIP LOCKED and a lease, so a crashed worker's game is picked up again).

use std::time::Duration;

use anyhow::{Context, Result};
use chess_core::Side;
use chess_engine::{Engine as _, Limit, UciEngine};
use chess_fairplay::{
    AnalysisConfig, Model, app_match, app_suggestions, assess_with_app, evaluate_game, features,
};
use tokio::sync::watch;
use tracing::{info, warn};

use crate::config::AnalyseConfig;
use crate::db::Db;
use crate::metrics::Metrics;
use crate::model;

async fn spawn_engine(cfg: &AnalyseConfig) -> Result<UciEngine> {
    let path = cfg
        .stockfish
        .clone()
        .or_else(chess_engine::find_stockfish)
        .context("no Stockfish: install it or set CHESS_STOCKFISH")?;
    let threads = cfg.threads.max(1).to_string();
    let hash = cfg.hash_mb.max(16).to_string();
    let e = UciEngine::spawn(&path, &[("Threads", &threads), ("Hash", &hash)]).await?;
    info!(engine = e.name(), "fair-play engine ready");
    Ok(e)
}

/// Claim the next game to screen (or one whose worker died).
async fn claim(db: &Db) -> Result<Option<String>> {
    let c = db.conn().await?;
    Ok(c.query_opt(
        &db.sql(
            "UPDATE chess.games SET analysis = 'running', analysis_lease_until = now() + interval '15 minutes'
              WHERE id = (SELECT id FROM chess.games
                           WHERE analysis = 'pending' OR (analysis = 'running' AND analysis_lease_until < now())
                           ORDER BY finished_at LIMIT 1 FOR UPDATE SKIP LOCKED)
             RETURNING id",
        ),
        &[],
    )
    .await?
    .map(|r| r.get(0)))
}

async fn set_state(db: &Db, id: &str, state: &str) -> Result<()> {
    let c = db.conn().await?;
    c.execute(
        &db.sql("UPDATE chess.games SET analysis = $2, analysis_lease_until = NULL WHERE id = $1"),
        &[&id, &state],
    )
    .await?;
    Ok(())
}

async fn screen(
    db: &Db,
    metrics: &Metrics,
    engine: &mut UciEngine,
    cfg: &AnalyseConfig,
    screening: &Model,
    id: &str,
) -> Result<()> {
    let c = db.conn().await?;
    let row = model::game(db, &*c, id).await?.context("game vanished")?;
    drop(c);
    let moves: Vec<String> = row.moves.split_whitespace().map(str::to_owned).collect();
    let acfg = AnalysisConfig { limit: Limit::Depth(cfg.depth), multipv: 3, skip_plies: cfg.skip_plies };
    let evals = evaluate_game(engine, row.initial_fen.as_deref(), &moves, &row.move_ms, &acfg).await?;
    // What the training app's engine would have suggested at each move
    // (CPU-bound, so off the async threads).
    let app = {
        let (fen, mv, depth, nodes) = (row.initial_fen.clone(), moves.clone(), cfg.app_depth, cfg.app_nodes);
        let net = match &cfg.app_net {
            Some(p) => Some(std::sync::Arc::new(
                chess_analysis::nnue::Network::from_bytes(
                    &std::fs::read(p).with_context(|| format!("reading {p}"))?,
                )
                .map_err(|e| anyhow::anyhow!("{p}: {e}"))?,
            )),
            None => None,
        };
        tokio::task::spawn_blocking(move || app_suggestions(fen.as_deref(), &mv, depth, nodes, net)).await??
    };
    let c = db.conn().await?;
    store_positions(db, &*c, row.initial_fen.as_deref(), &evals, engine.name(), cfg.depth).await?;
    for side in [Side::White, Side::Black] {
        let player = row.player(side);
        if player.bot {
            continue; // bots are engines, openly
        }
        let f = features(&evals, side, cfg.skip_plies);
        let fingerprint = app_match(&evals, &app, side, cfg.skip_plies);
        let a = assess_with_app(&f, Some(&fingerprint), screening);
        let declared: bool = c
            .query_one(
                &db.sql("SELECT EXISTS (SELECT 1 FROM chess.assist_marks WHERE game_id = $1 AND player_id = $2 AND assist)"),
                &[&row.id, &player.player_id],
            )
            .await?
            .get(0);
        let side_evals: Vec<&chess_fairplay::MoveEval> = evals.iter().filter(|e| e.side == side).collect();
        c.execute(
            &db.sql(
                "INSERT INTO chess.fairplay_reports (game_id, side, player_id, score, flagged, moves, accuracy, acpl, top1,
                        top3, time_cv, time_corr, reasons, engine, depth, evals, app_disagree, app_picked,
                        app_picked_reference, declared_assist)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)
                 ON CONFLICT (game_id, side) DO UPDATE SET score = EXCLUDED.score, flagged = EXCLUDED.flagged,
                        moves = EXCLUDED.moves, accuracy = EXCLUDED.accuracy, acpl = EXCLUDED.acpl, top1 = EXCLUDED.top1,
                        top3 = EXCLUDED.top3, time_cv = EXCLUDED.time_cv, time_corr = EXCLUDED.time_corr,
                        reasons = EXCLUDED.reasons, engine = EXCLUDED.engine, depth = EXCLUDED.depth,
                        evals = EXCLUDED.evals, app_disagree = EXCLUDED.app_disagree, app_picked = EXCLUDED.app_picked,
                        app_picked_reference = EXCLUDED.app_picked_reference, declared_assist = EXCLUDED.declared_assist,
                        created_at = now()",
            ),
            &[
                &row.id,
                &side.name(),
                &player.player_id,
                &(a.score as f32),
                &a.flagged,
                &(f.moves as i32),
                &(f.accuracy as f32),
                &(f.acpl as f32),
                &(f.top1 as f32),
                &(f.top3 as f32),
                &f.time_cv.map(|v| v as f32),
                &f.time_criticality_corr.map(|v| v as f32),
                &a.reasons,
                &engine.name(),
                &(cfg.depth as i32),
                &serde_json::to_value(&side_evals)?,
                &(fingerprint.disagreements as i32),
                &(fingerprint.picked_app as i32),
                &(fingerprint.picked_reference as i32),
                &declared,
            ],
        )
        .await?;
        metrics.analysed.with_label_values(&[if a.flagged { "yes" } else { "no" }]).inc();
        if a.flagged {
            info!(
                game = row.id,
                player = player.username,
                score = a.score,
                "fair-play flag: {}",
                a.reasons.join("; ")
            );
        }
    }
    Ok(())
}

/// Add every screened position's evaluation to the position dictionary,
/// keeping the deepest one seen.
async fn store_positions(
    db: &Db,
    c: &impl tokio_postgres::GenericClient,
    initial_fen: Option<&str>,
    evals: &[chess_fairplay::MoveEval],
    engine: &str,
    depth: u32,
) -> Result<()> {
    let mut game = chess_core::Game::new(initial_fen).context("replaying for the position dictionary")?;
    let sql = db.sql(
        "INSERT INTO chess.positions (key, best, score_cp, depth, engine) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (key) DO UPDATE SET seen = chess.positions.seen + 1,
             best = CASE WHEN EXCLUDED.depth >= chess.positions.depth THEN EXCLUDED.best ELSE chess.positions.best END,
             score_cp = CASE WHEN EXCLUDED.depth >= chess.positions.depth THEN EXCLUDED.score_cp ELSE chess.positions.score_cp END,
             engine = CASE WHEN EXCLUDED.depth >= chess.positions.depth THEN EXCLUDED.engine ELSE chess.positions.engine END,
             depth = GREATEST(EXCLUDED.depth, chess.positions.depth), updated_at = now()",
    );
    for e in evals {
        let key = crate::http::lab::position_key(&game);
        c.execute(&sql, &[&key, &e.best, &e.best_cp, &(depth as i32), &engine]).await?;
        if game.play_uci(&e.uci).is_err() {
            break;
        }
    }
    Ok(())
}

/// The most positions' moves a dictionary evaluation scores (every legal
/// move, up to Stockfish's MultiPV limit).
const MAX_MULTIPV: u32 = 256;
/// Mates as centipawns, on the scale the screened positions are stored
/// with (chess-fairplay's).
const MATE_CP: i32 = 1000;

/// Evaluate one position waiting in the dictionary (from a photo): the
/// best move and score, and every legal move's score. `false` when there
/// was nothing to do.
async fn evaluate_queued(db: &Db, engine: &mut UciEngine, cfg: &AnalyseConfig) -> Result<bool> {
    let c = db.conn().await?;
    let Some(key) = c
        .query_opt(
            &db.sql(
                "UPDATE chess.positions SET eval_lease_until = now() + interval '10 minutes'
                  WHERE key = (SELECT key FROM chess.positions
                                WHERE score_cp IS NULL AND (eval_lease_until IS NULL OR eval_lease_until < now())
                                ORDER BY updated_at LIMIT 1 FOR UPDATE SKIP LOCKED)
                 RETURNING key",
            ),
            &[],
        )
        .await?
        .map(|r| r.get::<_, String>(0))
    else {
        return Ok(false);
    };
    drop(c);
    let fen = chess_analysis::dictionary::fen_of_key(&key);
    let legal = chess_core::Game::new(Some(&fen)).map_or(0, |g| g.legal_moves().len());
    if legal == 0 {
        // Not a position anyone can play on: drop it.
        let c = db.conn().await?;
        c.execute(&db.sql("DELETE FROM chess.positions WHERE key = $1 AND score_cp IS NULL"), &[&key])
            .await?;
        return Ok(true);
    }
    let multipv = u32::try_from(legal).unwrap_or(MAX_MULTIPV).min(MAX_MULTIPV);
    engine.new_game().await?;
    let a = engine.analyse(&fen, &[], Limit::Depth(cfg.depth), multipv).await?;
    let Some(top) = a.lines.first() else { anyhow::bail!("no lines for {key}") };
    let moves: Vec<serde_json::Value> = a
        .lines
        .iter()
        .filter_map(|l| {
            Some(serde_json::json!({"uci": l.pv.first()?, "cp": l.score.to_cp(MATE_CP), "depth": l.depth}))
        })
        .collect();
    let best = a.best_move.clone().or_else(|| top.pv.first().cloned());
    let c = db.conn().await?;
    c.execute(
        &db.sql(
            "UPDATE chess.positions SET best = $2, score_cp = $3, depth = $4, engine = $5, moves = $6,
                    eval_lease_until = NULL, updated_at = now()
              WHERE key = $1",
        ),
        &[
            &key,
            &best,
            &top.score.to_cp(MATE_CP),
            &(top.depth as i32),
            &engine.name(),
            &serde_json::Value::from(moves),
        ],
    )
    .await?;
    info!(position = key, moves = legal, "dictionary position evaluated");
    Ok(true)
}

pub async fn run(
    db: Db,
    metrics: Metrics,
    cfg: AnalyseConfig,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let (screening, configured) = cfg.model()?;
    if !configured {
        warn!(
            "no CHESS_FAIRPLAY_MODEL: screening with the untuned example model; flags are only illustrative"
        );
    }
    let mut engine = spawn_engine(&cfg).await?;
    loop {
        if *stop.borrow() {
            break;
        }
        let id = match claim(&db).await {
            Ok(id) => id,
            Err(e) => {
                warn!("fair-play queue: {e:#}");
                None
            }
        };
        let Some(id) = id else {
            // Nothing to screen: a photo position for the dictionary.
            match evaluate_queued(&db, &mut engine, &cfg).await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(e) => {
                    warn!("dictionary position: {e:#}");
                    engine = spawn_engine(&cfg).await?;
                }
            }
            if cfg.once {
                break;
            }
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(5)) => {}
                () = crate::stopped(&mut stop) => break,
            }
            continue;
        };
        let started = std::time::Instant::now();
        match screen(&db, &metrics, &mut engine, &cfg, &screening, &id).await {
            Ok(()) => {
                set_state(&db, &id, "done").await?;
                info!(game = id, secs = started.elapsed().as_secs(), "game screened");
            }
            Err(e) => {
                warn!(game = id, "screening failed: {e:#}");
                set_state(&db, &id, "failed").await?;
                // The engine may be the problem: start a fresh one.
                engine = spawn_engine(&cfg).await?;
            }
        }
        if cfg.once {
            break;
        }
    }
    engine.quit().await;
    Ok(())
}
