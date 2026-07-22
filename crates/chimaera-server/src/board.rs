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

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::{fs, AppState};
use chimaera_board::layout::FontStack;
use chimaera_board::render::{render_page, RasterParams};
use chimaera_board::theme::Theme;

/// Journal page ceiling for GET /board/journal: oldest-first after `since`,
/// so a client pages forward by re-asking with the last seq it saw;
/// `latestSeq` tells it when it is caught up. The journal file itself is
/// size-capped, so this bounds one response, not total history.
const JOURNAL_PAGE_CAP: usize = 500;

/// How long after the last board gesture the deferred git bump fires. Board
/// edits arrive one per pointer-up; bumping the git epoch per gesture makes
/// every window refetch `git status` (seconds on a big repo over Lustre), so
/// a layout session settles to ONE status run (board plan §7).
const GIT_SETTLE: Duration = Duration::from_millis(1000);

/// Ceiling on distinct paths with a settle timer pending. Past it the bump
/// degrades to immediate (the pre-settle behavior) rather than growing the
/// map — bounded memory beats coalescing under a pathological client.
const GIT_SETTLE_MAX_PENDING: usize = 128;

/// Per-path write serialization for the mutating board routes: two concurrent
/// read-modify-write cycles on one file (edit vs. edit, or an edit's journal
/// append racing a POSTed journal event's seq stamp) would lose an update.
/// A striped lock — 16 async mutexes indexed by the canonical path's hash —
/// is bounded by construction (no per-path map to grow) and a cross-path
/// collision only costs a moment of false serialization.
const EDIT_SHARDS: usize = 16;
static EDIT_LOCKS: [tokio::sync::Mutex<()>; EDIT_SHARDS] =
    [const { tokio::sync::Mutex::const_new(()) }; EDIT_SHARDS];

fn edit_shard(path: &Path) -> &'static tokio::sync::Mutex<()> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    &EDIT_LOCKS[(hasher.finish() as usize) % EDIT_SHARDS]
}

/// 400 with the same JSON error body every board route answers.
fn board_error(err: &anyhow::Error) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": format!("{err:#}")})),
    )
        .into_response()
}

/// Resolve the board path off the reactor (canonicalize can stall on NFS),
/// before any shard lock — the shard key must be the canonical path or two
/// spellings of one file would take different locks.
async fn resolve_board_path_blocking(raw: String) -> anyhow::Result<PathBuf> {
    match tokio::task::spawn_blocking(move || resolve_board_path(&raw)).await {
        Ok(result) => result,
        Err(join) => Err(anyhow::anyhow!("filesystem task failed: {join}")),
    }
}

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

/// One semantic gesture from the pane: move/resize an object by id, or
/// replace a text/shape object's text.
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
    /// Plain paragraphs replacing the whole text of a text or shape object.
    /// Bare strings only — a rich (styled-run) text survives edits exactly by
    /// NOT using this op; sending it flattens the styling by design, because
    /// the pane's inline editor edits words, not runs.
    #[serde(default)]
    pub text: Option<Vec<String>>,
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
    let path = match resolve_board_path_blocking(req.path.clone()).await {
        Ok(path) => path,
        Err(err) => return board_error(&err),
    };
    // Held across the whole read→mutate→write (and its journal append):
    // without it, two concurrent edits interleave load/save and the earlier
    // gesture is silently lost.
    let _guard = edit_shard(&path).lock().await;
    let outcome = fs::blocking_value({
        let path = path.clone();
        move || {
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
                    if let Some(text) = &req.text {
                        let paras = || {
                            text.iter()
                                .map(|s| chimaera_board::schema::Paragraph::Plain(s.clone()))
                                .collect()
                        };
                        match obj {
                            chimaera_board::Object::Text(t) => t.text = paras(),
                            chimaera_board::Object::Shape(sh) => sh.text = paras(),
                            other => anyhow::bail!(
                                "text applies to text and shape objects; {:?} is a {}",
                                req.object,
                                other.kind()
                            ),
                        }
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
            });
            if let Some(seq) = journal_seq {
                response["journalSeq"] = json!(seq);
            }
            Ok(response)
        }
    })
    .await;

    match outcome {
        Ok(value) => {
            // The pane invalidates on this immediately (invalidate-and-pull);
            // the git bump — every window's `git status` — is deferred behind
            // the per-path settle so a layout session costs one status run,
            // not one per pointer-up.
            bump_board_epoch(&state, &path);
            schedule_git_settle(state.clone(), path.to_string_lossy().into_owned());
            Json(value).into_response()
        }
        Err(err) => board_error(&err),
    }
}

/// Bump the board epoch of every workspace whose root contains `path`, then
/// wake the events bus — the board-mutation counterpart of
/// `git::mark_path_dirty`. `/ws/events` carries only
/// `{"type":"board","epochs":{…}}` and the pane refetches `/board/render`:
/// invalidate-and-pull, never payload on the firehose. `path` is canonical
/// (every mutating route resolves first) and workspace roots are stored
/// canonical, so the component-wise prefix check holds.
pub(crate) fn bump_board_epoch(state: &AppState, path: &Path) {
    let workspaces = crate::lock(&state.workspaces).list();
    let mut bumped = false;
    for ws in workspaces {
        if path.starts_with(&ws.root) {
            *crate::lock(&state.board_epochs).entry(ws.id).or_insert(0) += 1;
            bumped = true;
        }
    }
    if bumped {
        state.changes.notify_waiters();
    }
}

/// Defer a board edit's git bump behind the per-path settle timer, reset by
/// every further edit — the board-plan §7 rule that keeps a layout session at
/// one `git status` instead of one per pointer-up. One timer task per active
/// path: a follow-up gesture only pushes the deadline the running task
/// re-reads when it wakes. Past [`GIT_SETTLE_MAX_PENDING`] distinct paths the
/// bump degrades to immediate (the pre-settle behavior) rather than growing
/// the map.
fn schedule_git_settle(state: Arc<AppState>, path: String) {
    let deadline = tokio::time::Instant::now() + GIT_SETTLE;
    {
        let mut pending = crate::lock(&state.board_git_settle);
        if let Some(existing) = pending.get_mut(&path) {
            *existing = deadline;
            return;
        }
        if pending.len() >= GIT_SETTLE_MAX_PENDING {
            drop(pending);
            tokio::spawn(async move {
                crate::git::mark_path_dirty(&state, &path).await;
            });
            return;
        }
        pending.insert(path.clone(), deadline);
    }
    tokio::spawn(async move {
        let mut deadline = deadline;
        loop {
            tokio::time::sleep_until(deadline).await;
            let now = tokio::time::Instant::now();
            let pushed = {
                let mut pending = crate::lock(&state.board_git_settle);
                match pending.get(&path).copied() {
                    Some(d) if d > now => Some(d),
                    _ => {
                        pending.remove(&path);
                        None
                    }
                }
            };
            match pushed {
                Some(d) => deadline = d,
                None => break,
            }
        }
        crate::git::mark_path_dirty(&state, &path).await;
    });
}

#[derive(Deserialize)]
pub(crate) struct JournalQuery {
    pub path: String,
    /// Only events with seq strictly greater than this; 0 (the default)
    /// reads everything the size-capped journal still holds.
    #[serde(default)]
    pub since: u64,
}

/// GET /api/v1/board/journal?path=…&since=N → `{events, latestSeq}` — the
/// semantic edit history after `since`, oldest first, capped at
/// [`JOURNAL_PAGE_CAP`] entries per response (page forward by re-asking with
/// the last seq received; `latestSeq` says when the reader is caught up).
/// Events serialize exactly as the journal lines do: seq-first, kebab-case
/// op, no timestamps.
pub(crate) async fn journal(Query(query): Query<JournalQuery>) -> Response {
    fs::blocking_json(move || {
        let path = resolve_board_path(&query.path)?;
        let ws = chimaera_board::workspace_root(&path);
        let journal_path = chimaera_board::journal::journal_path(&ws, &path);
        let mut events = chimaera_board::journal::read_since(&journal_path, query.since)?;
        let latest = chimaera_board::journal::latest_seq(&journal_path)?;
        events.truncate(JOURNAL_PAGE_CAP);
        Ok(json!({
            "events": events,
            "latestSeq": latest,
        }))
    })
    .await
}

/// One journal event to append: the board it belongs to, who did it, and the
/// op in the journal file's own tagged vocabulary (`"event":"move"`, …) —
/// validated by deserialization, so an unknown op or missing actor never
/// reaches the file. `seq` is assigned server-side by the append API, never
/// by the caller. This is the hook the pane's comment-pins ride.
#[derive(Deserialize)]
pub(crate) struct JournalAppendRequest {
    pub path: String,
    pub actor: chimaera_board::journal::Actor,
    #[serde(flatten)]
    pub event: chimaera_board::journal::EventKind,
}

/// POST /api/v1/board/journal → `{seq}`.
pub(crate) async fn journal_append(
    State(state): State<Arc<AppState>>,
    Json(req): Json<JournalAppendRequest>,
) -> Response {
    let path = match resolve_board_path_blocking(req.path.clone()).await {
        Ok(path) => path,
        Err(err) => return board_error(&err),
    };
    // Seq assignment is a read-modify-append on the same journal file an
    // edit's best-effort append touches — same shard, same serialization.
    let _guard = edit_shard(&path).lock().await;
    let outcome = fs::blocking_value({
        let path = path.clone();
        move || {
            let ws = chimaera_board::workspace_root(&path);
            // First use mints the surround's .gitignore so journals stay out
            // of git.
            chimaera_board::ensure_board_dir(&ws)?;
            let journal_path = chimaera_board::journal::journal_path(&ws, &path);
            let mut journal = chimaera_board::journal::Journal::open(&journal_path)?;
            let seq = journal.append(chimaera_board::journal::Event::new(req.actor, req.event))?;
            Ok(json!({"seq": seq}))
        }
    })
    .await;
    match outcome {
        Ok(value) => {
            // A journal event is a board mutation on the plan's terms (§7):
            // announce it so other windows' overlays refetch. No git settle —
            // the journal is gitignored, so nothing tracked changed.
            bump_board_epoch(&state, &path);
            Json(value).into_response()
        }
        Err(err) => board_error(&err),
    }
}

#[derive(Deserialize)]
pub(crate) struct ExportRequest {
    pub path: String,
    /// pptx | pdf | svg | svg-outlined — the CLI's vocabulary exactly
    /// (`svg` keeps real text; `svg-outlined` flattens glyphs to paths).
    pub format: String,
    /// 0-based page for the SVG variants; all pages when omitted. Does not
    /// apply to pdf/pptx, which take the whole deck.
    #[serde(default)]
    pub page: Option<usize>,
}

/// POST /api/v1/board/export → `{ticket, filename, pageCount}` (+ `objects`,
/// the per-object fidelity fates, for pptx). Bytes land in the same
/// `.chimaera/board/exports/` the CLI writes — same exporter functions, so
/// the two cannot disagree — and the ticket rides `GET /download/{ticket}`;
/// a multi-page SVG export tickets a directory, which downloads as a zip.
pub(crate) async fn export(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ExportRequest>,
) -> Response {
    // Exporters share the render engine (text shaping, zip/pdf encode) and
    // the board read may cross NFS, so the work runs under the same blocking
    // semaphore as render.
    let outcome = fs::blocking_value(move || {
        let path = resolve_board_path(&req.path)?;
        let ws = chimaera_board::workspace_root(&path);
        let mut board = chimaera_board::load(&path)?;
        chimaera_board::normalize(&mut board);
        let theme_name = board
            .theme
            .clone()
            .unwrap_or_else(|| "talk-dark".to_string());
        let theme = Theme::resolve(&theme_name, Some(&ws))?;
        let fonts = FontStack::for_workspace(&ws);
        let stem = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.trim_end_matches(".board.json").to_string())
            .unwrap_or_else(|| "board".to_string());
        let exports_dir = chimaera_board::ensure_board_dir(&ws)?.join("exports");
        std::fs::create_dir_all(&exports_dir)?;
        let page_count = board.pages.len();

        let mut fates = None;
        let dest: PathBuf = match req.format.as_str() {
            "svg" | "svg-outlined" => {
                use chimaera_board::export::svg::{export_svg, SvgVariant};
                let (variant, base) = if req.format == "svg" {
                    (SvgVariant::Text, stem.clone())
                } else {
                    (SvgVariant::Outlined, format!("{stem}-outlined"))
                };
                let pages: Vec<usize> = match req.page {
                    Some(p) => vec![p],
                    None => (0..board.pages.len()).collect(),
                };
                if pages.len() == 1 {
                    let svg = export_svg(&board, pages[0], &theme, &fonts, Some(&ws), variant)?;
                    let dest = exports_dir.join(format!("{base}.svg"));
                    chimaera_board::write_atomic(&dest, svg.as_bytes())?;
                    dest
                } else {
                    // All pages of a multi-page board: one file per page in a
                    // per-export directory, ticketed whole (the download
                    // route zips a directory on the fly). Cleared first so a
                    // page deleted since the last export cannot ride along.
                    let dir = exports_dir.join(format!("{base}-svg"));
                    let _ = std::fs::remove_dir_all(&dir);
                    std::fs::create_dir_all(&dir)?;
                    for p in pages {
                        let svg = export_svg(&board, p, &theme, &fonts, Some(&ws), variant)?;
                        chimaera_board::write_atomic(
                            &dir.join(format!("{base}-{}.svg", board.pages[p].id)),
                            svg.as_bytes(),
                        )?;
                    }
                    dir
                }
            }
            "pdf" => {
                if req.page.is_some() {
                    anyhow::bail!(
                        "page does not apply to pdf: the whole deck exports as one document"
                    );
                }
                let pdf =
                    chimaera_board::export::pdf::export_pdf(&board, &theme, &fonts, Some(&ws))?;
                let dest = exports_dir.join(format!("{stem}.pdf"));
                chimaera_board::write_atomic(&dest, &pdf)?;
                dest
            }
            "pptx" => {
                if req.page.is_some() {
                    anyhow::bail!(
                        "page does not apply to pptx: the whole deck exports as one file"
                    );
                }
                let mut bytes = Vec::new();
                let report = chimaera_board::export::write_pptx(
                    &board,
                    &theme,
                    &fonts,
                    Some(&ws),
                    &mut bytes,
                )?;
                // The degradation contract as data — the same per-object
                // fates the CLI prints, stated before the deck is opened.
                fates = Some(serde_json::to_value(&report.objects)?);
                let dest = exports_dir.join(format!("{stem}.pptx"));
                chimaera_board::write_atomic(&dest, &bytes)?;
                dest
            }
            other => anyhow::bail!("unknown format {other:?}: use svg | svg-outlined | pdf | pptx"),
        };

        let filename = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "export".to_string());
        let mut response = json!({
            "exportPath": dest,
            "filename": filename,
            "pageCount": page_count,
        });
        if let Some(objects) = fates {
            response["objects"] = objects;
        }
        Ok(response)
    })
    .await;

    match outcome {
        Ok(mut value) => {
            // Swap the private filesystem path for a download ticket — the
            // same post-blocking mint as render's /raw ticket.
            if let Some(path) = value
                .get("exportPath")
                .and_then(|p| p.as_str())
                .map(PathBuf::from)
            {
                let ticket = crate::lock(&state.tickets).create(path, fs::TICKET_TTL);
                if let Some(obj) = value.as_object_mut() {
                    obj.remove("exportPath");
                    obj.insert("ticket".into(), json!(ticket));
                }
            }
            Json(value).into_response()
        }
        Err(err) => board_error(&err),
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
    // A text edit carries no geometry, so the frame lookup gates only the
    // move/resize events, never the text-edited one.
    let saved = board
        .objects()
        .find(|(_, o)| o.id() == req.object)
        .and_then(|(_, o)| o.frame());
    let mut events = Vec::new();
    if let Some(saved) = saved {
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
    }
    if req.text.is_some() {
        events.push(Event::new(
            Actor::Human,
            EventKind::TextEdited {
                object: req.object.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The POST /board/journal body is validated purely by deserializing
    /// [`JournalAppendRequest`], so the §6.4 comment-pin vocabulary must
    /// parse exactly as the pane sends it — and an unknown op must not.
    #[test]
    fn journal_append_accepts_the_comment_pin_vocabulary() {
        let req: JournalAppendRequest = serde_json::from_str(
            r#"{"path":"/w/fig2.board.json","actor":"human","event":"comment","page":"bench","object":"callout","at":[320,96],"pin":"c1","text":"say the median"}"#,
        )
        .unwrap();
        assert_eq!(req.actor, chimaera_board::journal::Actor::Human);
        assert!(matches!(
            req.event,
            chimaera_board::journal::EventKind::Comment { ref pin, ref object, .. }
                if pin == "c1" && object.as_deref() == Some("callout")
        ));

        let req: JournalAppendRequest = serde_json::from_str(
            r#"{"path":"/w/fig2.board.json","actor":"human","event":"comment-resolved","pin":"c1"}"#,
        )
        .unwrap();
        assert!(matches!(
            req.event,
            chimaera_board::journal::EventKind::CommentResolved { ref pin } if pin == "c1"
        ));

        assert!(
            serde_json::from_str::<JournalAppendRequest>(
                r#"{"path":"/w/fig2.board.json","actor":"human","event":"from-the-future"}"#,
            )
            .is_err(),
            "an unknown op never reaches the journal file"
        );
    }
}
