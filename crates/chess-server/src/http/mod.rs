//! HTTP handlers, grouped as Lichess's API docs group them.

pub mod account;
pub mod admin;
pub mod app_files;
pub mod challenges;
pub mod fairplay;
pub mod feed;
pub mod games;
pub mod lab;
pub mod mobile;
pub mod oauth;
pub mod play;
pub mod streams;
pub mod web;

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

/// The open-chess logo (the Chess lab's icon, `web/chess/icon.svg`).
pub const LOGO_SVG: &str = include_str!("logo.svg");

/// The logo at `size` pixels, inline in a page.
pub fn logo(size: u32) -> String {
    LOGO_SVG.replacen("<svg ", &format!("<svg width=\"{size}\" height=\"{size}\" aria-hidden=\"true\" "), 1)
}

/// `GET /favicon.svg` and `/favicon.ico`: the logo (browsers take an SVG
/// whatever the name).
pub async fn favicon() -> Response {
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("image/svg+xml")),
            (header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=86400")),
        ],
        LOGO_SVG,
    )
        .into_response()
}

/// `GET /docs` and `/docs/{guide}`: the guides, shown in the Chess lab
/// (`/app/#docs/…`) when it's installed, otherwise on GitHub.
pub async fn docs(State(ctx): State<Ctx>, guide: Option<axum::extract::Path<String>>) -> Response {
    let name = guide
        .map(|g| g.0.trim_end_matches(".md").to_owned())
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    let to = if ctx.cfg.app_dir.as_deref().is_some_and(|d| !d.is_empty()) {
        let base = ctx.cfg.base_path().unwrap_or_default();
        match name {
            Some(n) => format!("{base}/app/#docs/{n}"),
            None => format!("{base}/app/#docs"),
        }
    } else {
        let repo = "https://github.com/OffeneDatenmodellierung/open-chess/blob/main/docs";
        match name {
            Some(n) => format!("{repo}/{n}.md"),
            None => repo.to_owned(),
        }
    };
    axum::response::Redirect::to(&to).into_response()
}

/// `GET /@/{username}`: a player's profile, in the Chess lab.
pub async fn profile(
    State(ctx): State<Ctx>,
    axum::extract::Path(username): axum::extract::Path<String>,
) -> Response {
    let base = ctx.cfg.base_path().unwrap_or_default();
    axum::response::Redirect::to(&format!("{base}/app/#@/{}", urlencoding::encode(&username))).into_response()
}

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
        // Browser sign-in (the OAuth page, the Account page): always, with
        // this server's own accounts; "signup" whether anyone may make one;
        // "esports" whether the host site's players sign in with it.
        "login": true,
        "signup": ctx.cfg.signup,
        "esports": ctx.cfg.esports().is_some(),
        "app": ctx.cfg.app_dir.as_deref().is_some_and(|d| !d.is_empty()).then(|| format!("{}/app/", ctx.cfg.public_url())),
        "docs": format!("{}/docs", ctx.cfg.public_url()),
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
    fn logo_is_the_apps_icon() {
        // In the repository (not in a published crate) the copy here must
        // match the Chess lab's icon.
        let app = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/chess/icon.svg");
        if let Ok(icon) = std::fs::read_to_string(app) {
            assert_eq!(icon, LOGO_SVG, "copy web/chess/icon.svg to crates/chess-server/src/http/logo.svg");
        }
        assert!(logo(36).starts_with("<svg width=\"36\" height=\"36\""));
    }

    #[test]
    fn lines_end_in_newline() {
        assert_eq!(&line(&json!({"a": 1}))[..], b"{\"a\":1}\n");
    }
}
