use axum::http::header;
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../web-ui/dist"]
struct Assets;

const BUILD_PLACEHOLDER: &str = "__CHIMAERA_BUILD_ID__";

/// Serve one embedded UI file, if it exists. The routing rules around it —
/// SPA fallback to index.html, the proxy rescue, the /assets 404 rule — live
/// in `proxy::fallback` (the router's fallback handler).
pub(crate) fn try_serve(path: &str) -> Option<Response> {
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
