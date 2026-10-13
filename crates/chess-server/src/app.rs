//! The shared context, and the HTTP server's routes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header, request::Parts};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

use crate::auth::{Auth, Identity, bearer};
use crate::config::Config;
use crate::db::Db;
use crate::error::ApiError;
use crate::http;
use crate::metrics::Metrics;

#[derive(Clone)]
pub struct Ctx {
    pub db: Db,
    pub cfg: Arc<Config>,
    pub auth: Auth,
    pub metrics: Metrics,
    /// Our board reader, when CHESS_VISION_LOCAL_MODEL names one.
    pub reader: Option<Arc<chess_vision::Reader>>,
    slots: Arc<Mutex<HashMap<(i64, &'static str), usize>>>,
}

/// Open streams a player may hold on one replica, by kind.
const STREAM_LIMITS: &[(&str, usize)] = &[("event", 8), ("game", 40), ("challenge", 20)];

/// One open stream of a player's; frees its slot when dropped.
pub struct Slot {
    slots: Arc<Mutex<HashMap<(i64, &'static str), usize>>>,
    key: (i64, &'static str),
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Ok(mut m) = self.slots.lock()
            && let Some(n) = m.get_mut(&self.key)
        {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&self.key);
            }
        }
    }
}

impl Ctx {
    pub fn new(db: Db, cfg: Config) -> Ctx {
        Ctx {
            auth: Auth::new(db.clone()),
            db,
            cfg: Arc::new(cfg),
            metrics: Metrics::new(),
            reader: None,
            slots: Arc::default(),
        }
    }

    /// Load the board reader CHESS_VISION_LOCAL_MODEL names, if any.
    pub fn with_reader(mut self) -> anyhow::Result<Ctx> {
        if let Some(r) = crate::vision::load_reader(&self.cfg)? {
            let card = r.card();
            tracing::info!(
                model = card.name,
                version = card.version,
                licence = card.licence,
                internal_only = card.internal_only,
                size = r.config().image_size,
                "board reader loaded"
            );
            self.reader = Some(Arc::new(r));
        }
        Ok(self)
    }

    /// Take a stream slot for `player`, or refuse with 429 (as Lichess does
    /// when one account opens too many streams).
    pub fn slot(&self, player: i64, kind: &'static str) -> Result<Slot, ApiError> {
        let limit = STREAM_LIMITS.iter().find(|(k, _)| *k == kind).map_or(10, |(_, n)| *n);
        let mut m = self.slots.lock().map_err(|_| ApiError::too_many("try again"))?;
        let n = m.entry((player, kind)).or_insert(0);
        if *n >= limit {
            return Err(ApiError::too_many(format!("Too many open {kind} streams for this account")));
        }
        *n += 1;
        Ok(Slot { slots: Arc::clone(&self.slots), key: (player, kind) })
    }

    /// Milliseconds each side has for a first move.
    pub fn first_move_ms(&self) -> i64 {
        i64::try_from(self.cfg.first_move_secs * 1000).unwrap_or(i64::MAX)
    }
}

/// A caller with a valid bearer token.
pub struct Who(pub Identity);

impl FromRequestParts<Ctx> for Who {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, ctx: &Ctx) -> Result<Who, ApiError> {
        let t = bearer(&parts.headers).ok_or_else(ApiError::unauthorized)?;
        match ctx.auth.token(t).await? {
            Some(id) => Ok(Who(id)),
            None => Err(ApiError::unauthorized()),
        }
    }
}

impl Who {
    /// The token must carry one of `scopes`.
    pub fn need(&self, scopes: &[&str]) -> Result<&Identity, ApiError> {
        if self.0.has_any(scopes) { Ok(&self.0) } else { Err(ApiError::missing_scope(scopes)) }
    }
}

/// Lichess answers API calls from any origin (tokens, not cookies, carry
/// the identity), so browsers' preflights get a yes.
async fn cors(req: Request, next: Next) -> Response {
    let preflight = req.method() == Method::OPTIONS;
    let mut resp = if preflight { StatusCode::NO_CONTENT.into_response() } else { next.run(req).await };
    let h = resp.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    if preflight {
        h.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, DELETE, OPTIONS"),
        );
        h.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("Authorization, Content-Type"),
        );
        h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("86400"));
    }
    resp
}

pub fn router(ctx: &Ctx) -> anyhow::Result<Router> {
    use http::{account, app_files, challenges, fairplay, feed, games, lab, oauth, streams, web};
    let api = Router::new()
        // OAuth (PKCE) and tokens
        .route("/oauth", get(oauth::authorize).post(oauth::decide))
        .route("/login", get(web::login_page).post(web::login))
        .route("/signup", get(web::signup_page).post(web::signup))
        .route("/logout", post(web::logout))
        .route("/account", get(web::account))
        .route("/account/tokens", post(web::new_token))
        .route("/account/tokens/revoke", post(web::revoke_token))
        .route("/auth/esports", get(web::esports_start))
        .route("/auth/esports/back", get(web::esports_back))
        .route("/api/esports/token", post(web::esports_token))
        .route("/api/esports/revoke", post(web::esports_revoke))
        .route("/api/esports/players", get(web::esports_players))
        .route("/api/feed", get(feed::feed))
        .route("/api/token", post(oauth::token).delete(oauth::revoke))
        .route("/api/token/test", post(oauth::test))
        // account and users
        .route("/api/account", get(account::me))
        .route("/api/account/email", get(account::email))
        .route("/api/account/playing", get(account::playing))
        .route("/api/user/{username}", get(account::user))
        .route("/api/player/autocomplete", get(account::autocomplete))
        .route("/api/bot/account/upgrade", post(account::upgrade))
        .route("/api/bot/online", get(account::bots_online))
        // streams
        .route("/api/stream/event", get(streams::events))
        .route("/api/board/game/stream/{id}", get(streams::board_game))
        .route("/api/bot/game/stream/{id}", get(streams::bot_game))
        .route("/api/stream/game/{id}", get(streams::moves))
        // challenges
        .route("/api/challenge", get(challenges::list))
        .route("/api/challenge/ai", post(challenges::ai))
        .route("/api/challenge/{id}", post(challenges::create))
        .route("/api/challenge/{id}/show", get(challenges::show))
        .route("/api/challenge/{id}/accept", post(challenges::accept))
        .route("/api/challenge/{id}/decline", post(challenges::decline))
        .route("/api/challenge/{id}/cancel", post(challenges::cancel))
        // playing: Board API (people) and Bot API (bot accounts)
        .merge(play_routes("board"))
        .merge(play_routes("bot"))
        // games
        .route("/game/export/{id}", get(games::export))
        .route("/api/games/export/{id}", get(games::export))
        // fair play (our own, for reviewers)
        .route("/api/fairplay/queue", get(fairplay::queue))
        .route("/api/fairplay/games/{id}", get(fairplay::game))
        .route("/api/fairplay/players/{username}", get(fairplay::player))
        .route("/api/fairplay/reviews", post(fairplay::review))
        // our additions for the training app
        .route("/api/board/game/{id}/assist", post(lab::assist))
        .route("/api/esl/position", get(lab::position).post(lab::position_hint))
        .route("/api/esl/position/moves", get(lab::position_moves))
        .route("/api/esl/tablebase", get(lab::tablebase))
        // Photos are sent as base64: allow up to 10 MB.
        .route(
            "/api/esl/recognise",
            post(lab::recognise).layer(axum::extract::DefaultBodyLimit::max(10 * 1024 * 1024)),
        )
        .route("/api/esl/photos/{id}/confirm", post(lab::confirm_photo))
        .route("/api/esl/photos", get(lab::photos))
        .route("/api/esl/photos/{id}/image", get(lab::photo_image))
        .route("/api/esl/photos/{id}/label", post(lab::label_photo))
        // the training app (static files)
        .route("/app", get(app_files::bare))
        .route("/app/", get(app_files::index))
        .route("/app/theme.css", get(app_files::theme))
        .route("/app/{*path}", get(app_files::file))
        .route("/docs", get(http::docs))
        .route("/docs/", get(http::docs))
        .route("/docs/{guide}", get(http::docs))
        .route("/favicon.svg", get(http::favicon))
        .route("/favicon.ico", get(http::favicon))
        .route("/", get(http::index))
        .layer(middleware::from_fn(cors))
        .with_state(ctx.clone());
    let base = ctx.cfg.base_path()?;
    Ok(if base.is_empty() { api } else { Router::new().nest(&base, api) })
}

/// The same actions under /api/board and /api/bot (the handler tells them
/// apart by the matched path).
fn play_routes(api: &str) -> Router<Ctx> {
    use http::play;
    let p = |tail: &str| format!("/api/{api}/game/{{id}}/{tail}");
    Router::new()
        .route(&p("move/{uci}"), post(play::make_move))
        .route(&p("resign"), post(play::resign))
        .route(&p("abort"), post(play::abort))
        .route(&p("draw/{accept}"), post(play::draw))
        .route(&p("takeback/{accept}"), post(play::takeback))
        .route(&p("claim-victory"), post(play::claim_victory))
        .route(&p("claim-draw"), post(play::claim_draw))
        .route(&p("chat"), post(play::chat_post).get(play::chat_get))
}

/// /metrics, /healthz and /readyz, on their own port.
pub fn ops_router(ctx: Ctx) -> Router {
    Router::new()
        .route("/metrics", get(|State(c): State<Ctx>| async move { c.metrics.render() }))
        .route("/healthz", get(|| async { "ok" }))
        .route(
            "/readyz",
            get(|State(c): State<Ctx>| async move {
                match c.db.conn().await {
                    Ok(conn) if conn.simple_query("SELECT 1").await.is_ok() => (StatusCode::OK, "ready"),
                    _ => (StatusCode::SERVICE_UNAVAILABLE, "database unavailable"),
                }
            }),
        )
        .with_state(ctx)
}
