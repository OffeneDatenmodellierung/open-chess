//! An async client for the Lichess Board, Bot and Challenge APIs.
//!
//! Point it at `https://lichess.org` or at an open-chess server:
//! they speak the same API. This is the transport a bot needs (event
//! stream, game streams, moves, challenges) and nothing more; engines live
//! in `open-chess-engine`.
//!
//! ```no_run
//! # async fn demo() -> Result<(), open_chess_client::Error> {
//! use open_chess_client::{Api, Client};
//! use open_chess_client::proto::StreamEvent;
//! let c = Client::new("https://lichess.org", Some("lip_…"))?;
//! let mut events = c.stream_events().await?;
//! while let Some(ev) = events.next().await {
//!     if let StreamEvent::GameStart(g) = ev? {
//!         c.make_move(Api::Bot, &g.game_id, "e2e4", false).await?;
//!     }
//! }
//! # Ok(()) }
//! ```

pub use chess_proto as proto;

use std::fmt;
use std::pin::Pin;
use std::time::Duration;

use bytes::Bytes;
use chess_proto::{BoardEvent, Challenge, ChallengeList, GameJson, MoveLine, NowPlaying, StreamEvent, User};
use futures_util::{Stream, StreamExt};
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Debug)]
pub enum Error {
    /// The request didn't get a response (network, TLS, timeout).
    Http(reqwest::Error),
    /// The server answered with an error status.
    Api { status: u16, message: String },
    /// The response wasn't the JSON expected.
    Json(serde_json::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Http(e) => write!(f, "request failed: {e}"),
            Error::Api { status, message } => write!(f, "the server said {status}: {message}"),
            Error::Json(e) => write!(f, "unexpected reply: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Which API a game is played through: humans use the Board API, bot
/// accounts the Bot API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Api {
    Board,
    Bot,
}

impl Api {
    fn prefix(self) -> &'static str {
        match self {
            Api::Board => "board",
            Api::Bot => "bot",
        }
    }
}

/// Parameters for `POST /api/challenge/{username}`.
#[derive(Debug, Clone, Default)]
pub struct ChallengeRequest {
    pub rated: bool,
    /// Seconds; `None` with `increment_secs` `None` makes an unlimited game.
    pub limit_secs: Option<u32>,
    pub increment_secs: Option<u32>,
    /// white | black | random (default random).
    pub color: Option<String>,
    /// A FEN to start from (variant fromPosition).
    pub fen: Option<String>,
    /// The opponent's token: the challenge is accepted on their behalf and
    /// the game starts at once (an open-chess extension, also understood by
    /// Lichess for some token kinds).
    pub accept_by_token: Option<String>,
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .field("token", &self.token.as_ref().map(|_| "…"))
            .finish_non_exhaustive()
    }
}

impl Client {
    /// A client for `base` (e.g. `https://lichess.org`, or the chess
    /// server's public URL), sending `token` as the bearer if given.
    pub fn new(base: &str, token: Option<&str>) -> Result<Client> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("open-chess-client/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(Client::with_http(http, base, token))
    }

    pub fn with_http(http: reqwest::Client, base: &str, token: Option<&str>) -> Client {
        Client { http, base: base.trim_end_matches('/').to_owned(), token: token.map(str::to_owned) }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let r = self.http.request(method, self.url(path));
        match &self.token {
            Some(t) => r.bearer_auth(t),
            None => r,
        }
    }

    async fn send(r: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let resp = r.send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        let message = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| body.chars().take(300).collect());
        Err(Error::Api { status: status.as_u16(), message })
    }

    async fn json<T: DeserializeOwned>(r: reqwest::RequestBuilder) -> Result<T> {
        let resp = Self::send(r.header("Accept", "application/json")).await?;
        let bytes = resp.bytes().await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    async fn ok(r: reqwest::RequestBuilder) -> Result<()> {
        Self::send(r).await.map(drop)
    }

    // ─── account ───

    pub async fn account(&self) -> Result<User> {
        Self::json(self.req(reqwest::Method::GET, "/api/account")).await
    }

    pub async fn user(&self, username: &str) -> Result<User> {
        Self::json(self.req(reqwest::Method::GET, &format!("/api/user/{}", urlencoding::encode(username))))
            .await
    }

    pub async fn playing(&self) -> Result<NowPlaying> {
        Self::json(self.req(reqwest::Method::GET, "/api/account/playing")).await
    }

    /// Make this account a bot (irreversible; the account must not have
    /// played a game).
    pub async fn upgrade_to_bot(&self) -> Result<()> {
        Self::ok(self.req(reqwest::Method::POST, "/api/bot/account/upgrade")).await
    }

    // ─── streams ───

    pub async fn stream_events(&self) -> Result<Ndjson<StreamEvent>> {
        let resp = Self::send(self.req(reqwest::Method::GET, "/api/stream/event")).await?;
        Ok(Ndjson::new(resp, StreamEvent::parse))
    }

    pub async fn stream_game(&self, api: Api, game_id: &str) -> Result<Ndjson<BoardEvent>> {
        let path = format!("/api/{}/game/stream/{}", api.prefix(), urlencoding::encode(game_id));
        let resp = Self::send(self.req(reqwest::Method::GET, &path)).await?;
        Ok(Ndjson::new(resp, BoardEvent::parse))
    }

    /// The public move stream: a game description first, then
    /// [`MoveLine`]s (as JSON values: the first line has another shape).
    pub async fn stream_moves(&self, game_id: &str) -> Result<Ndjson<Value>> {
        let path = format!("/api/stream/game/{}", urlencoding::encode(game_id));
        let resp = Self::send(self.req(reqwest::Method::GET, &path)).await?;
        Ok(Ndjson::new(resp, |l| serde_json::from_slice(l)))
    }

    /// Parse a move-stream line as a move (None for the description).
    pub fn move_line(v: &Value) -> Option<MoveLine> {
        v.get("fen").and_then(|_| serde_json::from_value(v.clone()).ok())
    }

    // ─── playing ───

    pub async fn make_move(&self, api: Api, game_id: &str, uci: &str, offering_draw: bool) -> Result<()> {
        let mut path = format!(
            "/api/{}/game/{}/move/{}",
            api.prefix(),
            urlencoding::encode(game_id),
            urlencoding::encode(uci)
        );
        if offering_draw {
            path.push_str("?offeringDraw=true");
        }
        Self::ok(self.req(reqwest::Method::POST, &path)).await
    }

    async fn game_action(&self, api: Api, game_id: &str, action: &str) -> Result<()> {
        let path = format!("/api/{}/game/{}/{action}", api.prefix(), urlencoding::encode(game_id));
        Self::ok(self.req(reqwest::Method::POST, &path)).await
    }

    pub async fn resign(&self, api: Api, game_id: &str) -> Result<()> {
        self.game_action(api, game_id, "resign").await
    }

    pub async fn abort(&self, api: Api, game_id: &str) -> Result<()> {
        self.game_action(api, game_id, "abort").await
    }

    pub async fn draw(&self, api: Api, game_id: &str, accept: bool) -> Result<()> {
        self.game_action(api, game_id, if accept { "draw/yes" } else { "draw/no" }).await
    }

    pub async fn takeback(&self, api: Api, game_id: &str, accept: bool) -> Result<()> {
        self.game_action(api, game_id, if accept { "takeback/yes" } else { "takeback/no" }).await
    }

    pub async fn claim_victory(&self, api: Api, game_id: &str) -> Result<()> {
        self.game_action(api, game_id, "claim-victory").await
    }

    pub async fn claim_draw(&self, api: Api, game_id: &str) -> Result<()> {
        self.game_action(api, game_id, "claim-draw").await
    }

    pub async fn chat(&self, api: Api, game_id: &str, room: &str, text: &str) -> Result<()> {
        let path = format!("/api/{}/game/{}/chat", api.prefix(), urlencoding::encode(game_id));
        Self::ok(self.req(reqwest::Method::POST, &path).form(&[("room", room), ("text", text)])).await
    }

    // ─── challenges ───

    pub async fn challenge(&self, username: &str, c: &ChallengeRequest) -> Result<Challenge> {
        let mut form: Vec<(&str, String)> = vec![("rated", c.rated.to_string())];
        if let Some(l) = c.limit_secs {
            form.push(("clock.limit", l.to_string()));
        }
        if let Some(i) = c.increment_secs {
            form.push(("clock.increment", i.to_string()));
        }
        if let Some(col) = &c.color {
            form.push(("color", col.clone()));
        }
        if let Some(fen) = &c.fen {
            form.push(("variant", "fromPosition".into()));
            form.push(("fen", fen.clone()));
        }
        if let Some(t) = &c.accept_by_token {
            form.push(("acceptByToken", t.clone()));
        }
        let path = format!("/api/challenge/{}", urlencoding::encode(username));
        Self::json(self.req(reqwest::Method::POST, &path).form(&form)).await
    }

    pub async fn challenges(&self) -> Result<ChallengeList> {
        Self::json(self.req(reqwest::Method::GET, "/api/challenge")).await
    }

    pub async fn accept_challenge(&self, id: &str) -> Result<()> {
        Self::ok(
            self.req(reqwest::Method::POST, &format!("/api/challenge/{}/accept", urlencoding::encode(id))),
        )
        .await
    }

    pub async fn decline_challenge(&self, id: &str, reason: Option<&str>) -> Result<()> {
        let r =
            self.req(reqwest::Method::POST, &format!("/api/challenge/{}/decline", urlencoding::encode(id)));
        let r = match reason {
            Some(reason) => r.form(&[("reason", reason)]),
            None => r,
        };
        Self::ok(r).await
    }

    pub async fn cancel_challenge(&self, id: &str) -> Result<()> {
        Self::ok(
            self.req(reqwest::Method::POST, &format!("/api/challenge/{}/cancel", urlencoding::encode(id))),
        )
        .await
    }

    // ─── games ───

    /// The game as JSON with its PGN.
    pub async fn export_game(&self, id: &str) -> Result<GameJson> {
        let path = format!("/game/export/{}?pgnInJson=true&clocks=true", urlencoding::encode(id));
        Self::json(self.req(reqwest::Method::GET, &path)).await
    }
}

type ByteStream = Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>;

/// A newline-delimited JSON stream. Blank keep-alive lines are skipped.
pub struct Ndjson<T> {
    inner: ByteStream,
    buf: Vec<u8>,
    parse: fn(&[u8]) -> serde_json::Result<T>,
}

impl<T> Ndjson<T> {
    fn new(resp: reqwest::Response, parse: fn(&[u8]) -> serde_json::Result<T>) -> Ndjson<T> {
        Ndjson { inner: Box::pin(resp.bytes_stream()), buf: Vec::with_capacity(4096), parse }
    }

    /// The next message, or `None` once the server closes the stream.
    pub async fn next(&mut self) -> Option<Result<T>> {
        loop {
            if let Some(nl) = self.buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=nl).collect();
                let line = &line[..line.len() - 1];
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                return Some((self.parse)(line).map_err(Error::Json));
            }
            match self.inner.next().await {
                Some(Ok(chunk)) => self.buf.extend_from_slice(&chunk),
                Some(Err(e)) => return Some(Err(Error::Http(e))),
                None => {
                    // A last line without a newline.
                    if self.buf.iter().all(u8::is_ascii_whitespace) {
                        return None;
                    }
                    let line = std::mem::take(&mut self.buf);
                    return Some((self.parse)(&line).map_err(Error::Json));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Serve one canned HTTP response; return the base URL and the raw
    /// request received.
    async fn serve_once(response: String) -> (String, tokio::task::JoinHandle<String>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let h = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = s.read(&mut buf).await.unwrap();
            s.write_all(response.as_bytes()).await.unwrap();
            s.shutdown().await.unwrap();
            String::from_utf8_lossy(&buf[..n]).into_owned()
        });
        (format!("http://{addr}"), h)
    }

    #[tokio::test]
    async fn ndjson_skips_keepalives_and_reads_last_line() {
        let body = "\n{\"type\":\"gameState\",\"moves\":\"e2e4\",\"wtime\":1,\"btime\":1,\"winc\":0,\"binc\":0,\"status\":\"started\"}\n\n{\"type\":\"opponentGone\",\"gone\":false}";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (base, req) = serve_once(resp).await;
        let c = Client::new(&base, Some("tok")).unwrap();
        let mut s = c.stream_game(Api::Bot, "g1").await.unwrap();
        assert!(matches!(s.next().await, Some(Ok(BoardEvent::GameState(_)))));
        assert!(matches!(s.next().await, Some(Ok(BoardEvent::OpponentGone(_)))));
        assert!(s.next().await.is_none());
        let raw = req.await.unwrap();
        assert!(raw.starts_with("GET /api/bot/game/stream/g1 "), "{raw}");
        assert!(raw.to_lowercase().contains("authorization: bearer tok"));
    }

    #[tokio::test]
    async fn api_errors_carry_the_message() {
        let body = r#"{"error":"Not your turn, or game already over"}"#;
        let resp = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (base, req) = serve_once(resp).await;
        let c = Client::new(&base, Some("tok")).unwrap();
        let e = c.make_move(Api::Board, "g1", "e2e4", true).await.unwrap_err();
        match e {
            Error::Api { status, message } => {
                assert_eq!(status, 400);
                assert_eq!(message, "Not your turn, or game already over");
            }
            other => panic!("{other}"),
        }
        assert!(req.await.unwrap().starts_with("POST /api/board/game/g1/move/e2e4?offeringDraw=true "));
    }

    #[tokio::test]
    async fn challenge_form_fields() {
        let body = r#"{"id":"abcd1234","url":"http://x/abcd1234","status":"accepted","challenger":{"id":"a","name":"A"},
            "destUser":{"id":"b","name":"B"},"variant":{"key":"standard","name":"Standard"},"rated":false,"speed":"blitz",
            "timeControl":{"type":"clock","limit":180,"increment":2},"color":"random","perf":{"icon":"","name":"Blitz"}}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (base, req) = serve_once(resp).await;
        let c = Client::new(&base, Some("tok")).unwrap();
        let ch = c
            .challenge(
                "bob",
                &ChallengeRequest {
                    limit_secs: Some(180),
                    increment_secs: Some(2),
                    accept_by_token: Some("esl_x".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(ch.id, "abcd1234");
        let raw = req.await.unwrap();
        assert!(raw.contains("clock.limit=180&clock.increment=2&acceptByToken=esl_x"), "{raw}");
    }
}
