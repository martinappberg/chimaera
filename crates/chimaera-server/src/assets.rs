use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rust_embed::RustEmbed;
use serde_json::json;

#[derive(RustEmbed)]
#[folder = "../../web-ui/dist"]
struct Assets;

const BUILD_PLACEHOLDER: &str = "__CHIMAERA_BUILD_ID__";

/// Serve embedded UI files for every non-/api path, with SPA fallback to
/// index.html for client-side routes — but NOT for missing hashed asset chunks.
pub(crate) async fn static_handler(uri: Uri) -> Response {
    let path = uri.path();
    if path.starts_with("/api") {
        return not_found();
    }

    let trimmed = path.trim_start_matches('/');
    let candidate = if trimmed.is_empty() {
        "index.html"
    } else {
        trimmed
    };

    if let Some(resp) = serve(candidate) {
        return resp;
    }
    // A missing hashed build chunk under /assets/ must 404, not fall back to
    // index.html. A browser holding a stale index.html after a redeploy would
    // otherwise get HTML (200, text/html) for an old `/assets/index-*.js`, fail
    // to parse it as a module, and break silently with no signal to hard-reload.
    // SPA routes (extension-less paths like /workspace/foo) still get index.html
    // so client-side routing works.
    if path.starts_with("/assets/") {
        return not_found();
    }
    serve("index.html").unwrap_or_else(not_found)
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response()
}

fn serve(path: &str) -> Option<Response> {
    let file = Assets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    // The entry document carries the identity of the exact asset graph that
    // supplied it. A later health response cannot provide this safely: the
    // daemon may have handed off between the document and that first poll.
    let body = if path == "index.html" {
        String::from_utf8(file.data.into_owned())
            .ok()?
            .replace(BUILD_PLACEHOLDER, chimaera_core::BUILD_ID)
            .into_bytes()
    } else {
        file.data.into_owned()
    };
    // A reload after a daemon handoff must obtain the new entry document.
    // Hashed assets are the opposite: their name identifies their bytes and
    // may be cached forever. This pair makes release transitions atomic from
    // the browser's point of view.
    let cache = if path == "index.html" {
        "no-store"
    } else if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    Some(
        (
            [
                (header::CONTENT_TYPE, mime.as_ref()),
                (header::CACHE_CONTROL, cache),
            ],
            body,
        )
            .into_response(),
    )
}
