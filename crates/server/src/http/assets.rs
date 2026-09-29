//! The embedded frontend: the pages, `tokens.css`, the shared chrome and the
//! vendored scripts. Nothing here is read from disk at runtime.

use axum::extract::Path;
use axum::http::header;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;

/// The files under `static/`, embedded at build time (see build.rs).
mod embedded {
    include!(concat!(env!("OUT_DIR"), "/assets.rs"));
}

/// Looks up an embedded asset by its path relative to `static/`.
fn asset(path: &str) -> Option<&'static [u8]> {
    embedded::ASSETS
        .iter()
        .find(|(p, _)| *p == path)
        .map(|(_, data)| *data)
}

/// Content-Type for an embedded asset, by extension.
fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json; charset=utf-8",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// Serves an embedded asset, or 404 if there's no such file. Unknown paths
/// (including any `..` traversal) simply don't match an embedded key.
pub(super) fn serve_asset(path: &str) -> Response {
    match asset(path) {
        Some(bytes) => ([(header::CONTENT_TYPE, content_type(path))], bytes).into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

pub(super) async fn index() -> Response {
    serve_asset("index.html")
}

pub(super) async fn analytics_page() -> Response {
    serve_asset("analytics.html")
}

pub(super) async fn transcripts_page() -> Response {
    serve_asset("transcripts.html")
}

pub(super) async fn tokens_css() -> Response {
    serve_asset("tokens.css")
}

pub(super) async fn vendor(Path(path): Path<String>) -> Response {
    serve_asset(&format!("vendor/{path}"))
}

/// Brand assets (logo mark, favicon). Public so the login page can show the
/// mark before a teacher is authenticated.
pub(super) async fn brand_asset(Path(path): Path<String>) -> Response {
    serve_asset(&format!("assets/{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every teacher page's header links these; a missing one leaves the menu
    /// button dead and unstyled, and only in a built binary, not in the fixtures
    /// server.
    #[test]
    fn the_shared_page_chrome_ships_with_the_binary() {
        for (path, content_type) in [
            ("assets/common.js", "application/javascript; charset=utf-8"),
            ("assets/chrome.css", "text/css; charset=utf-8"),
            (
                "assets/attention.js",
                "application/javascript; charset=utf-8",
            ),
            ("assets/since.js", "application/javascript; charset=utf-8"),
        ] {
            assert!(
                asset(path).is_some_and(|b| !b.is_empty()),
                "{path} is embedded"
            );
            assert_eq!(super::content_type(path), content_type);
        }
        for page in ["index.html", "analytics.html", "transcripts.html"] {
            let html = String::from_utf8(asset(page).unwrap().to_vec()).unwrap();
            for link in ["/assets/chrome.css", "/assets/common.js"] {
                assert!(html.contains(link), "{page} links {link}");
            }
        }
        let board = String::from_utf8(asset("index.html").unwrap().to_vec()).unwrap();
        for script in ["/assets/attention.js", "/assets/since.js"] {
            assert!(board.contains(script), "the board loads {script}");
        }
    }
}
