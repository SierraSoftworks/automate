//! Serving of the single-page admin UI.
//!
//! The compiled UI (produced by `trunk build` in the `ui` crate) is embedded
//! into the binary at compile time. Any request that does not match an API,
//! webhook, or OAuth route is served from these embedded assets, falling back to
//! `index.html` so that client-side routing works on deep links.

use actix_web::{HttpRequest, HttpResponse, http::header::ContentType};
use include_dir::{Dir, include_dir};

/// The embedded output of the UI's `trunk build`.
static ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../ui/dist");

/// Serves a static asset by path, or falls back to the SPA shell (`index.html`)
/// for unknown paths so that client-side routes resolve on a hard refresh.
pub async fn serve(req: HttpRequest) -> HttpResponse {
    let path = req.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match ASSETS.get_file(path) {
        Some(file) => asset_response(path, file.contents()),
        None => index_response(),
    }
}

/// Builds a response for an embedded asset, inferring its content type from the
/// file extension.
fn asset_response(path: &str, contents: &'static [u8]) -> HttpResponse {
    let content_type = match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    };

    HttpResponse::Ok()
        .insert_header((actix_web::http::header::CONTENT_TYPE, content_type))
        .body(contents)
}

/// Serves the SPA shell, or a minimal placeholder if the UI has not been built.
fn index_response() -> HttpResponse {
    match ASSETS.get_file("index.html") {
        Some(file) => HttpResponse::Ok()
            .content_type(ContentType::html())
            .body(file.contents()),
        None => HttpResponse::InternalServerError()
            .content_type(ContentType::html())
            .body("<!DOCTYPE html><title>Automate</title><p>The user interface has not been built.</p>"),
    }
}
