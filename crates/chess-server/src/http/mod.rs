//! HTTP handlers, grouped as Lichess's API docs group them.

pub mod account;
pub mod app_files;
pub mod challenges;
pub mod fairplay;
pub mod games;
pub mod lab;
pub mod oauth;
pub mod play;
pub mod streams;

use std::convert::Infallible;

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde::Serialize;
use serde_json::json;
use tokio::sync::mpsc;

use crate::app::Ctx;

/// `GET /`: what this is. Browsers go to the training app when it's
/// installed.
pub async fn index(State(ctx): State<Ctx>, headers: axum::http::HeaderMap) -> Response {
    if app_files::wants_html(&headers) && ctx.cfg.app_dir.as_deref().is_some_and(|d| !d.is_empty()) {
        let base = ctx.cfg.base_path().unwrap_or_default();
        return axum::response::Redirect::to(&format!("{base}/app/")).into_response();
    }
    Json(json!({
        "name": "open-chess",
        "site": ctx.cfg.site_name,
        "api": "Lichess-compatible: Board, Bot, Challenge, OAuth (PKCE), event and game streams, game export",
        "url": ctx.cfg.public_url(),
        "version": env!("CARGO_PKG_VERSION"),
        // Whether the OAuth page signs people in (through the host site).
        "login": ctx.cfg.esports_login,
        "app": ctx.cfg.app_dir.as_deref().is_some_and(|d| !d.is_empty()).then(|| format!("{}/app/", ctx.cfg.public_url())),
    }))
    .into_response()
}

pub fn ok() -> Response {
    Json(chess_proto::OK).into_response()
}

/// One NDJSON line.
pub fn line<T: Serialize>(v: &T) -> Bytes {
    let mut b = serde_json::to_vec(v).unwrap_or_default();
    b.push(b'\n');
    Bytes::from(b)
}

/// The response half of an NDJSON stream; a task writes lines into the
/// sender and stops when sending fails (the client went away).
pub fn ndjson(rx: mpsc::Receiver<Bytes>) -> Response {
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|b| (Ok::<Bytes, Infallible>(b), rx))
    });
    let mut r = Body::from_stream(stream).into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/x-ndjson"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    // Ask proxies (nginx ingress) not to buffer the stream.
    h.insert("x-accel-buffering", HeaderValue::from_static("no"));
    r
}

/// A urlencoded body (missing or empty is fine: no fields).
pub fn form(body: &[u8]) -> std::collections::HashMap<String, String> {
    url::form_urlencoded::parse(body).into_owned().collect()
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes() {
        assert_eq!(html_escape("<a href=\"x\">&'"), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;");
    }

    #[test]
    fn lines_end_in_newline() {
        assert_eq!(&line(&json!({"a": 1}))[..], b"{\"a\":1}\n");
    }
}
