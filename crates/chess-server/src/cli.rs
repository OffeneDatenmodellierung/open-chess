//! The server's commands, as the `open-chess` program runs them (the
//! `open-chess` crate; built with its `server` feature):
//!
//!   open-chess serve      the Lichess-compatible API
//!   open-chess analyse    the fair-play worker (needs Stockfish)
//!   open-chess migrate    create or update the chess schema, then exit
//!   open-chess bot create --name esl-stockfish-1800   a bot account and its token
//!   open-chess healthcheck   for container health checks

use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tokio::sync::watch;
use tracing::info;

use crate::app::{self, Ctx};
use crate::config::{AnalyseConfig, Config};
use crate::db::Db;
use crate::{analyse, auth, stopped};

/// `open-chess` with no subcommand: serve, configured from the environment.
#[derive(Parser)]
struct ServeOnly {
    #[command(flatten)]
    cfg: Config,
}

/// The server's commands.
#[derive(Subcommand)]
pub enum Cmd {
    /// Serve the API (and sweep clocks and challenges).
    Serve(Config),
    /// Screen finished games for fair play with Stockfish.
    Analyse {
        #[command(flatten)]
        cfg: Config,
        #[command(flatten)]
        analyse: AnalyseConfig,
    },
    /// Create or update the chess schema and exit.
    Migrate(Config),
    /// Board photos kept for review.
    Photos {
        #[command(subcommand)]
        cmd: PhotosCmd,
    },
    /// Bot accounts.
    Bot {
        #[command(subcommand)]
        cmd: BotCmd,
    },
    /// Exit 0 if the local server answers /healthz.
    Healthcheck {
        #[arg(long, env = "CHESS_METRICS_BIND", default_value = "0.0.0.0:9465")]
        metrics_bind: String,
    },
}

#[derive(Subcommand)]
pub enum BotCmd {
    /// Make a bot: a site player, its bot account here and an API token
    /// (printed once).
    Create {
        #[command(flatten)]
        cfg: Config,
        /// Username, e.g. esl-stockfish-1800.
        #[arg(long)]
        name: String,
        /// Token lifetime.
        #[arg(long, default_value_t = 365)]
        days: i64,
    },
    /// List bot accounts.
    List(Config),
}

#[derive(Subcommand)]
pub enum PhotosCmd {
    /// Write the labelled photos as a training set for chess-vision-train:
    /// `photos/<id>.<ext>` and `labels.jsonl` (datasets `open-chess-photos`,
    /// or with `--public-only` `open-chess-public`, in its datasets.toml).
    /// Only photos kept under the photo licence; each line says its licence.
    Export {
        #[command(flatten)]
        cfg: Config,
        /// Directory to write (created; existing photos are kept).
        #[arg(long)]
        out: std::path::PathBuf,
        /// Only photos two people agreed on, or an admin labelled, not ones
        /// only the player who sent them confirmed.
        #[arg(long)]
        reviewed_only: bool,
        /// Only photos their senders released as CC0 (the public dataset).
        #[arg(long)]
        public_only: bool,
    },
}

fn tracing_init() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).with_target(false).init();
}

/// Ctrl-C or SIGTERM flips the returned watch to true.
fn shutdown_signal() -> watch::Receiver<bool> {
    let (tx, rx) = watch::channel(false);
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler");
            tokio::select! {
                _ = ctrl_c => {}
                _ = term.recv() => {}
            }
        }
        #[cfg(not(unix))]
        let _ = ctrl_c.await;
        info!("shutting down");
        let _ = tx.send(true);
    });
    rx
}

/// Whether the environment configures a server (`DATABASE_URL` is set):
/// `open-chess` with no command serves then.
pub fn configured() -> bool {
    std::env::var_os("DATABASE_URL").is_some_and(|v| !v.is_empty())
}

/// Run a server command; `None` serves, configured from the environment.
pub fn run(cmd: Option<Cmd>) -> Result<()> {
    tracing_init();
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(dispatch(cmd))
}

async fn dispatch(cmd: Option<Cmd>) -> Result<()> {
    let cmd = match cmd {
        Some(c) => c,
        None => {
            Cmd::Serve(ServeOnly::try_parse_from(["open-chess"]).context("DATABASE_URL must be set")?.cfg)
        }
    };
    match cmd {
        Cmd::Serve(cfg) => serve(cfg).await,
        Cmd::Analyse { cfg, analyse: a } => {
            cfg.validate()?;
            let db = Db::connect(&cfg)?;
            db.migrate().await?;
            let ctx = Ctx::new(db.clone(), cfg.clone());
            let stop = shutdown_signal();
            let ops = tokio::net::TcpListener::bind(&cfg.metrics_bind)
                .await
                .context("binding CHESS_METRICS_BIND")?;
            let stop_ops = stop.clone();
            // The worker counts into the registry /metrics serves.
            let metrics = ctx.metrics.clone();
            tokio::spawn(async move {
                let _ = axum::serve(ops, app::ops_router(ctx)).with_graceful_shutdown(wait(stop_ops)).await;
            });
            analyse::run(db, metrics, a, stop).await
        }
        Cmd::Migrate(cfg) => {
            cfg.validate()?;
            Db::connect(&cfg)?.migrate().await?;
            info!("chess schema up to date");
            Ok(())
        }
        Cmd::Bot { cmd: BotCmd::Create { cfg, name, days } } => bot_create(cfg, &name, days).await,
        Cmd::Bot { cmd: BotCmd::List(cfg) } => bot_list(cfg).await,
        Cmd::Photos { cmd: PhotosCmd::Export { cfg, out, reviewed_only, public_only } } => {
            photos_export(cfg, &out, reviewed_only, public_only).await
        }
        Cmd::Healthcheck { metrics_bind } => healthcheck(&metrics_bind),
    }
}

async fn wait(mut stop: watch::Receiver<bool>) {
    stopped(&mut stop).await;
}

async fn serve(cfg: Config) -> Result<()> {
    cfg.validate()?;
    let db = Db::connect(&cfg)?;
    db.migrate().await?;
    let ctx = Ctx::new(db.clone(), cfg.clone()).with_reader()?;
    let stop = shutdown_signal();

    crate::spawn_background(&ctx, &stop);

    let ops = tokio::net::TcpListener::bind(&cfg.metrics_bind).await.context("binding CHESS_METRICS_BIND")?;
    let ops_ctx = ctx.clone();
    let stop_ops = stop.clone();
    tokio::spawn(async move {
        let _ = axum::serve(ops, app::ops_router(ops_ctx)).with_graceful_shutdown(wait(stop_ops)).await;
    });

    let listener = tokio::net::TcpListener::bind(&cfg.bind).await.context("binding CHESS_BIND")?;
    info!(bind = cfg.bind, public = cfg.public_url(), "chess server listening");
    if cfg.esports_login && cfg.esports().is_none() {
        tracing::warn!(
            "ESPORTS_LOGIN is on but ESPORTS_SECRET isn't set (32+ characters): signing in with the site is off"
        );
    }
    axum::serve(listener, app::router(&ctx)?).with_graceful_shutdown(wait(stop)).await?;
    Ok(())
}

async fn bot_create(cfg: Config, name: &str, days: i64) -> Result<()> {
    cfg.validate()?;
    let db = Db::connect(&cfg)?;
    db.migrate().await?;
    let mut c = db.conn().await?;
    let tx = c.transaction().await?;
    let player: i64 = tx
        .query_one(
            &db.sql("INSERT INTO chess.players (name, key_hash) VALUES ($1, $2) RETURNING id"),
            &[&name, &auth::hash_key(&auth::random_token(28))],
        )
        .await?
        .get(0);
    tx.execute(
        &db.sql("INSERT INTO chess.accounts (player_id, username, kind) VALUES ($1, $2, 'bot')"),
        &[&player, &name],
    )
    .await
    .context("that username is taken or invalid (letters, digits, _ and -, 2–30 characters)")?;
    let token = format!("{}{}", auth::TOKEN_PREFIX, auth::random_token(36));
    let prefix: String = token.chars().take(auth::TOKEN_PREFIX.len() + 6).collect();
    let scopes: Vec<String> =
        ["bot:play", "challenge:read", "challenge:write"].iter().map(|s| (*s).to_owned()).collect();
    let expires = chrono::Utc::now() + chrono::Duration::days(days.clamp(1, 365));
    tx.execute(
        &db.sql(
            "INSERT INTO chess.api_tokens (player_id, name, token_hash, prefix, scopes, expires_at)
             VALUES ($1, 'chess bot', $2, $3, $4, $5)",
        ),
        &[&player, &auth::hash_key(&token), &prefix, &scopes, &expires],
    )
    .await?;
    tx.commit().await?;
    println!("bot {name} created (player {player}); its token, shown once:\n{token}");
    Ok(())
}

async fn bot_list(cfg: Config) -> Result<()> {
    cfg.validate()?;
    let db = Db::connect(&cfg)?;
    let c = db.conn().await?;
    for r in c
        .query(
            &db.sql("SELECT username, player_id, seen_at FROM chess.accounts WHERE kind = 'bot' ORDER BY lower(username)"),
            &[],
        )
        .await?
    {
        let seen: Option<chrono::DateTime<chrono::Utc>> = r.get(2);
        println!(
            "{:<30} player {:<6} last seen {}",
            r.get::<_, String>(0),
            r.get::<_, i64>(1),
            seen.map_or_else(|| "never".into(), |t| t.to_rfc3339())
        );
    }
    Ok(())
}

async fn photos_export(
    cfg: Config,
    out: &std::path::Path,
    reviewed_only: bool,
    public_only: bool,
) -> Result<()> {
    cfg.validate()?;
    let db = Db::connect(&cfg)?;
    let c = db.conn().await?;
    std::fs::create_dir_all(out.join("photos")).with_context(|| format!("creating {}", out.display()))?;
    let rows = c
        .query(
            &db.sql(
                "SELECT id, player_id, created_at, image, mime, label, label_source, public
                   FROM chess.board_photos
                  WHERE label IS NOT NULL AND NOT unreadable AND terms IS NOT NULL
                    AND ($1 = false OR label_source IN ('consensus', 'admin', 'reviewer'))
                    AND ($2 = false OR public)
                  ORDER BY id",
            ),
            &[&reviewed_only, &public_only],
        )
        .await?;
    let mut lines = String::new();
    for r in &rows {
        let id: i64 = r.get(0);
        let player: Option<i64> = r.get(1);
        let at: chrono::DateTime<chrono::Utc> = r.get(2);
        let ext = match r.get::<_, String>(4).as_str() {
            "image/png" => "png",
            "image/webp" => "webp",
            _ => "jpg",
        };
        let name = format!("photos/{id}.{ext}");
        std::fs::write(out.join(&name), r.get::<_, Vec<u8>>(3))?;
        // One player's photos on one day are probably of the same board, so
        // they share a group and a split never divides them.
        let group = format!("player-{}-{}", player.unwrap_or(0), at.format("%Y-%m-%d"));
        lines.push_str(&serde_json::to_string(&serde_json::json!({
            "image": name, "placement": r.get::<_, String>(5), "group": group, "source": r.get::<_, Option<String>>(6),
            "licence": if r.get::<_, bool>(7) { "CC0-1.0" } else { PHOTO_LICENCE },
        }))?);
        lines.push('\n');
    }
    std::fs::write(out.join("labels.jsonl"), lines)?;
    info!(photos = rows.len(), out = %out.display(), "exported");
    Ok(())
}

/// Photos kept under the photo licence (docs/photo-licence.md), as the
/// trainer's datasets.toml names it.
const PHOTO_LICENCE: &str = "LicenseRef-open-chess-photo-licence-1";

fn healthcheck(bind: &str) -> Result<()> {
    use std::io::{Read, Write};
    let port = bind.rsplit(':').next().unwrap_or("9465");
    let mut s = std::net::TcpStream::connect(format!("127.0.0.1:{port}")).context("connecting")?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
    let mut buf = String::new();
    s.read_to_string(&mut buf)?;
    if buf.starts_with("HTTP/1.0 200") || buf.starts_with("HTTP/1.1 200") {
        Ok(())
    } else {
        anyhow::bail!("unhealthy: {}", buf.lines().next().unwrap_or(""))
    }
}
