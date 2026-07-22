//! Board routes: the pane's pixel source and the agent's read-back.
//!
//! Thin wrappers over the same `chimaera-board` crate functions the CLI
//! calls — a second render path is how the pane and the export stop agreeing.
//! Both routes sit behind the bearer middleware; the PNG itself is fetched via
//! the existing `/raw/{ticket}` capability, so no image bytes ride the JSON
//! wire and the `<img>` cache behaves exactly as it does for ordinary files.
//!
//! Renders land as content-addressed files under the workspace's
//! `.chimaera/board/renders/` — a render is a pure function of board bytes,
//! theme and raster params, so a cache hit is correct by construction and the
//! cache needs pruning, never invalidation.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::{fs, AppState};
use chimaera_board::layout::FontStack;
use chimaera_board::render::{render_page, RasterParams};
use chimaera_board::theme::Theme;

#[derive(Deserialize)]
pub(crate) struct RenderRequest {
    /// Absolute path (or `~/…`) of the `.board.json`.
    pub path: String,
    /// 0-based page index; defaults to the first page.
    #[serde(default)]
    pub page: usize,
    /// Device scale; the pane asks at its own DPR. Clamped to [0.25, 4].
    #[serde(default)]
    pub scale: Option<f64>,
    /// Theme id or path, overriding the board's own.
    #[serde(default)]
    pub theme: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct DescribeRequest {
    pub path: String,
}

/// One semantic gesture from the pane: move/resize an object by id.
///
/// The pane never serializes a board itself — a client-side
/// `JSON.stringify` would destroy the canonical byte-stable form and churn
/// every diff — so a gesture routes through here, where the crate's writer is
/// the one authority on bytes.
#[derive(Deserialize)]
pub(crate) struct EditRequest {
    pub path: String,
    /// The object's id — the same id that is the diff anchor, the journal
    /// subject, and the merge key.
    pub object: String,
    #[serde(default)]
    pub at: Option<[f64; 2]>,
    #[serde(default)]
    pub size: Option<[f64; 2]>,
}

/// POST /api/v1/board/render → `{ticket, width, height, pageCount, pages,
/// diagnostics}`. The PNG is fetched as `/raw/{ticket}`.
pub(crate) async fn render(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RenderRequest>,
) -> Response {
    // The render (usvg + resvg + PNG encode) is CPU-bound and the board read
    // may cross NFS, so it runs under the same blocking semaphore as every
    // other preview.
    let outcome = fs::blocking_value(move || {
        let path = resolve_board_path(&req.path)?;
        let ws = chimaera_board::workspace_root(&path);

        let mut board = chimaera_board::load(&path)?;
        let mut diagnostics = chimaera_board::normalize(&mut board);
        let page_count = board.pages.len();

        let theme_name = req
            .theme
            .clone()
            .or_else(|| board.theme.clone())
            .unwrap_or_else(|| "talk-dark".to_string());
        let theme = Theme::resolve(&theme_name, Some(&ws))?;
        let fonts = FontStack::for_workspace(&ws);
        let params = RasterParams {
            scale: req.scale.unwrap_or(2.0).clamp(0.25, 4.0),
        };

        let canonical = chimaera_board::to_string(&board)?;
        let key = chimaera_board::render::render_key(&canonical, &theme, req.page, params);
        let dir = chimaera_board::board_dir(&ws).join("renders");
        std::fs::create_dir_all(&dir)?;
        let png_path = dir.join(format!("{key}.png"));

        let (width, height) = if png_path.exists() {
            // Content-addressed hit: dimensions come cheap from the fixed
            // IHDR offsets of our own encoder's output.
            let bytes = std::fs::read(&png_path)?;
            png_dimensions(&bytes).unwrap_or((0, 0))
        } else {
            let out = render_page(&board, req.page, &theme, &fonts, params)?;
            std::fs::write(&png_path, &out.png)?;
            diagnostics.extend(out.diagnostics);
            (out.width, out.height)
        };

        Ok(json!({
            "pngPath": png_path,
            "width": width,
            "height": height,
            "pageCount": page_count,
            "pages": board.pages.iter().map(|p| p.id.clone()).collect::<Vec<_>>(),
            "diagnostics": diagnostics
                .iter()
                .map(|d| json!({
                    "severity": d.severity.label(),
                    "object": d.object,
                    "message": d.message,
                    "rendered": d.render(),
                }))
                .collect::<Vec<_>>(),
        }))
    })
    .await;

    match outcome {
        Ok(mut value) => {
            // Swap the private filesystem path for a /raw ticket — minted
            // here, after the blocking work, where the brief state lock is
            // safe.
            if let Some(path) = value
                .get("pngPath")
                .and_then(|p| p.as_str())
                .map(PathBuf::from)
            {
                let ticket = crate::lock(&state.tickets).create(path, fs::TICKET_TTL);
                if let Some(obj) = value.as_object_mut() {
                    obj.remove("pngPath");
                    obj.insert("ticket".into(), json!(ticket));
                }
            }
            Json(value).into_response()
        }
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("{err:#}")})),
        )
            .into_response(),
    }
}

/// POST /api/v1/board/describe → `{text}`, the agent-facing read-back.
pub(crate) async fn describe(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<DescribeRequest>,
) -> Response {
    fs::blocking_json(move || {
        let path = resolve_board_path(&req.path)?;
        let mut board = chimaera_board::load(&path)?;
        let diagnostics = chimaera_board::normalize(&mut board);
        let text = chimaera_board::describe::describe(&board);
        Ok(json!({
            "text": text,
            "diagnostics": diagnostics.iter().map(|d| d.render()).collect::<Vec<_>>(),
        }))
    })
    .await
}

/// POST /api/v1/board/edit → `{mtime}`. Applies the gesture, renormalizes,
/// saves canonically, and returns the new `X-Mtime` token so the fileStore
/// adopts the write as its own rather than treating it as external.
pub(crate) async fn edit(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EditRequest>,
) -> Response {
    let outcome = fs::blocking_value(move || {
        let path = resolve_board_path(&req.path)?;
        let mut board = chimaera_board::load(&path)?;

        let mut found = false;
        for page in &mut board.pages {
            // Top-level objects only: group children are page-absolute too,
            // but moving one without its siblings is a different gesture
            // (enter-the-group), which the pane does not offer yet.
            for obj in &mut page.objects {
                if obj.id() != req.object {
                    continue;
                }
                found = true;
                if let Some(at) = req.at {
                    obj.set_at(at);
                }
                if let Some(size) = req.size {
                    obj.set_size(size);
                }
            }
        }
        if !found {
            anyhow::bail!("no object {:?} in {}", req.object, path.display());
        }

        // Normalize (grid snap, group re-union) before the canonical save —
        // the same pipeline an agent edit goes through, so a human drag and
        // an agent Edit produce bytes of identical shape.
        chimaera_board::normalize(&mut board);
        chimaera_board::save(&path, &board)?;

        let meta = std::fs::metadata(&path)?;
        Ok(json!({
            "mtime": fs::mtime_token(&meta),
            "path": path.to_string_lossy(),
        }))
    })
    .await;

    match outcome {
        Ok(mut value) => {
            // Nudge the git watcher so the FILES tree's dirty badge follows a
            // drag without waiting for the poll.
            if let Some(p) = value.get("path").and_then(|p| p.as_str()) {
                crate::git::mark_path_dirty(&state, p).await;
            }
            if let Some(obj) = value.as_object_mut() {
                obj.remove("path");
            }
            Json(value).into_response()
        }
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("{err:#}")})),
        )
            .into_response(),
    }
}

fn resolve_board_path(raw: &str) -> anyhow::Result<PathBuf> {
    let path = fs::canonical_file(raw)?;
    if !chimaera_board::is_board_path(&path) {
        anyhow::bail!(
            "not a board: {} does not end in .board.json",
            path.display()
        );
    }
    Ok(path)
}

/// Width and height from a PNG's IHDR — always at bytes 16..24 of a
/// well-formed file, which ours are (we wrote them).
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let be = |s: &[u8]| u32::from_be_bytes([s[0], s[1], s[2], s[3]]);
    Some((be(&bytes[16..20]), be(&bytes[20..24])))
}
