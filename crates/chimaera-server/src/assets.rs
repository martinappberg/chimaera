use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rust_embed::RustEmbed;
use serde_json::json;

#[derive(RustEmbed)]
#[folder = "../../web-ui/dist"]
struct Assets;

/// Serve embedded UI files for every non-/api path, with SPA fallback to index.html.
pub(crate) async fn static_handler(uri: Uri) -> Response {
    let path = uri.path();
    if path.starts_with("/api") {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response();
    }

    let trimmed = path.trim_start_matches('/');
    let candidate = if trimmed.is_empty() {
        "index.html"
    } else {
        trimmed
    };

    serve(candidate)
        .or_else(|| serve("index.html"))
        .unwrap_or_else(|| {
            (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response()
        })
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
