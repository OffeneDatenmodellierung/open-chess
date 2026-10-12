//! The device's web server: the Chess lab app and a Lichess-style API.
//!
//! A phone or tablet on the same network opens `http://<device>:<port>/`,
//! gets the app, and the app shows the game on the board as it shows an
//! online game: it reads the same Board API streams it reads from
//! lichess.org or an open-chess server. The device answers:
//!
//! * `GET /`: the site's name, and that it's a device (`"device": true`);
//!   browsers are sent on to the app.
//! * `GET /api/account`, `GET /api/account/playing`.
//! * `GET /api/stream/event`: `gameStart` when a game starts on the board,
//!   `gameFinish` when it ends.
//! * `GET /api/board/game/stream/{id}`: `gameFull`, then `gameState` as
//!   moves are made, and the clock.
//! * `GET /api/stream/game/{id}`: the live feed, for anyone watching.
//! * `GET /api/device`, `POST /api/device`: the device's settings (the
//!   engine's level, the side the person plays, the time control), which
//!   the app's device panel changes.
//! * `GET /app/…`: the app's files, from `--app`.
//!
//! Moves are made on the board, so the Board API's move, resign, draw and
//! abort calls answer with an error saying so. No sign-in: anyone who can
//! reach the device can watch and change its settings, so it listens only
//! where `--listen` says (a home network, not the internet).

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use bytes::Bytes;
use chess_core::Side;
use chess_proto as p;
use serde_json::json;
use tokio::sync::{mpsc, oneshot, watch};

use crate::controller::TimeControl;
use crate::view::{self, View};

/// How often an idle stream sends an empty line, as Lichess does, so
/// clients and proxies know it's alive.
const KEEPALIVE: Duration = Duration::from_secs(7);

/// A change to the device's settings, from the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Level(u8),
    Player(Side),
    Time(Option<TimeControl>),
}

/// A change and where to send what came of it (a message, or why not).
#[derive(Debug)]
pub struct Request {
    pub change: Change,
    pub reply: oneshot::Sender<Result<String, String>>,
}

/// What the web server shares with the board loop.
#[derive(Debug, Clone)]
pub struct Web {
    view: watch::Receiver<View>,
    requests: std_mpsc::Sender<Request>,
    app_dir: Option<Arc<PathBuf>>,
    site: Arc<str>,
}

impl Web {
    pub fn new(
        view: watch::Receiver<View>,
        requests: std_mpsc::Sender<Request>,
        app_dir: Option<PathBuf>,
        site: &str,
    ) -> Web {
        Web { view, requests, app_dir: app_dir.map(Arc::new), site: site.into() }
    }
}

/// Serve on `addr` from a thread of its own. Returns where it listens.
pub fn spawn(addr: &str, web: Web) -> anyhow::Result<SocketAddr> {
    let listener = std::net::TcpListener::bind(addr)?;
    listener.set_nonblocking(true)?;
    let local = listener.local_addr()?;
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    std::thread::Builder::new().name("web".into()).spawn(move || {
        rt.block_on(async move {
            match tokio::net::TcpListener::from_std(listener) {
                Ok(l) => {
                    if let Err(e) = axum::serve(l, router(web)).await {
                        eprintln!("web: {e}");
                    }
                }
                Err(e) => eprintln!("web: {e}"),
            }
        });
    })?;
    Ok(local)
}

pub fn router(web: Web) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/account", get(account))
        .route("/api/account/playing", get(playing))
        .route("/api/stream/event", get(events))
        .route("/api/board/game/stream/{id}", get(board_game))
        .route("/api/stream/game/{id}", get(feed))
        .route("/api/board/game/{id}/{*action}", post(board_action))
        .route("/api/device", get(device).post(change_device))
        .route("/app", get(|| async { Redirect::permanent("/app/") }))
        .route("/app/", get(app_index))
        .route("/app/{*path}", get(app_file))
        .with_state(web)
}

fn error(status: StatusCode, msg: &str) -> Response {
    (status, Json(p::ApiError { error: msg.into() })).into_response()
}

async fn index(State(w): State<Web>, headers: HeaderMap) -> Response {
    let html =
        headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()).is_some_and(|a| a.contains("text/html"));
    if html && w.app_dir.is_some() {
        return Redirect::to("/app/").into_response();
    }
    Json(json!({"site": &*w.site, "device": true})).into_response()
}

async fn account() -> Response {
    Json(view::account()).into_response()
}

async fn playing(State(w): State<Web>) -> Response {
    let v = w.view.borrow().clone();
    let games: Vec<_> =
        if v.started() { vec![v.game_event(p::GameEvent::START, Instant::now()).game] } else { Vec::new() };
    Json(json!({ "nowPlaying": games })).into_response()
}

async fn device(State(w): State<Web>) -> Response {
    Json(w.view.borrow().device()).into_response()
}

/// `POST /api/device` with any of `level` (1–8), `play` (white, black) and
/// `time` (`5+3`, or `none` for untimed).
async fn change_device(State(w): State<Web>, Form(form): Form<HashMap<String, String>>) -> Response {
    let mut changes = Vec::new();
    if let Some(l) = form.get("level").filter(|s| !s.is_empty()) {
        match l.parse::<u8>() {
            Ok(n @ 1..=8) => changes.push(Change::Level(n)),
            _ => return error(StatusCode::BAD_REQUEST, "level: 1 to 8"),
        }
    }
    if let Some(side) = form.get("play").filter(|s| !s.is_empty()) {
        match Side::from_name(side) {
            Some(s) => changes.push(Change::Player(s)),
            None => return error(StatusCode::BAD_REQUEST, "play: white or black"),
        }
    }
    if let Some(t) = form.get("time").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if t == "none" {
            changes.push(Change::Time(None));
        } else {
            match t.parse::<TimeControl>() {
                Ok(tc) => changes.push(Change::Time(Some(tc))),
                Err(e) => return error(StatusCode::BAD_REQUEST, &format!("time: {e}")),
            }
        }
    }
    let mut said = Vec::new();
    for change in changes {
        let (tx, rx) = oneshot::channel();
        if w.requests.send(Request { change, reply: tx }).is_err() {
            return error(StatusCode::SERVICE_UNAVAILABLE, "the device is stopping");
        }
        match tokio::time::timeout(Duration::from_secs(5), rx).await {
            Ok(Ok(Ok(msg))) => said.push(msg),
            Ok(Ok(Err(e))) => return error(StatusCode::BAD_REQUEST, &e),
            _ => return error(StatusCode::SERVICE_UNAVAILABLE, "the device didn't answer"),
        }
    }
    let mut d = w.view.borrow().device();
    d["said"] = json!(said);
    Json(d).into_response()
}

async fn board_action(UrlPath((_id, action)): UrlPath<(String, String)>) -> Response {
    let msg = if action.starts_with("move/") {
        "Make your move on the board."
    } else {
        "On a board game, set up the starting position for a new game, or take moves back on the board."
    };
    error(StatusCode::BAD_REQUEST, msg)
}

// ── streams ──

fn ndjson(rx: mpsc::Receiver<Bytes>) -> Response {
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|b| (Ok::<Bytes, Infallible>(b), rx))
    });
    let mut r = Body::from_stream(stream).into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/x-ndjson"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    r
}

/// Send one JSON line. False once the client has gone.
async fn send<T: serde::Serialize>(tx: &mpsc::Sender<Bytes>, value: &T) -> bool {
    let Ok(mut line) = serde_json::to_vec(value) else { return false };
    line.push(b'\n');
    tx.send(Bytes::from(line)).await.is_ok()
}

/// Wait for the game to change. False once the client or the device has
/// gone; sends keepalives meanwhile.
async fn next(rx: &mut watch::Receiver<View>, tx: &mpsc::Sender<Bytes>) -> bool {
    loop {
        match tokio::time::timeout(KEEPALIVE, rx.changed()).await {
            Ok(Ok(())) => return true,
            Ok(Err(_)) => return false,
            Err(_) => {
                if tx.send(Bytes::from_static(b"\n")).await.is_err() {
                    return false;
                }
            }
        }
    }
}

/// The game, ended because a new one was set up on the board.
fn abandoned(v: &View) -> View {
    View { status: chess_core::Status::Aborted, running: None, ..v.clone() }
}

async fn events(State(w): State<Web>) -> Response {
    let mut rx = w.view.clone();
    let (tx, out) = mpsc::channel(16);
    tokio::spawn(async move {
        let mut last: Option<View> = None;
        loop {
            let v = rx.borrow_and_update().clone();
            let now = Instant::now();
            let mut lines = Vec::new();
            match &last {
                Some(l) if l.game_id != v.game_id => {
                    if l.started() {
                        lines.push(abandoned(l).game_event(p::GameEvent::FINISH, now));
                    }
                    if v.started() {
                        lines.push(v.game_event(p::GameEvent::START, now));
                    }
                }
                Some(l) if l.started() && !v.started() => lines.push(v.game_event(p::GameEvent::FINISH, now)),
                Some(l) if !l.started() && v.started() => lines.push(v.game_event(p::GameEvent::START, now)),
                None if v.started() => lines.push(v.game_event(p::GameEvent::START, now)),
                _ => {}
            }
            for line in &lines {
                if !send(&tx, line).await {
                    return;
                }
            }
            last = Some(v);
            if !next(&mut rx, &tx).await {
                return;
            }
        }
    });
    ndjson(out)
}

async fn board_game(State(w): State<Web>, UrlPath(id): UrlPath<String>) -> Response {
    let mut rx = w.view.clone();
    if rx.borrow().game_id != id {
        return error(StatusCode::NOT_FOUND, "No such game on this board");
    }
    let (tx, out) = mpsc::channel(16);
    tokio::spawn(async move {
        let mut last = rx.borrow_and_update().clone();
        if !send(&tx, &last.game_full(Instant::now())).await {
            return;
        }
        while last.started() && next(&mut rx, &tx).await {
            let v = rx.borrow_and_update().clone();
            let now = Instant::now();
            if v.game_id != id {
                send(&tx, &abandoned(&last).game_state(now)).await;
                return;
            }
            if !v.same(&last) && !send(&tx, &v.game_state(now)).await {
                return;
            }
            last = v;
        }
    });
    ndjson(out)
}

async fn feed(State(w): State<Web>, UrlPath(id): UrlPath<String>) -> Response {
    let mut rx = w.view.clone();
    if rx.borrow().game_id != id {
        return error(StatusCode::NOT_FOUND, "No such game on this board");
    }
    let (tx, out) = mpsc::channel(16);
    tokio::spawn(async move {
        let mut last = rx.borrow_and_update().clone();
        if !send(&tx, &last.feed_description(Instant::now())).await {
            return;
        }
        while last.started() && next(&mut rx, &tx).await {
            let v = rx.borrow_and_update().clone();
            let now = Instant::now();
            if v.game_id != id {
                send(&tx, &abandoned(&last).feed_description(now)).await;
                return;
            }
            if v.moves != last.moves && !send(&tx, &v.feed_move(now)).await {
                return;
            }
            if !v.started() {
                send(&tx, &v.feed_description(now)).await;
            }
            last = v;
        }
    });
    ndjson(out)
}

// ── the app's files ──

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "wasm" => "application/wasm",
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// The file for a URL path inside the app, or `None` if the path tries to
/// leave the app directory or names something hidden.
fn resolve(dir: &Path, tail: &str) -> Option<PathBuf> {
    let tail = tail.trim_start_matches('/');
    let rel =
        if tail.is_empty() || tail.ends_with('/') { format!("{tail}index.html") } else { tail.to_owned() };
    let mut out = dir.to_path_buf();
    for c in Path::new(&rel).components() {
        match c {
            Component::Normal(part) if part.to_str().is_some_and(|s| !s.starts_with('.')) => out.push(part),
            _ => return None,
        }
    }
    Some(out)
}

async fn app_index(State(w): State<Web>) -> Response {
    serve_file(&w, "").await
}

async fn app_file(State(w): State<Web>, UrlPath(tail): UrlPath<String>) -> Response {
    serve_file(&w, &tail).await
}

async fn serve_file(w: &Web, tail: &str) -> Response {
    let Some(dir) = &w.app_dir else {
        return (StatusCode::NOT_FOUND, "the app isn't installed on this device (--app)").into_response();
    };
    let Some(path) = resolve(dir, tail) else { return StatusCode::NOT_FOUND.into_response() };
    let Ok(body) = tokio::fs::read(&path).await else { return StatusCode::NOT_FOUND.into_response() };
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let cache = if matches!(name, "index.html" | "sw.js" | "manifest.webmanifest") {
        "no-cache"
    } else {
        "public, max-age=3600"
    };
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type(&path)));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    (h, body).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{Controller, Setup};
    use crate::view::{EngineInfo, game_id};
    use chess_dgt::Follower;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    fn view(c: &Controller, f: &Follower, game: u32) -> View {
        View::new(c, f, &EngineInfo { name: None }, game_id(1, game), 0, Instant::now())
    }

    /// A raw HTTP/1.1 request; the response's head and a reader for its body.
    async fn request(
        addr: SocketAddr,
        method: &str,
        path: &str,
        body: &str,
    ) -> (String, BufReader<tokio::net::TcpStream>) {
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).await.unwrap();
        let mut r = BufReader::new(s);
        let mut head = String::new();
        loop {
            let mut line = String::new();
            r.read_line(&mut line).await.unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
            head.push_str(&line);
        }
        (head, r)
    }

    /// The next JSON line of a chunked ndjson body.
    async fn json_line(r: &mut BufReader<tokio::net::TcpStream>) -> serde_json::Value {
        loop {
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(5), r.read_line(&mut line)).await.unwrap().unwrap();
            let t = line.trim();
            if t.starts_with('{') {
                return serde_json::from_str(t).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn the_app_follows_the_board_game() {
        let c = Controller::new(Setup::default());
        let f = Follower::new();
        let (view_tx, view_rx) = watch::channel(view(&c, &f, 1));
        let (req_tx, req_rx) = std_mpsc::channel::<Request>();
        let web = Web::new(view_rx, req_tx, None, "test board");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(web)).await });

        let (head, mut r) = request(addr, "GET", "/", "").await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let mut body = String::new();
        r.read_to_string(&mut body).await.unwrap();
        assert!(body.contains("\"device\":true") && body.contains("test board"), "{body}");

        let id = game_id(1, 1);
        let (_, mut events) = request(addr, "GET", "/api/stream/event", "").await;
        let start = json_line(&mut events).await;
        assert_eq!(
            (start["type"].as_str(), start["game"]["gameId"].as_str()),
            (Some("gameStart"), Some(&*id))
        );

        let (_, mut game) = request(addr, "GET", &format!("/api/board/game/stream/{id}"), "").await;
        let full = json_line(&mut game).await;
        assert_eq!(full["type"], "gameFull");
        assert_eq!(full["state"]["moves"], "");

        // A move on the board.
        let mut f2 = Follower::new();
        f2.play("e2e4").unwrap();
        view_tx.send(view(&c, &f2, 1)).unwrap();
        let state = json_line(&mut game).await;
        assert_eq!((state["type"].as_str(), state["moves"].as_str()), (Some("gameState"), Some("e2e4")));

        // A move from the app is refused: it's made on the board.
        let (head, _) = request(addr, "POST", &format!("/api/board/game/{id}/move/e7e5"), "").await;
        assert!(head.starts_with("HTTP/1.1 400"), "{head}");

        // A new game set up on the board ends this one and starts the next.
        view_tx.send(view(&c, &Follower::new(), 2)).unwrap();
        let end = json_line(&mut game).await;
        assert_eq!(end["status"], "aborted");
        let finish = json_line(&mut events).await;
        assert_eq!(finish["type"], "gameFinish");
        let next = json_line(&mut events).await;
        assert_eq!(
            (next["type"].as_str(), next["game"]["gameId"].as_str()),
            (Some("gameStart"), Some(&*game_id(1, 2)))
        );

        // Settings go to the board loop, which answers.
        let answer = std::thread::spawn(move || {
            let r = req_rx.recv().unwrap();
            assert_eq!(r.change, Change::Level(6));
            r.reply.send(Ok("level 6".into())).unwrap();
        });
        let (head, mut r) = request(addr, "POST", "/api/device", "level=6").await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let mut body = String::new();
        r.read_to_string(&mut body).await.unwrap();
        assert!(body.contains("level 6"), "{body}");
        answer.join().unwrap();

        let (head, _) = request(addr, "POST", "/api/device", "level=9").await;
        assert!(head.starts_with("HTTP/1.1 400"), "{head}");
    }

    #[test]
    fn app_paths_stay_inside_the_app() {
        let d = Path::new("/srv/app");
        assert_eq!(resolve(d, ""), Some(PathBuf::from("/srv/app/index.html")));
        assert_eq!(resolve(d, "pkg/engine.wasm"), Some(PathBuf::from("/srv/app/pkg/engine.wasm")));
        assert_eq!(resolve(d, "../etc/passwd"), None);
        assert_eq!(resolve(d, ".git/config"), None);
    }
}
