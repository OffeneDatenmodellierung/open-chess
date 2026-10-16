//! Settings, all from the environment (containers on Kubernetes and
//! Cloudflare both pass configuration that way). Every variable has a
//! working default except `DATABASE_URL`.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;

#[derive(Debug, Clone, Args)]
#[allow(clippy::struct_excessive_bools)] // settings switches
pub struct Config {
    /// Postgres URL: this server's database (its own; it may share a host
    /// site's database in its own schema). A pooled (`-pooler`) host is
    /// fine; notifications use the direct host automatically.
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    pub database_url: String,

    /// A separate URL for LISTEN, if the direct host can't be derived.
    #[arg(long, env = "CHESS_LISTEN_DATABASE_URL", hide_env_values = true)]
    pub listen_database_url: Option<String>,

    /// A host site's schema in the same database whose chess players (and
    /// their chess tokens) are imported once, with their ids, the first
    /// time this version starts. Nothing is read from it afterwards; a
    /// schema that isn't there is skipped.
    #[arg(long, env = "CHESS_SITE_SCHEMA", default_value = "esports")]
    pub site_schema: String,

    /// This server's own schema (games, challenges, ratings, fair play).
    #[arg(long, env = "CHESS_SCHEMA", default_value = "chess")]
    pub schema: String,

    #[arg(long, env = "CHESS_POOL_SIZE", default_value_t = 8)]
    pub pool_size: usize,

    /// Where clients reach this server, including any path prefix
    /// (`https://chess.example.com`). Used in game URLs and
    /// to mount the API under the prefix.
    #[arg(long, env = "CHESS_PUBLIC_URL", default_value = "http://localhost:8090")]
    pub public_url: String,

    /// The host site (esports-lab) players can sign in with.
    #[arg(long, env = "ESPORTS_PUBLIC_URL", default_value = "http://localhost:8080")]
    pub site_url: String,

    /// "Sign in with the host site" (esports-lab) on this server's sign-in
    /// page: its players sign in here through it. Needs `ESPORTS_SECRET`.
    /// Off by default; the site's own use of this server (players, their
    /// tokens, reading games) needs only `ESPORTS_SECRET`, and the server's
    /// own accounts work either way.
    #[arg(
        long,
        env = "ESPORTS_LOGIN",
        default_value_t = false,
        num_args = 0..=1,
        default_missing_value = "true",
        action = clap::ArgAction::Set,
        value_parser = clap::builder::BoolishValueParser::new()
    )]
    pub esports_login: bool,

    /// The secret shared with the host site: it signs sign-ins and asks
    /// for its players' tokens with it (at least 32 characters).
    #[arg(long, env = "ESPORTS_SECRET", hide_env_values = true)]
    pub esports_secret: Option<String>,

    /// Anyone may make an account here (a name and a password). Off: only
    /// accounts that exist (and the host site's players, with
    /// `ESPORTS_LOGIN`) sign in.
    #[arg(
        long,
        env = "CHESS_SIGNUP",
        default_value_t = true,
        num_args = 0..=1,
        default_missing_value = "true",
        action = clap::ArgAction::Set,
        value_parser = clap::builder::BoolishValueParser::new()
    )]
    pub signup: bool,

    /// What players know the site as: on the OAuth page, in PGN event
    /// names and in `GET /` (which the training app shows).
    #[arg(long, env = "CHESS_SITE_NAME", default_value = "open-chess")]
    pub site_name: String,

    /// What players know the host site as (esports-lab), when they can sign
    /// in with it: "Sign in with …", "linked to your … account". The site
    /// name when unset.
    #[arg(long, env = "CHESS_HOST_NAME")]
    pub host_name: Option<String>,

    /// People made admins here, by email (comma-separated): an account gets
    /// the admin role when it signs in with one of these, once the email is
    /// confirmed (a host site's sign-in vouches for it) and it has a
    /// passkey. Accounts made here have unconfirmed emails: make them admins
    /// with `open-chess admin grant`.
    #[arg(long, env = "CHESS_ADMIN_EMAILS", value_delimiter = ',', default_value = "")]
    pub admin_emails: Vec<String>,

    /// Hours an admin's or manager's passkey check lasts.
    #[arg(long, env = "CHESS_ADMIN_PASSKEY_HOURS", default_value_t = 12)]
    pub staff_passkey_hours: i32,

    #[arg(long, env = "CHESS_BIND", default_value = "0.0.0.0:8090")]
    pub bind: String,

    /// /metrics, /healthz and /readyz.
    #[arg(long, env = "CHESS_METRICS_BIND", default_value = "0.0.0.0:9465")]
    pub metrics_bind: String,

    /// Milliseconds given back per move for the network round trip.
    #[arg(long, env = "CHESS_LAG_COMP_MS", default_value_t = 100)]
    pub lag_comp_ms: i64,

    /// A real-time challenge lapses after this many seconds unless the
    /// challenger keeps a `keepAliveStream` open.
    #[arg(long, env = "CHESS_CHALLENGE_TTL_SECS", default_value_t = 20)]
    pub challenge_ttl_secs: u64,

    /// Each side's first move must come within this many seconds or the
    /// game is aborted.
    #[arg(long, env = "CHESS_FIRST_MOVE_SECS", default_value_t = 30)]
    pub first_move_secs: u64,

    /// A player with no open game stream for this long is "gone".
    #[arg(long, env = "CHESS_GONE_AFTER_SECS", default_value_t = 20)]
    pub gone_after_secs: u64,

    /// Then the opponent may claim the win after this many more seconds.
    #[arg(long, env = "CHESS_CLAIM_WIN_SECS", default_value_t = 10)]
    pub claim_win_secs: u64,

    /// The public move stream runs this many plies behind a live game, so
    /// it can't feed an engine helping a player.
    #[arg(long, env = "CHESS_PUBLIC_DELAY_PLIES", default_value_t = 3)]
    pub public_delay_plies: usize,

    /// Queue human players' finished games for fair-play analysis.
    #[arg(long, env = "CHESS_FAIRPLAY", default_value_t = true, action = clap::ArgAction::Set)]
    pub fairplay: bool,

    /// Accounts marked for engine use can't start new games.
    #[arg(long, env = "CHESS_BLOCK_MARKED", default_value_t = true, action = clap::ArgAction::Set)]
    pub block_marked: bool,

    /// How often the clock sweeper looks for flags and lapsed challenges.
    #[arg(long, env = "CHESS_SWEEP_MS", default_value_t = 500)]
    pub sweep_ms: u64,

    /// Reading boards from photos: base URL of a vision model behind an
    /// OpenAI-style `/chat/completions` (Claude via
    /// `https://api.anthropic.com/v1`, Qwen-VL via Ollama's
    /// `http://localhost:11434/v1`, or a gateway). Empty: photo reading is
    /// off.
    #[arg(long, env = "CHESS_VISION_ENDPOINT", default_value = "")]
    pub vision_endpoint: String,

    /// The vision model's name as that server knows it.
    #[arg(long, env = "CHESS_VISION_MODEL", default_value = "qwen3-vl:8b")]
    pub vision_model: String,

    /// Bearer token for the vision server, if it needs one.
    #[arg(long, env = "CHESS_VISION_API_KEY", hide_env_values = true)]
    pub vision_api_key: Option<String>,

    /// Our own board reader (chess-vision): a `.safetensors` weights file
    /// from `chess-vision-train`. It reads photos first, on this
    /// server's CPU; the vision model at CHESS_VISION_ENDPOINT (if set) is
    /// only asked about photos it isn't sure of. Empty: not used.
    #[arg(long, env = "CHESS_VISION_LOCAL_MODEL", default_value = "")]
    pub vision_local_model: String,

    /// The board reader's answer is used without review when every square
    /// is above its unsure threshold and the chance that all 64 are right
    /// is at least this.
    #[arg(long, env = "CHESS_VISION_MIN_CONFIDENCE", default_value_t = 0.5)]
    pub vision_min_confidence: f32,

    /// Keep photos that couldn't be read for sure for the Review page,
    /// where a person confirms the position; confirmed photos become
    /// training data (`open-chess photos export`). Players are told.
    #[arg(long, env = "CHESS_VISION_REVIEW", default_value_t = true, action = clap::ArgAction::Set)]
    pub vision_review: bool,

    /// At most this many photos wait for review; beyond it, new ones
    /// aren't kept.
    #[arg(long, env = "CHESS_VISION_REVIEW_MAX", default_value_t = 5000)]
    pub vision_review_max: i64,

    /// Where exact endgame results (up to seven pieces) come from: a
    /// Lichess-compatible tablebase server. Answers are kept in the
    /// database, so each position is fetched once. Empty: off.
    #[arg(long, env = "CHESS_TABLEBASE_URL", default_value = "https://tablebase.lichess.ovh")]
    pub tablebase_url: String,

    /// The built training app (web/chess plus its wasm engine), served at
    /// `/app/`. The chess image puts it at /usr/share/open-chess/app.
    #[arg(long, env = "CHESS_APP_DIR")]
    pub app_dir: Option<String>,

    /// The app's look for this deployment: a theme the app bundles, by name
    /// (`esports-lab`: web/chess/themes), CSS, or the path of a .css file;
    /// served as `/app/theme.css`, after the app's own styles, so it can set
    /// the app's colour and font variables (docs/api.md, "The Chess lab's
    /// look"). `esports-lab` unless set; `none` for the app's own look.
    #[arg(long, env = "CHESS_APP_THEME", default_value = "esports-lab")]
    pub app_theme: Option<String>,

    /// Apple push notifications for the iOS app ("your move" in
    /// correspondence games, results to confirm): the `.p8` signing key
    /// (its PEM, or the path of the file) from the Apple Developer account.
    /// Without it, and the three below, nothing is sent.
    #[arg(long, env = "CHESS_APNS_KEY", hide_env_values = true)]
    pub apns_key: Option<String>,

    /// The key's id (10 characters, shown with the key).
    #[arg(long, env = "CHESS_APNS_KEY_ID")]
    pub apns_key_id: Option<String>,

    /// The Apple Developer team id.
    #[arg(long, env = "CHESS_APNS_TEAM_ID")]
    pub apns_team_id: Option<String>,

    /// The app's bundle id, which notifications are addressed to.
    #[arg(long, env = "CHESS_APNS_TOPIC", default_value = "org.offenedatenmodellierung.openchess")]
    pub apns_topic: String,

    /// Sign in with Apple: the `.p8` key made for it in the Apple Developer
    /// account (Keys › Sign in with Apple; its PEM, or the path of the
    /// file). With its id below (and the team id), the app's "Sign in with
    /// Apple" works; without them it's refused.
    #[arg(long, env = "CHESS_APPLE_KEY", hide_env_values = true)]
    pub apple_key: Option<String>,

    /// The Sign in with Apple key's id.
    #[arg(long, env = "CHESS_APPLE_KEY_ID")]
    pub apple_key_id: Option<String>,

    /// The Apple Developer team id (default: CHESS_APNS_TEAM_ID).
    #[arg(long, env = "CHESS_APPLE_TEAM_ID")]
    pub apple_team_id: Option<String>,

    /// Who Apple's sign-ins are for: the app's bundle id (default:
    /// CHESS_APNS_TOPIC), comma-separated with any others accepted.
    #[arg(long, env = "CHESS_APPLE_CLIENT_IDS", value_delimiter = ',')]
    pub apple_client_ids: Vec<String>,

    /// Sign in with Apple on the website (the sign-in and account pages):
    /// the Services ID made for it, whose return URL is
    /// `{CHESS_PUBLIC_URL}/auth/apple/callback`. Without it, only the app
    /// signs in with Apple.
    #[arg(long, env = "CHESS_APPLE_SERVICES_ID")]
    pub apple_services_id: Option<String>,

    /// Apple's sign-in service (only tests change it).
    #[arg(long, env = "CHESS_APPLE_URL", default_value = "https://appleid.apple.com")]
    pub apple_url: String,
}

impl Config {
    /// The path the API lives under (`/chess`, or empty).
    pub fn base_path(&self) -> Result<String> {
        let u = url::Url::parse(&self.public_url).context("CHESS_PUBLIC_URL isn't a URL")?;
        let p = u.path().trim_end_matches('/');
        Ok(p.to_owned())
    }

    pub fn public_url(&self) -> &str {
        self.public_url.trim_end_matches('/')
    }

    /// What players know the host site as.
    pub fn host_name(&self) -> &str {
        self.host_name.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(&self.site_name)
    }

    /// `CHESS_ADMIN_EMAILS`, lower-cased.
    pub fn admin_emails(&self) -> Vec<String> {
        self.admin_emails.iter().map(|e| e.trim().to_lowercase()).filter(|e| e.contains('@')).collect()
    }

    /// The secret shared with the host site (32 characters or more): the
    /// site's server uses it to act as the host site.
    pub fn site_secret(&self) -> Option<&str> {
        self.esports_secret.as_deref().filter(|s| s.len() >= 32)
    }

    /// The shared secret, when signing in here with the host site is on.
    pub fn esports(&self) -> Option<&str> {
        self.site_secret().filter(|_| self.esports_login)
    }

    pub fn challenge_ttl(&self) -> Duration {
        Duration::from_secs(self.challenge_ttl_secs.max(5))
    }

    pub fn validate(&self) -> Result<()> {
        for s in [&self.schema, &self.site_schema] {
            if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                bail!("schema names are letters, digits and _ (got {s:?})");
            }
        }
        if self.schema == self.site_schema {
            bail!("CHESS_SCHEMA must differ from CHESS_SITE_SCHEMA");
        }
        self.base_path()?;
        Ok(())
    }
}

/// Settings for the fair-play worker (`open-chess analyse`).
#[derive(Debug, Clone, Args)]
pub struct AnalyseConfig {
    /// The UCI engine (Stockfish).
    #[arg(long, env = "CHESS_STOCKFISH")]
    pub stockfish: Option<String>,

    /// Search depth per position.
    #[arg(long, env = "CHESS_ANALYSIS_DEPTH", default_value_t = 14)]
    pub depth: u32,

    /// Engine threads.
    #[arg(long, env = "CHESS_ANALYSIS_THREADS", default_value_t = 1)]
    pub threads: u32,

    /// Engine hash, MiB.
    #[arg(long, env = "CHESS_ANALYSIS_HASH_MB", default_value_t = 64)]
    pub hash_mb: u32,

    /// Plies at the start left out of screening (opening theory).
    #[arg(long, env = "CHESS_FAIRPLAY_SKIP_PLIES", default_value_t = 16)]
    pub skip_plies: usize,

    /// The screening model: how the fair-play signals turn into a score
    /// and a flag (`chess_fairplay::Model`, as JSON). Either a path to a
    /// JSON file or the JSON itself (handy as a secret). Keep a tuned
    /// model private: its numbers tell people what screening looks for.
    /// Unset: an untuned example model, and a warning.
    #[arg(long, env = "CHESS_FAIRPLAY_MODEL", hide_env_values = true)]
    pub model: Option<String>,

    /// Overrides the model's flag score (0–100).
    #[arg(long, env = "CHESS_FAIRPLAY_FLAG_SCORE")]
    pub flag_score: Option<f64>,

    /// Overrides the model's fewest screened moves before a game can be
    /// flagged.
    #[arg(long, env = "CHESS_FAIRPLAY_MIN_MOVES")]
    pub min_moves: Option<usize>,

    /// The training app's engine settings, for the app fingerprint (the
    /// app's "Normal" search). Keep in step with web/chess/app.js.
    #[arg(long, env = "CHESS_FAIRPLAY_APP_DEPTH", default_value_t = 6)]
    pub app_depth: u8,

    #[arg(long, env = "CHESS_FAIRPLAY_APP_NODES", default_value_t = 400_000)]
    pub app_nodes: u64,

    /// The NNUE weight file the app ships (web/chess/nets/default.nnue),
    /// if it ships one, so the fingerprint uses the same evaluation.
    #[arg(long, env = "CHESS_FAIRPLAY_APP_NET")]
    pub app_net: Option<String>,

    /// Analyse one game and exit (for testing).
    #[arg(long)]
    pub once: bool,
}

impl AnalyseConfig {
    /// The screening model: `CHESS_FAIRPLAY_MODEL` (a JSON file or inline
    /// JSON), else the untuned example, with any overrides applied.
    /// Returns whether a model was configured, so callers can warn.
    pub fn model(&self) -> Result<(chess_fairplay::Model, bool)> {
        let (mut model, configured) = match self.model.as_deref().map(str::trim) {
            None | Some("") => (chess_fairplay::Model::default(), false),
            Some(json) if json.starts_with('{') => {
                (serde_json::from_str(json).context("CHESS_FAIRPLAY_MODEL isn't a valid model")?, true)
            }
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("reading the fair-play model {path}"))?;
                (serde_json::from_str(&text).with_context(|| format!("{path} isn't a valid model"))?, true)
            }
        };
        if let Some(s) = self.flag_score {
            model.flag_score = s;
        }
        if let Some(n) = self.min_moves {
            model.min_moves = n;
        }
        model.validate().map_err(|e| anyhow::anyhow!("fair-play model: {e}"))?;
        Ok((model, configured))
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::Config;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        cfg: Config,
    }

    #[test]
    fn the_esports_look_unless_set() {
        if std::env::var_os("CHESS_APP_THEME").is_some() {
            return; // set where the tests run: the default can't show
        }
        let cli = Cli::try_parse_from(["chess-server", "--database-url", "postgres://x"]).unwrap();
        assert_eq!(cli.cfg.app_theme.as_deref(), Some("esports-lab"));
        let cli =
            Cli::try_parse_from(["chess-server", "--database-url", "postgres://x", "--app-theme", "none"])
                .unwrap();
        assert_eq!(cli.cfg.app_theme.as_deref(), Some("none"));
    }
}
