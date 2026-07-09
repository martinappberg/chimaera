use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rust_embed::RustEmbed;
use serde_json::json;

#[derive(RustEmbed)]
#[folder = "../../web-ui/dist"]
struct Assets;

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
    Some(
        (
            [(header::CONTENT_TYPE, mime.as_ref())],
            file.data.into_owned(),
        )
            .into_response(),
    )
}
