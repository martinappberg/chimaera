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
        let params = RasterParams {
            scale: req.scale.unwrap_or(2.0).clamp(0.25, 4.0),
            workspace: Some(ws.clone()),
        };

        let canonical = chimaera_board::to_string(&board)?;
        let key = chimaera_board::render::render_key(&canonical, &theme, req.page, params.clone());
        let dir = chimaera_board::board_dir(&ws).join("renders");
        std::fs::create_dir_all(&dir)?;
        let png_path = dir.join(format!("{key}.png"));
        // Render diagnostics are part of the same pure function as the pixels,
        // so they persist beside them — a cache hit that silently dropped the
        // sub-floor errors would make warnings vanish on every reload.
        let sidecar = png_path.with_extension("json");

        let (width, height) = match read_sidecar(&sidecar, &png_path) {
            Some((w, h, cached)) => {
                diagnostics.extend(cached);
                (w, h)
            }
            None => {
                // Only a miss pays for the font stack — building one walks
                // every system font directory, which is not hit-path work on
                // a shared login node.
                let fonts = FontStack::for_workspace(&ws);
                let out = render_page(&board, req.page, &theme, &fonts, params)?;
                chimaera_board::write_atomic(&png_path, &out.png)?;
                write_sidecar(&sidecar, out.width, out.height, &out.diagnostics);
                chimaera_board::prune_renders(&dir, chimaera_board::RENDER_CACHE_CAP);
                diagnostics.extend(out.diagnostics);
                (out.width, out.height)
            }
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
        let ws = chimaera_board::workspace_root(&path);
        let journal =
            chimaera_board::journal::summary(&chimaera_board::journal::journal_path(&ws, &path));
        let text = chimaera_board::describe::describe_with_journal(&board, journal);
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
        let mut prior = None;
        for page in &mut board.pages {
            // Top-level objects only: group children are page-absolute too,
            // but moving one without its siblings is a different gesture
            // (enter-the-group), which the pane does not offer yet.
            for obj in &mut page.objects {
                if obj.id() != req.object {
                    continue;
                }
                found = true;
                prior = obj.frame();
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

        let journal_seq = journal_edit(&path, &board, &req, prior);

        let meta = std::fs::metadata(&path)?;
        let mut response = json!({
            "mtime": fs::mtime_token(&meta),
            "path": path.to_string_lossy(),
        });
        if let Some(seq) = journal_seq {
            response["journalSeq"] = json!(seq);
        }
        Ok(response)
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

/// Append the gesture to the board's semantic edit journal, actor `human` —
/// the pane's edit route is the human's hand. Best-effort by design: the
/// board file is truth and the journal is the audit trail, so a journal
/// failure warns and returns `None` rather than failing an edit that already
/// saved. Returns the last appended seq (the resize's, when both moved and
/// resized) for the additive `journalSeq` response field.
fn journal_edit(
    path: &std::path::Path,
    board: &chimaera_board::Board,
    req: &EditRequest,
    prior: Option<chimaera_board::schema::Frame>,
) -> Option<u64> {
    use chimaera_board::journal::{Actor, Event, EventKind};

    // The journaled `to` is the *saved* geometry (post-normalize grid snap),
    // not the requested one — the journal narrates the file, never the wire.
    let saved = board
        .objects()
        .find(|(_, o)| o.id() == req.object)
        .and_then(|(_, o)| o.frame())?;
    let mut events = Vec::new();
    if req.at.is_some() {
        events.push(Event::new(
            Actor::Human,
            EventKind::Move {
                object: req.object.clone(),
                from: prior.map(|f| [f.x, f.y]).unwrap_or([saved.x, saved.y]),
                to: [saved.x, saved.y],
            },
        ));
    }
    if req.size.is_some() {
        events.push(Event::new(
            Actor::Human,
            EventKind::Resize {
                object: req.object.clone(),
                from: prior.map(|f| [f.w, f.h]).unwrap_or([saved.w, saved.h]),
                to: [saved.w, saved.h],
            },
        ));
    }
    if events.is_empty() {
        return None;
    }

    let ws = chimaera_board::workspace_root(path);
    // First use mints the surround's .gitignore so journals stay out of git.
    if let Err(err) = chimaera_board::ensure_board_dir(&ws) {
        tracing::warn!(?err, "board dir setup failed; skipping journal");
        return None;
    }
    let journal_path = chimaera_board::journal::journal_path(&ws, path);
    let appended = chimaera_board::journal::Journal::open(&journal_path)
        .and_then(|mut journal| journal.append_batch(events));
    match appended {
        Ok(seqs) => seqs.last().copied(),
        Err(err) => {
            tracing::warn!(?err, path = %journal_path.display(), "board journal append failed");
            None
        }
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

/// The persisted half of a render's output: dimensions plus diagnostics,
/// written beside the PNG under the same content-addressed key.
#[derive(serde::Serialize, Deserialize)]
struct RenderSidecar {
    width: u32,
    height: u32,
    diagnostics: Vec<SidecarDiag>,
}

#[derive(serde::Serialize, Deserialize)]
struct SidecarDiag {
    severity: String,
    #[serde(default)]
    page: Option<String>,
    #[serde(default)]
    object: Option<String>,
    #[serde(default)]
    field: Option<String>,
    message: String,
}

/// A cache hit needs both halves intact; a missing or unreadable sidecar (or
/// PNG) degrades to a re-render, never to serving broken state.
fn read_sidecar(
    sidecar: &std::path::Path,
    png_path: &std::path::Path,
) -> Option<(u32, u32, Vec<chimaera_board::Diagnostic>)> {
    if !png_path.exists() {
        return None;
    }
    let raw = std::fs::read_to_string(sidecar).ok()?;
    let parsed: RenderSidecar = serde_json::from_str(&raw).ok()?;
    let diags = parsed
        .diagnostics
        .into_iter()
        .map(|d| {
            let severity = match d.severity.as_str() {
                "error" => chimaera_board::Severity::Error,
                "warning" => chimaera_board::Severity::Warning,
                _ => chimaera_board::Severity::Info,
            };
            let mut diag = chimaera_board::Diagnostic::new(severity, d.message);
            diag.page = d.page;
            diag.object = d.object;
            diag.field = d.field;
            diag
        })
        .collect();
    Some((parsed.width, parsed.height, diags))
}

/// Best-effort: a failed sidecar write costs a re-render on the next hit,
/// nothing more.
fn write_sidecar(
    sidecar: &std::path::Path,
    width: u32,
    height: u32,
    diagnostics: &[chimaera_board::Diagnostic],
) {
    let payload = RenderSidecar {
        width,
        height,
        diagnostics: diagnostics
            .iter()
            .map(|d| SidecarDiag {
                severity: d.severity.label().to_string(),
                page: d.page.clone(),
                object: d.object.clone(),
                field: d.field.clone(),
                message: d.message.clone(),
            })
            .collect(),
    };
    if let Ok(bytes) = serde_json::to_vec(&payload) {
        let _ = chimaera_board::write_atomic(sidecar, &bytes);
    }
}
