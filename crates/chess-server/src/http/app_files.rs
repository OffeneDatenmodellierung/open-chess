//! The Chess lab training app (`web/chess`, built with its wasm engine),
//! served as static files under `/app/` from `CHESS_APP_DIR`.
//!
//! Serving it from this server puts the app and the API on one origin, so
//! signing in, photo reading and the position dictionary need no
//! cross-origin calls. The app caches itself for offline use with a
//! service worker; the page and the service worker are always revalidated
//! so a new release reaches phones on their next visit.

use std::path::{Component, Path, PathBuf};

use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};

use crate::app::Ctx;

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
        "md" => "text/markdown; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// The file for a URL path inside the app, or `None` if the path tries to
/// leave the app directory or names something hidden.
pub fn resolve(dir: &Path, tail: &str) -> Option<PathBuf> {
    let tail = tail.trim_start_matches('/');
    let rel =
        if tail.is_empty() || tail.ends_with('/') { format!("{tail}index.html") } else { tail.to_string() };
    let rel = Path::new(&rel);
    let mut out = dir.to_path_buf();
    for c in rel.components() {
        match c {
            Component::Normal(part) => {
                if part.to_str().is_none_or(|s| s.starts_with('.')) {
                    return None;
                }
                out.push(part);
            }
            _ => return None,
        }
    }
    Some(out)
}

/// `GET /app/{*path}`.
pub async fn file(State(ctx): State<Ctx>, UrlPath(tail): UrlPath<String>) -> Response {
    serve(&ctx, &tail).await
}

/// Where `CHESS_APP_THEME` points: CSS itself, a theme the app bundles
/// (`themes/{name}.css` in the app, by name), or a .css file.
#[derive(Debug, PartialEq)]
enum Theme<'a> {
    Css(&'a str),
    Bundled(&'a str),
    File(&'a str),
}

fn theme_source(t: &str) -> Theme<'_> {
    if t.contains('{') {
        Theme::Css(t)
    } else if !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        Theme::Bundled(t)
    } else {
        Theme::File(t)
    }
}

/// `GET /app/theme.css`: the deployment's look (`CHESS_APP_THEME`: a
/// bundled theme's name, CSS, or a file of it), empty when there is none.
pub async fn theme(State(ctx): State<Ctx>) -> Response {
    let read = |path: PathBuf, t: &str| {
        let t = t.to_owned();
        async move {
            tokio::fs::read_to_string(&path).await.unwrap_or_else(|e| {
                tracing::warn!("CHESS_APP_THEME {t} ({}): {e}", path.display());
                String::new()
            })
        }
    };
    let css = match ctx.cfg.app_theme.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        None => String::new(),
        Some(t) => match theme_source(t) {
            Theme::Css(css) => css.to_owned(),
            Theme::File(f) => read(PathBuf::from(f), t).await,
            Theme::Bundled(name) => {
                if let Some(dir) = ctx.cfg.app_dir.as_deref().filter(|d| !d.is_empty()) {
                    read(Path::new(dir).join("themes").join(format!("{name}.css")), t).await
                } else {
                    tracing::warn!("CHESS_APP_THEME {name}: a bundled theme needs the app (CHESS_APP_DIR)");
                    String::new()
                }
            }
        },
    };
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/css; charset=utf-8"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    (h, css).into_response()
}

/// `GET /app/`.
pub async fn index(State(ctx): State<Ctx>) -> Response {
    serve(&ctx, "").await
}

/// `GET /app`: on to `/app/`, so relative links resolve inside it.
pub async fn bare(State(ctx): State<Ctx>) -> Response {
    let base = ctx.cfg.base_path().unwrap_or_default();
    Redirect::permanent(&format!("{base}/app/")).into_response()
}

async fn serve(ctx: &Ctx, tail: &str) -> Response {
    let Some(dir) = ctx.cfg.app_dir.as_deref().filter(|d| !d.is_empty()) else {
        return (StatusCode::NOT_FOUND, "the training app isn't installed on this server (CHESS_APP_DIR)")
            .into_response();
    };
    let Some(path) = resolve(Path::new(dir), tail) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(body) = tokio::fs::read(&path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type(&path)));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    // The page, the service worker and its manifest are checked on every
    // visit; everything else may be cached for an hour (the service worker
    // keeps its own copy for offline use and replaces it on each release).
    let cache = if matches!(name, "index.html" | "sw.js" | "manifest.webmanifest") {
        "no-cache"
    } else {
        "public, max-age=3600"
    };
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    if name == "sw.js" {
        // The worker controls the whole app directory.
        let base = ctx.cfg.base_path().unwrap_or_default();
        if let Ok(v) = HeaderValue::from_str(&format!("{base}/app/")) {
            h.insert("service-worker-allowed", v);
        }
    }
    (h, body).into_response()
}

/// Whether a request for `/` came from a browser (rather than an API
/// client), so it can be sent to the app.
pub fn wants_html(headers: &HeaderMap) -> bool {
    headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()).is_some_and(|a| a.contains("text/html"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_the_app() {
        let d = Path::new("/srv/app");
        assert_eq!(resolve(d, ""), Some(PathBuf::from("/srv/app/index.html")));
        assert_eq!(resolve(d, "pkg/engine.wasm"), Some(PathBuf::from("/srv/app/pkg/engine.wasm")));
        assert_eq!(resolve(d, "fonts/"), Some(PathBuf::from("/srv/app/fonts/index.html")));
        assert_eq!(resolve(d, "../etc/passwd"), None);
        assert_eq!(resolve(d, "pkg/../../x"), None);
        assert_eq!(resolve(d, ".git/config"), None);
        assert_eq!(resolve(d, "/etc/passwd"), Some(PathBuf::from("/srv/app/etc/passwd")));
    }

    #[test]
    fn theme_sources() {
        assert_eq!(theme_source("esports-lab"), Theme::Bundled("esports-lab"));
        assert_eq!(theme_source(":root { --ink: #000 }"), Theme::Css(":root { --ink: #000 }"));
        assert_eq!(theme_source("/etc/chess/theme.css"), Theme::File("/etc/chess/theme.css"));
        assert_eq!(theme_source("theme.css"), Theme::File("theme.css"));
        // No way out of themes/ by name.
        assert_eq!(theme_source("../x"), Theme::File("../x"));
    }

    #[test]
    fn bundled_themes_exist() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/chess/themes");
        let css = std::fs::read_to_string(dir.join("esports-lab.css")).unwrap();
        assert!(css.contains("--paper"));
    }

    #[test]
    fn types() {
        assert_eq!(content_type(Path::new("a.wasm")), "application/wasm");
        assert_eq!(content_type(Path::new("manifest.webmanifest")), "application/manifest+json");
        assert_eq!(content_type(Path::new("app.js")), "text/javascript; charset=utf-8");
    }
}
