//! Postgres: the connection pool, this server's schema, and the LISTEN
//! connection that fans changes out to every replica.
//!
//! SQL is written with `chess.` table prefixes; [`Db::sql`] swaps in the
//! configured schema name. The server keeps its own players, API tokens
//! and sessions there; a host site's (`site.`, `CHESS_SITE_SCHEMA`) are read
//! once, to import the players who already used chess there. Explicit names rather than a
//! `search_path` because Neon's pooler (transaction pooling) doesn't keep
//! session settings.

use std::fmt::Write as _;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use deadpool::managed::{self, Metrics, RecycleError, RecycleResult};
use futures_util::StreamExt;
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::config::Config;

pub fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn tls() -> Result<tokio_postgres_rustls::MakeRustlsConnect> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    let (added, _) = roots.add_parsable_certificates(native.certs);
    if added == 0 {
        bail!("no trusted root certificates found for TLS");
    }
    let cfg = rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth();
    Ok(tokio_postgres_rustls::MakeRustlsConnect::new(cfg))
}

/// Neon's pooled hostname (`ep-x-123-pooler.region…`) → the direct one,
/// which LISTEN needs (transaction poolers don't deliver notifications).
pub fn direct_host(host: &str) -> Option<String> {
    let (first, rest) = host.split_once('.')?;
    first.strip_suffix("-pooler").map(|ep| format!("{ep}.{rest}"))
}

pub struct Manager {
    pg: tokio_postgres::Config,
    tls: tokio_postgres_rustls::MakeRustlsConnect,
}

impl managed::Manager for Manager {
    type Type = tokio_postgres::Client;
    type Error = tokio_postgres::Error;

    async fn create(&self) -> Result<tokio_postgres::Client, tokio_postgres::Error> {
        let (client, conn) = self.pg.connect(self.tls.clone()).await?;
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                warn!("postgres connection: {e}");
            }
        });
        Ok(client)
    }

    async fn recycle(
        &self,
        client: &mut tokio_postgres::Client,
        _: &Metrics,
    ) -> RecycleResult<tokio_postgres::Error> {
        if client.is_closed() {
            return Err(RecycleError::message("connection closed"));
        }
        Ok(())
    }
}

pub type Pool = managed::Pool<Manager>;
pub type Conn = managed::Object<Manager>;

/// A change other replicas (and local streams) need to hear about.
#[derive(Debug, Clone)]
pub enum Note {
    /// A game's state changed: reload it.
    Game(String),
    /// A chat line in a game.
    Chat { game: String, line: serde_json::Value },
    /// An event for one user's event stream (already in its final JSON).
    User { player: i64, event: serde_json::Value },
    /// The LISTEN connection dropped and came back: notes may have been
    /// missed, so every stream reloads.
    Resync,
}

#[derive(Clone)]
pub struct Db {
    pub pool: Pool,
    schema: Arc<str>,
    site: Arc<str>,
    channel: Arc<str>,
    notes: broadcast::Sender<Note>,
}

impl Db {
    pub fn connect(cfg: &Config) -> Result<Db> {
        let pg = tokio_postgres::Config::from_str(&cfg.database_url)
            .context("DATABASE_URL isn't a postgres:// URL")?;
        let pool = Pool::builder(Manager { pg, tls: tls()? })
            .max_size(cfg.pool_size.max(2))
            .wait_timeout(Some(Duration::from_secs(10)))
            .create_timeout(Some(Duration::from_secs(15)))
            .runtime(deadpool::Runtime::Tokio1)
            .build()
            .context("building the database pool")?;
        let (notes, _) = broadcast::channel(4096);
        Ok(Db {
            pool,
            schema: cfg.schema.as_str().into(),
            site: cfg.site_schema.as_str().into(),
            channel: format!("{}_events", cfg.schema).into(),
            notes,
        })
    }

    /// Swap `chess.` and `site.` table prefixes for the configured schemas
    /// (only where a prefix starts a word, so prose in comments is safe).
    pub fn sql(&self, q: &str) -> String {
        let c = format!("{}.", quote_ident(&self.schema));
        let s = format!("{}.", quote_ident(&self.site));
        prefix_replace(&prefix_replace(q, "chess.", &c), "site.", &s)
    }

    pub async fn conn(&self) -> Result<Conn> {
        self.pool.get().await.map_err(|e| anyhow::anyhow!("database unavailable: {e}"))
    }

    pub fn channel(&self) -> &str {
        &self.channel
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Note> {
        self.notes.subscribe()
    }

    pub fn encode(n: &Note) -> String {
        let v = match n {
            Note::Game(id) => serde_json::json!({"t": "g", "g": id}),
            Note::Chat { game, line } => serde_json::json!({"t": "c", "g": game, "l": line}),
            Note::User { player, event } => serde_json::json!({"t": "u", "u": player, "e": event}),
            Note::Resync => serde_json::json!({"t": "r"}),
        };
        v.to_string()
    }

    pub fn decode(s: &str) -> Option<Note> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        match v.get("t")?.as_str()? {
            "g" => Some(Note::Game(v.get("g")?.as_str()?.to_owned())),
            "c" => Some(Note::Chat { game: v.get("g")?.as_str()?.to_owned(), line: v.get("l")?.clone() }),
            "u" => Some(Note::User { player: v.get("u")?.as_i64()?, event: v.get("e")?.clone() }),
            "r" => Some(Note::Resync),
            _ => None,
        }
    }

    /// Run migrations: this server's schema and tables, then (once) the
    /// import of a host site's chess players ([`Db::import_site`]).
    pub async fn migrate(&self) -> Result<()> {
        let mut c = self.conn().await?;
        let tx = c.transaction().await?;
        tx.batch_execute(&format!(
            "SELECT pg_advisory_xact_lock(hashtext('open-chess-migrate'));
             CREATE SCHEMA IF NOT EXISTS {s};
             CREATE TABLE IF NOT EXISTS {s}.schema_version (version integer NOT NULL);",
            s = quote_ident(&self.schema)
        ))
        .await?;
        // Players, tokens and sessions first: the migrations refer to them.
        tx.batch_execute(&self.sql(IDENTITY)).await.context("chess identity tables")?;
        let current: i32 = tx
            .query_opt(&self.sql("SELECT version FROM chess.schema_version"), &[])
            .await?
            .map_or(0, |r| r.get(0));
        for (i, m) in MIGRATIONS.iter().enumerate() {
            let v = i32::try_from(i + 1).unwrap_or(i32::MAX);
            if v > current {
                tx.batch_execute(&self.sql(m))
                    .await
                    .with_context(|| format!("chess schema migration {v}"))?;
                info!(version = v, "chess schema migrated");
            }
        }
        let latest = i32::try_from(MIGRATIONS.len()).unwrap_or(i32::MAX);
        if current == 0 {
            tx.execute(&self.sql("INSERT INTO chess.schema_version VALUES ($1)"), &[&latest]).await?;
        } else if latest > current {
            tx.execute(&self.sql("UPDATE chess.schema_version SET version = $1"), &[&latest]).await?;
        }
        self.import_site(&tx).await.context("importing the site's chess players")?;
        tx.commit().await?;
        Ok(())
    }

    /// Once: copy the host site's players who use chess (their roles stay
    /// the site's; the site vouches for their emails) (they have a chess
    /// account, or are named in any of our tables, or hold a token with a
    /// chess scope) with their ids, and their unexpired chess tokens, into
    /// our own tables, linked to the site by `esports_id`; then point our
    /// tables' references at our players. A site without those tables (a
    /// stand-alone server) has nothing to import.
    async fn import_site(&self, tx: &tokio_postgres::Transaction<'_>) -> Result<()> {
        if *self.site == *self.schema {
            return Ok(());
        }
        let has = |t: &'static str| {
            let site = self.site.clone();
            async move {
                tx.query_one(
                    "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_schema = $1 AND table_name = $2)",
                    &[&&*site, &t],
                )
                .await
                .map(|r| r.get::<_, bool>(0))
            }
        };
        if !(has("players").await? && has("api_tokens").await?) {
            return Ok(());
        }
        let done = tx
            .query_opt(&self.sql("SELECT 1 FROM chess.imports WHERE source = $1"), &[&&*self.site])
            .await?
            .is_some();
        if done {
            return Ok(());
        }
        let site_players = format!("{}.players", quote_ident(&self.site));
        // Our foreign keys still pointing at the site's players.
        let fks = tx
            .query(
                "SELECT c.conname, c.conrelid::regclass::text, a.attname, pg_get_constraintdef(c.oid)
                   FROM pg_constraint c
                   JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = c.conkey[1]
                  WHERE c.contype = 'f' AND c.confrelid = to_regclass($1)
                    AND c.connamespace = (SELECT oid FROM pg_namespace WHERE nspname = $2)",
                &[&site_players, &&*self.schema],
            )
            .await?;
        let scopes: Vec<String> = crate::auth::SCOPES.iter().map(|(s, _)| (*s).to_owned()).collect();
        let mut named = String::from("SELECT player_id FROM site.api_tokens WHERE scopes && $1::text[]");
        for f in &fks {
            let (table, column): (String, String) = (f.get(1), f.get(2));
            let _ = write!(named, " UNION SELECT {} FROM {table}", quote_ident(&column));
        }
        let players = tx
            .execute(
                &self.sql(&format!(
                    "INSERT INTO chess.players (id, name, email, email_verified_at, key_hash, disabled_at, esports_id)
                     SELECT p.id, p.name, p.email, CASE WHEN p.email IS NULL THEN NULL ELSE now() END, p.key_hash,
                            p.disabled_at, p.id::text
                       FROM site.players p WHERE p.id IN ({named})
                     ON CONFLICT (id) DO NOTHING"
                )),
                &[&scopes],
            )
            .await?;
        let tokens = tx
            .execute(
                &self.sql(
                    "INSERT INTO chess.api_tokens (player_id, name, token_hash, prefix, scopes, created_at,
                                                   expires_at, last_used_at, revoked_at, revoked_by)
                     SELECT t.player_id, t.name, t.token_hash, t.prefix,
                            ARRAY(SELECT unnest(t.scopes) INTERSECT SELECT unnest($1::text[])),
                            t.created_at, t.expires_at, t.last_used_at, t.revoked_at, t.revoked_by
                       FROM site.api_tokens t
                      WHERE t.player_id IN (SELECT id FROM chess.players) AND t.scopes && $1::text[]
                        AND t.revoked_at IS NULL AND t.expires_at > now()
                     ON CONFLICT (token_hash) DO NOTHING",
                ),
                &[&scopes],
            )
            .await?;
        tx.execute(
            &self.sql(
                "SELECT setval(pg_get_serial_sequence($1, 'id'), GREATEST((SELECT max(id) FROM chess.players), 1))",
            ),
            &[&self.sql("chess.players")],
        )
        .await?;
        let ours = self.sql("chess.players");
        for f in &fks {
            let (name, table, column, def): (String, String, String, String) =
                (f.get(0), f.get(1), f.get(2), f.get(3));
            let tail = def.split_once(".players(id)").map_or("", |(_, t)| t);
            tx.batch_execute(&format!(
                "ALTER TABLE {table} DROP CONSTRAINT {n};
                 ALTER TABLE {table} ADD CONSTRAINT {n} FOREIGN KEY ({c}) REFERENCES {ours}(id){tail};",
                n = quote_ident(&name),
                c = quote_ident(&column),
            ))
            .await?;
        }
        tx.execute(&self.sql("INSERT INTO chess.imports (source) VALUES ($1)"), &[&&*self.site]).await?;
        info!(
            players,
            tokens,
            references = fks.len(),
            site = &*self.site,
            "imported the site's chess players"
        );
        Ok(())
    }

    /// Keep a LISTEN connection open and forward notifications into the
    /// local broadcast. Reconnects with backoff; after a reconnect every
    /// stream is told to resync.
    pub async fn listen(self, cfg: Config, mut stop: tokio::sync::watch::Receiver<bool>) {
        let mut backoff = Duration::from_millis(500);
        let mut first = true;
        loop {
            let started = std::time::Instant::now();
            match self.listen_once(&cfg, first).await {
                Ok(()) => {}
                Err(e) => warn!("chess LISTEN connection: {e:#}"),
            }
            first = false;
            if started.elapsed() > Duration::from_secs(30) {
                backoff = Duration::from_millis(500);
            }
            tokio::select! {
                () = tokio::time::sleep(backoff) => {}
                () = crate::stopped(&mut stop) => return,
            }
            backoff = (backoff * 2).min(Duration::from_secs(10));
        }
    }

    async fn listen_once(&self, cfg: &Config, first: bool) -> Result<()> {
        let url = match &cfg.listen_database_url {
            Some(u) => u.clone(),
            None => listen_url(&cfg.database_url),
        };
        let pg = tokio_postgres::Config::from_str(&url).context("the LISTEN database URL")?;
        let (client, mut conn) = pg.connect(tls()?).await?;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let driver = tokio::spawn(async move {
            let mut s = futures_util::stream::poll_fn(move |cx| conn.poll_message(cx));
            while let Some(m) = s.next().await {
                match m {
                    Ok(tokio_postgres::AsyncMessage::Notification(n)) => {
                        if tx.send(n.payload().to_owned()).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("chess LISTEN: {e}");
                        break;
                    }
                }
            }
        });
        client.batch_execute(&format!("LISTEN {}", quote_ident(&self.channel))).await?;
        info!(channel = &*self.channel, "listening for chess changes");
        if !first {
            let _ = self.notes.send(Note::Resync);
        }
        let mut ping = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                m = rx.recv() => match m {
                    Some(p) => {
                        if let Some(n) = Db::decode(&p) {
                            let _ = self.notes.send(n);
                        }
                    }
                    None => break,
                },
                _ = ping.tick() => {
                    if client.simple_query("SELECT 1").await.is_err() {
                        break;
                    }
                }
            }
        }
        driver.abort();
        bail!("the LISTEN connection closed")
    }
}

/// The URL for LISTEN: a Neon `-pooler` host swapped for the direct one
/// (the host is replaced, not added: tokio-postgres would try a second
/// host only if the first failed, and the pooler doesn't fail, it just
/// never delivers notifications). Anything else is used as given.
pub fn listen_url(database_url: &str) -> String {
    let Ok(mut u) = url::Url::parse(database_url) else { return database_url.to_owned() };
    let Some(direct) = u.host_str().and_then(direct_host) else { return database_url.to_owned() };
    if u.set_host(Some(&direct)).is_err() {
        return database_url.to_owned();
    }
    u.into()
}

/// Replace `from` with `to` wherever it starts a word and is followed by a
/// letter (a table name).
fn prefix_replace(q: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(q.len() + 32);
    let mut rest = q;
    let mut prev: Option<char> = None;
    while let Some(i) = rest.find(from) {
        let before = rest[..i].chars().next_back().or(prev);
        let after = rest[i + from.len()..].chars().next();
        out.push_str(&rest[..i]);
        let starts_word = before.is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '"' || c == '.'));
        if starts_word && after.is_some_and(|c| c.is_ascii_alphabetic()) {
            out.push_str(to);
        } else {
            out.push_str(from);
        }
        prev = from.chars().next_back();
        rest = &rest[i + from.len()..];
    }
    out.push_str(rest);
    out
}

/// Send `note` from inside a transaction (delivered to every replica on
/// commit). Payloads over Postgres's 8000-byte limit are trimmed to a
/// reload instruction.
pub async fn notify(tx: &impl tokio_postgres::GenericClient, db: &Db, note: &Note) -> Result<()> {
    let mut payload = Db::encode(note);
    if payload.len() > 7900 {
        payload = match note {
            Note::Game(id) | Note::Chat { game: id, .. } => Db::encode(&Note::Game(id.clone())),
            _ => Db::encode(&Note::Resync),
        };
    }
    // Postgres delivers on commit, to every replica listening.
    tx.execute("SELECT pg_notify($1, $2)", &[&db.channel(), &payload]).await?;
    Ok(())
}

/// The server's own players, API tokens and sessions (created before the
/// migrations, which refer to them; idempotent). `esports_id` links a
/// player to the host site's (signed in with it, or imported from it).
const IDENTITY: &str = r"
CREATE TABLE IF NOT EXISTS chess.players (
    id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
    name text NOT NULL,
    email text,
    key_hash text,
    password_hash text,
    role text NOT NULL DEFAULT 'player',
    roles text[] NOT NULL DEFAULT '{}',
    esports_id text UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    disabled_at timestamptz);
CREATE TABLE IF NOT EXISTS chess.api_tokens (
    id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
    player_id bigint NOT NULL REFERENCES chess.players ON DELETE CASCADE,
    name text NOT NULL,
    token_hash text NOT NULL UNIQUE,
    prefix text NOT NULL,
    scopes text[] NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    last_used_at timestamptz,
    revoked_at timestamptz,
    revoked_by text);
CREATE INDEX IF NOT EXISTS api_tokens_player_idx ON chess.api_tokens (player_id);
CREATE TABLE IF NOT EXISTS chess.sessions (
    id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
    token_hash text NOT NULL UNIQUE,
    player_id bigint NOT NULL REFERENCES chess.players ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz);
CREATE TABLE IF NOT EXISTS chess.imports (
    source text PRIMARY KEY,
    at timestamptz NOT NULL DEFAULT now());
";

const MIGRATIONS: &[&str] = &[
    r"
-- One row per site player who has used the chess server. `username` is
-- their Lichess-style handle here; `kind` is human or bot (bots play
-- through the Bot API and are never fair-play screened); `marked_at` is a
-- confirmed fair-play violation (shown as tosViolation).
CREATE TABLE chess.accounts (
    player_id bigint PRIMARY KEY REFERENCES chess.players ON DELETE CASCADE,
    username text NOT NULL,
    kind text NOT NULL DEFAULT 'human' CHECK (kind IN ('human', 'bot')),
    created_at timestamptz NOT NULL DEFAULT now(),
    seen_at timestamptz,
    marked_at timestamptz,
    marked_by bigint,
    CHECK (username ~ '^[A-Za-z0-9][A-Za-z0-9_-]{1,29}$'));
CREATE UNIQUE INDEX accounts_username_idx ON chess.accounts (lower(username));

-- Glicko-2 per rating pool (bullet, blitz, …).
CREATE TABLE chess.ratings (
    player_id bigint NOT NULL REFERENCES chess.accounts ON DELETE CASCADE,
    perf text NOT NULL,
    rating double precision NOT NULL,
    deviation double precision NOT NULL,
    volatility double precision NOT NULL,
    games integer NOT NULL DEFAULT 0,
    prog integer NOT NULL DEFAULT 0,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (player_id, perf));

CREATE TABLE chess.challenges (
    id text PRIMARY KEY,
    challenger_id bigint NOT NULL REFERENCES chess.accounts ON DELETE CASCADE,
    dest_id bigint NOT NULL REFERENCES chess.accounts ON DELETE CASCADE,
    status text NOT NULL DEFAULT 'created'
        CHECK (status IN ('created', 'offline', 'canceled', 'declined', 'accepted')),
    variant text NOT NULL DEFAULT 'standard' CHECK (variant IN ('standard', 'fromPosition')),
    initial_fen text,
    rated boolean NOT NULL DEFAULT false,
    clock_limit integer,
    clock_increment integer,
    color text NOT NULL DEFAULT 'random' CHECK (color IN ('white', 'black', 'random')),
    final_color text NOT NULL CHECK (final_color IN ('white', 'black')),
    rules text[] NOT NULL DEFAULT '{}',
    decline_reason text,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL);
CREATE INDEX challenges_dest_idx ON chess.challenges (dest_id) WHERE status = 'created';
CREATE INDEX challenges_challenger_idx ON chess.challenges (challenger_id) WHERE status = 'created';
CREATE INDEX challenges_expiry_idx ON chess.challenges (expires_at) WHERE status = 'created';

-- Games. `moves` is UCI (standard castling), `turn_started_at` is when the
-- side to move's clock started (the database clock is the only clock, so
-- every replica agrees). move_ms / clock_ms: time used and time left per
-- ply, for exports and fair-play timing.
CREATE TABLE chess.games (
    id text PRIMARY KEY,
    white_id bigint NOT NULL REFERENCES chess.accounts,
    black_id bigint NOT NULL REFERENCES chess.accounts,
    variant text NOT NULL DEFAULT 'standard',
    initial_fen text,
    rated boolean NOT NULL,
    source text NOT NULL DEFAULT 'friend',
    speed text NOT NULL,
    rules text[] NOT NULL DEFAULT '{}',
    clock_initial_ms bigint,
    clock_increment_ms bigint,
    status smallint NOT NULL DEFAULT 20,
    winner text CHECK (winner IN ('white', 'black')),
    moves text NOT NULL DEFAULT '',
    ply integer NOT NULL DEFAULT 0,
    fen text NOT NULL,
    white_ms bigint,
    black_ms bigint,
    move_ms bigint[] NOT NULL DEFAULT '{}',
    clock_ms bigint[] NOT NULL DEFAULT '{}',
    draw_offer text CHECK (draw_offer IN ('white', 'black')),
    takeback_offer text CHECK (takeback_offer IN ('white', 'black')),
    white_rating integer NOT NULL,
    black_rating integer NOT NULL,
    white_provisional boolean NOT NULL DEFAULT false,
    black_provisional boolean NOT NULL DEFAULT false,
    white_rating_diff integer,
    black_rating_diff integer,
    created_at timestamptz NOT NULL DEFAULT now(),
    turn_started_at timestamptz NOT NULL DEFAULT now(),
    last_move_at timestamptz,
    finished_at timestamptz,
    analysis text NOT NULL DEFAULT 'none'
        CHECK (analysis IN ('none', 'pending', 'running', 'done', 'failed', 'skipped')),
    analysis_lease_until timestamptz,
    CHECK (white_id <> black_id));
CREATE INDEX games_ongoing_idx ON chess.games (turn_started_at) WHERE status IN (10, 20);
CREATE INDEX games_white_idx ON chess.games (white_id, created_at DESC);
CREATE INDEX games_black_idx ON chess.games (black_id, created_at DESC);
CREATE INDEX games_analysis_idx ON chess.games (finished_at) WHERE analysis IN ('pending', 'running');

CREATE TABLE chess.chat (
    id bigserial PRIMARY KEY,
    game_id text NOT NULL REFERENCES chess.games ON DELETE CASCADE,
    room text NOT NULL CHECK (room IN ('player', 'spectator')),
    player_id bigint,
    username text NOT NULL,
    text text NOT NULL CHECK (length(text) <= 140),
    at timestamptz NOT NULL DEFAULT now());
CREATE INDEX chat_game_idx ON chess.chat (game_id, id);

-- Who has a game stream open (heartbeats; nothing here needs to survive
-- a restart, hence unlogged).
CREATE UNLOGGED TABLE chess.presence (
    game_id text NOT NULL,
    player_id bigint NOT NULL,
    seen_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (game_id, player_id));

-- OAuth authorization codes (PKCE), single use, two minutes.
CREATE TABLE chess.oauth_codes (
    code_hash text PRIMARY KEY,
    player_id bigint NOT NULL REFERENCES chess.players ON DELETE CASCADE,
    client_id text NOT NULL,
    redirect_uri text NOT NULL,
    code_challenge text NOT NULL,
    scopes text[] NOT NULL,
    expires_at timestamptz NOT NULL,
    used_at timestamptz);

-- Fair play: one report per screened side of a game, and reviewers'
-- verdicts.
CREATE TABLE chess.fairplay_reports (
    game_id text NOT NULL REFERENCES chess.games ON DELETE CASCADE,
    side text NOT NULL CHECK (side IN ('white', 'black')),
    player_id bigint NOT NULL REFERENCES chess.accounts ON DELETE CASCADE,
    score real NOT NULL,
    flagged boolean NOT NULL,
    moves integer NOT NULL,
    accuracy real NOT NULL,
    acpl real NOT NULL,
    top1 real NOT NULL,
    top3 real NOT NULL,
    time_cv real,
    time_corr real,
    reasons text[] NOT NULL DEFAULT '{}',
    engine text NOT NULL,
    depth integer NOT NULL,
    evals jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (game_id, side));
CREATE INDEX fairplay_flagged_idx ON chess.fairplay_reports (created_at DESC) WHERE flagged;
CREATE INDEX fairplay_player_idx ON chess.fairplay_reports (player_id, created_at DESC);
CREATE TABLE chess.fairplay_reviews (
    id bigserial PRIMARY KEY,
    player_id bigint NOT NULL REFERENCES chess.accounts ON DELETE CASCADE,
    game_id text REFERENCES chess.games ON DELETE SET NULL,
    verdict text NOT NULL CHECK (verdict IN ('clean', 'engine', 'unclear')),
    note text,
    reviewer_id bigint NOT NULL,
    at timestamptz NOT NULL DEFAULT now());
",
    r"
-- 2. The position dictionary, assistance declarations, and the training-app
-- fingerprint on fair-play reports.
--
-- positions: one row per position (FEN without the move counters), the
-- deepest engine evaluation stored for it, filled by the fair-play worker
-- as it screens games. A lookup is a key read.
CREATE TABLE chess.positions (
    key text PRIMARY KEY,
    best text,
    score_cp integer NOT NULL,
    depth integer NOT NULL,
    engine text NOT NULL,
    seen integer NOT NULL DEFAULT 1,
    updated_at timestamptz NOT NULL DEFAULT now());
-- assist_marks: a player switching the training app's engine help on or
-- off during one of our games, as declared by the app. Ground truth for
-- training the fair-play model; screening never relies on it.
CREATE TABLE chess.assist_marks (
    id bigserial PRIMARY KEY,
    game_id text NOT NULL REFERENCES chess.games ON DELETE CASCADE,
    player_id bigint NOT NULL REFERENCES chess.accounts ON DELETE CASCADE,
    ply integer NOT NULL,
    assist boolean NOT NULL,
    at timestamptz NOT NULL DEFAULT now());
CREATE INDEX assist_marks_game_idx ON chess.assist_marks (game_id, player_id);
ALTER TABLE chess.fairplay_reports
    ADD COLUMN app_disagree integer,
    ADD COLUMN app_picked integer,
    ADD COLUMN app_picked_reference integer,
    ADD COLUMN declared_assist boolean NOT NULL DEFAULT false;
",
    r"
-- 3. Board photos that couldn't be read for sure, kept for review.
--
-- reading: what our board reader made of it (placement, confidence, unsure
-- squares, model); suggestion: the vision language model's reading, which
-- the Review page pre-fills; label: the position a person confirmed (FEN
-- placement, White's side), by the player who sent it in the app or by a
-- reviewer on the Review page. Labelled photos are training data.
CREATE TABLE chess.board_photos (
    id bigserial PRIMARY KEY,
    player_id bigint,
    created_at timestamptz NOT NULL DEFAULT now(),
    image bytea NOT NULL,
    mime text NOT NULL,
    reason text NOT NULL CHECK (reason IN ('unsure', 'implausible', 'no_model')),
    reading jsonb,
    suggestion jsonb,
    suggestion_failed text,
    label text,
    label_source text CHECK (label_source IN ('player', 'reviewer')),
    unreadable boolean NOT NULL DEFAULT false,
    labelled_by bigint,
    labelled_at timestamptz);
CREATE INDEX board_photos_open_idx ON chess.board_photos (id DESC) WHERE label IS NULL AND NOT unreadable;
",
    r"
-- 4. Endgame tablebase answers (up to seven pieces), kept once fetched:
-- the tablebase part of the position dictionary.
CREATE TABLE chess.tablebase (
    key text PRIMARY KEY,
    answer jsonb NOT NULL,
    fetched_at timestamptz NOT NULL DEFAULT now());
",
    r"
-- 5. The position dictionary grows.
--
-- positions: positions people confirmed from board photos join it before
-- anyone has evaluated them (score_cp NULL, photos counting them); the
-- fair-play worker evaluates those when it has no game to screen, scoring
-- every legal move (moves: [{uci, cp, depth}], from the side to move's
-- view). eval_lease_until: a worker is on it.
-- position_hints: analyses the training app sends in, one per position and
-- player (the deepest kept). Hints, not trusted like the worker's.
ALTER TABLE chess.positions ALTER COLUMN score_cp DROP NOT NULL;
ALTER TABLE chess.positions ALTER COLUMN depth SET DEFAULT 0;
ALTER TABLE chess.positions ALTER COLUMN engine DROP NOT NULL;
ALTER TABLE chess.positions ADD COLUMN photos integer NOT NULL DEFAULT 0;
ALTER TABLE chess.positions ADD COLUMN moves jsonb;
ALTER TABLE chess.positions ADD COLUMN eval_lease_until timestamptz;
CREATE INDEX positions_unscored_idx ON chess.positions (updated_at) WHERE score_cp IS NULL;
CREATE TABLE chess.position_hints (
    key text NOT NULL,
    player_id bigint NOT NULL,
    best text,
    score_cp integer NOT NULL,
    depth integer NOT NULL,
    engine text NOT NULL,
    moves jsonb,
    at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (key, player_id));
",
    r"
-- 6. Photo review by everyone, and the photo licence.
--
-- players.photo_terms: the photo licence the player accepted (its version),
-- and photo_public whether they also release their photos as CC0. A photo
-- is kept for review only under accepted terms, which it records.
-- photo_votes: what each reviewer says a photo shows (anyone signed in; the
-- player who sent it votes by confirming it). Two agreeing votes label it
-- ('consensus'); an admin's or manager's word is final ('admin').
ALTER TABLE chess.players ADD COLUMN IF NOT EXISTS photo_terms text;
ALTER TABLE chess.players ADD COLUMN IF NOT EXISTS photo_terms_at timestamptz;
ALTER TABLE chess.players ADD COLUMN IF NOT EXISTS photo_public boolean NOT NULL DEFAULT false;
ALTER TABLE chess.board_photos ADD COLUMN terms text;
ALTER TABLE chess.board_photos ADD COLUMN public boolean NOT NULL DEFAULT false;
ALTER TABLE chess.board_photos DROP CONSTRAINT IF EXISTS board_photos_label_source_check;
ALTER TABLE chess.board_photos ADD CONSTRAINT board_photos_label_source_check
    CHECK (label_source IN ('player', 'reviewer', 'consensus', 'admin'));
CREATE TABLE chess.photo_votes (
    photo_id bigint NOT NULL REFERENCES chess.board_photos ON DELETE CASCADE,
    player_id bigint NOT NULL,
    placement text,
    unreadable boolean NOT NULL DEFAULT false,
    voted_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (photo_id, player_id));
",
    r"
-- 7. Accounts of our own.
--
-- Roles are this server's alone now: a host site's roles no longer come
-- with its players, so everyone starts again as a player (CHESS_ADMIN_EMAILS
-- and `open-chess admin grant` make admins). email_verified_at: an email
-- someone vouched for (the host site, at sign-in), which CHESS_ADMIN_EMAILS
-- trusts. Passkeys sign people in instead of a password; an admin's or
-- manager's rights need a passkey check, recorded on the session or token
-- it was made in (staff_verified_at); a token made through OAuth never
-- inherits one (an app approved for a scope mustn't get an admin's rights). Partners are other services' API
-- access: a named token with partner scopes, made by an admin.
UPDATE chess.players SET role = 'player', roles = '{}' WHERE role <> 'player' OR roles <> '{}';
ALTER TABLE chess.players ADD COLUMN email_verified_at timestamptz;
ALTER TABLE chess.sessions ADD COLUMN staff_verified_at timestamptz;
ALTER TABLE chess.api_tokens ADD COLUMN staff_verified_at timestamptz;
CREATE TABLE chess.passkeys (
    id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
    player_id bigint NOT NULL REFERENCES chess.players ON DELETE CASCADE,
    user_handle bytea NOT NULL,
    credential_id bytea NOT NULL UNIQUE,
    public_key bytea NOT NULL,
    alg integer NOT NULL,
    sign_count bigint NOT NULL DEFAULT 0,
    name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz);
CREATE INDEX passkeys_player_idx ON chess.passkeys (player_id);
CREATE TABLE chess.webauthn_challenges (
    id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
    challenge bytea NOT NULL,
    purpose text NOT NULL CHECK (purpose IN ('register', 'signin', 'signup', 'staff')),
    player_id bigint,
    data jsonb,
    created_at timestamptz NOT NULL DEFAULT now());
CREATE TABLE chess.partners (
    id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
    name text NOT NULL,
    token_hash text NOT NULL UNIQUE,
    prefix text NOT NULL,
    scopes text[] NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    created_by bigint,
    last_used_at timestamptz,
    revoked_at timestamptz);
",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neon_direct_host() {
        assert_eq!(
            direct_host("ep-cool-1234-pooler.eu-central-1.aws.neon.tech").as_deref(),
            Some("ep-cool-1234.eu-central-1.aws.neon.tech")
        );
        assert_eq!(direct_host("localhost"), None);
        assert_eq!(direct_host("db.internal"), None);
    }

    #[test]
    fn listen_url_replaces_the_pooler_host() {
        let u = listen_url(
            "postgresql://u:p@ep-cool-1234-pooler.eu-central-1.aws.neon.tech/neondb?sslmode=require",
        );
        assert_eq!(u, "postgresql://u:p@ep-cool-1234.eu-central-1.aws.neon.tech/neondb?sslmode=require");
        let pg = tokio_postgres::Config::from_str(&u).unwrap();
        assert_eq!(pg.get_hosts().len(), 1, "one host: the direct one");
        let local = "postgres://dev:dev@127.0.0.1/chess?sslmode=disable";
        assert_eq!(listen_url(local), local);
        assert_eq!(listen_url("host=x user=y"), "host=x user=y");
    }

    #[test]
    fn notes_round_trip() {
        for n in [
            Note::Game("abcd1234".into()),
            Note::Chat { game: "g".into(), line: serde_json::json!({"type": "chatLine"}) },
            Note::User { player: 7, event: serde_json::json!({"type": "gameStart"}) },
            Note::Resync,
        ] {
            let back = Db::decode(&Db::encode(&n)).unwrap();
            assert_eq!(format!("{back:?}"), format!("{n:?}"));
        }
        assert!(Db::decode("nope").is_none());
    }

    #[test]
    fn prefix_replacement_only_touches_table_names() {
        let q = "SELECT * FROM chess.games g JOIN site.players p ON p.id = g.white_id -- the website. chess.";
        assert_eq!(
            prefix_replace(&prefix_replace(q, "chess.", "\"c\"."), "site.", "\"s\"."),
            "SELECT * FROM \"c\".games g JOIN \"s\".players p ON p.id = g.white_id -- the website. chess."
        );
        assert_eq!(prefix_replace("(chess.a,chess.b)", "chess.", "x."), "(x.a,x.b)");
    }

    #[test]
    fn schema_prefixes() {
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
        assert!(MIGRATIONS[0].contains("REFERENCES chess.players"));
    }
}
