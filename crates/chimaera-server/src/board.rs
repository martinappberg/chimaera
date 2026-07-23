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

/// Ceiling on `childFrames` entries per render response. A page holds a few
/// composites of a few dozen children each; the cap only exists so a
/// pathological board (hundreds of colorbars) cannot grow the response —
/// bounded wire beats complete hit-testing on a board nobody can read anyway.
const CHILD_FRAMES_CAP: usize = 512;

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
    /// The viewer's appearance, `"light"` or `"dark"` — what a board whose
    /// theme is `auto` (or absent) resolves to, so the pane and a shown card
    /// match the app around them. A pinned theme ignores it. Absent (an older
    /// client) keeps the pre-auto behavior: auto resolves dark.
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct DescribeRequest {
    pub path: String,
}

/// One semantic gesture from the pane: move/resize an object by id, replace
/// a text/shape object's text, or `set` sparse configuration fields.
///
/// The pane never serializes a board itself — a client-side
/// `JSON.stringify` would destroy the canonical byte-stable form and churn
/// every diff — so a gesture routes through here, where the crate's writer is
/// the one authority on bytes.
#[derive(Deserialize)]
pub(crate) struct EditRequest {
    pub path: String,
    /// The object's id — the same id that is the diff anchor, the journal
    /// subject, and the merge key. Optional (additively) because the one
    /// board-level gesture, `canvasBackground`, has no object; every
    /// object-scoped op still requires it.
    #[serde(default)]
    pub object: Option<String>,
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
    /// Sparse configuration updates: dot-paths over the object's canonical
    /// JSON → new values (`{"x.title": "Time (s)", "marks.0.fill": "@cat3"}`;
    /// null clears the field). Generic on purpose — a chart's axes, sort and
    /// mark config are exactly config, not a special object. The mutation is
    /// applied to the object's serialized form and the result must re-parse
    /// as a valid object, so this route can never write a board it could not
    /// read back. `id`/`type` are immutable and `at`/`size` are refused —
    /// move/resize own geometry, which keeps the undo stack's frame-based
    /// staleness rules honest. `BTreeMap` so application order is
    /// deterministic.
    #[serde(default)]
    pub set: Option<std::collections::BTreeMap<String, serde_json::Value>>,
    /// The one board-level field edit: set (a string — an `@token` or
    /// `#rrggbb`) or clear (JSON null) `canvas.background`, the ground painted
    /// under every page. Double-optional so "absent" and "null" stay distinct
    /// on the wire: absent = not this gesture, null = back to the theme's
    /// ground. Journaled as `canvas-changed`.
    #[serde(
        default,
        rename = "canvasBackground",
        deserialize_with = "double_option"
    )]
    pub canvas_background: Option<Option<String>>,
}

/// Deserialize a present-but-maybe-null field into `Some(inner)` — serde's
/// stock `Option` folds JSON null into "absent", which would make "clear the
/// field" indistinguishable from "don't touch it".
fn double_option<'de, D>(de: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<String>::deserialize(de)?))
}

/// A `set` refusal: the request was well-formed but the mutation is not
/// applicable — an immutable/geometry field, a path that doesn't traverse,
/// or a value whose result fails to parse back into a valid object. Mapped
/// to 422 (vs. the routes' generic 400) so a caller can tell "fix the value"
/// from "fix the request". Nothing is written on this path.
#[derive(Debug)]
struct SetRejected(String);

impl std::fmt::Display for SetRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SetRejected {}

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

        // `auto` (and an absent theme) follows the viewer's mode; a pinned
        // theme wins regardless. The cache key hashes the RESOLVED theme (its
        // whole serialized form), so an auto board's light and dark renders
        // are distinct entries by construction — "auto" itself never keys.
        let dark = req.mode.as_deref() != Some("light");
        let theme_name = req.theme.clone().or_else(|| board.theme.clone());
        let theme =
            chimaera_board::theme::resolve_for_mode(theme_name.as_deref(), dark, Some(&ws))?;
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

        let (width, height, child_frames) = match read_sidecar(&sidecar, &png_path) {
            Some((w, h, cached, children)) => {
                diagnostics.extend(cached);
                (w, h, children)
            }
            None => {
                // Only a miss pays for the font stack — building one walks
                // every system font directory, which is not hit-path work on
                // a shared login node.
                let fonts = FontStack::for_workspace(&ws);
                let out = render_page(&board, req.page, &theme, &fonts, params)?;
                // Child frames are part of the same pure function as the
                // pixels (board bytes × theme × page), so they persist in the
                // sidecar exactly like diagnostics: a cache hit must not make
                // composite children silently unselectable.
                let children = board
                    .pages
                    .get(req.page)
                    .map(|p| sidecar_child_frames(&board, p, &theme, &fonts))
                    .unwrap_or_default();
                chimaera_board::write_atomic(&png_path, &out.png)?;
                write_sidecar(&sidecar, out.width, out.height, &out.diagnostics, &children);
                chimaera_board::prune_renders(&dir, chimaera_board::RENDER_CACHE_CAP);
                diagnostics.extend(out.diagnostics);
                (out.width, out.height, children)
            }
        };

        Ok(json!({
            "pngPath": png_path,
            "width": width,
            "height": height,
            "pageCount": page_count,
            "pages": board.pages.iter().map(|p| p.id.clone()).collect::<Vec<_>>(),
            // The theme's categorical ramp as @token → resolved hex, for the
            // inspector's series-color swatches. Tokens only — the swatch row
            // commits the @-ref (the theme indirection), never a literal, and
            // the hex exists purely so the swatch can *show* what the token
            // resolves to under this board's theme. Additive; cheap enough to
            // ride the cached-render path (no sidecar participation).
            "catSwatches": theme
                .chart
                .categorical
                .iter()
                .filter(|t| t.starts_with('@'))
                .filter_map(|t| theme.color(t).map(|rgb| json!({"token": t, "hex": rgb.hex()})))
                .collect::<Vec<_>>(),
            // The theme's ground tones as @token → resolved hex, for the
            // pane's canvas-background swatch row. Same contract as
            // catSwatches: the control commits the token, the hex only shows
            // what it resolves to under the theme THIS render used. Additive.
            "bgSwatches": (["@bg", "@surface", "@edge", "@grid"]
                .iter()
                .filter_map(|t| theme.color(t).map(|rgb| json!({"token": t, "hex": rgb.hex()})))
                .collect::<Vec<_>>()),
            // Every composite's derived children (`<id>/<part>`) and where
            // the layout put them, in page points — the pane's hit-test map
            // for selecting/dragging/discussing a diagram node instead of
            // treating the composite as one opaque rectangle. Additive;
            // ordered within a composite by z (expansion order).
            "childFrames": child_frames,
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
        move || perform_edit(&path, &req)
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
        Err(err) if err.downcast_ref::<SetRejected>().is_some() => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": format!("{err:#}")})),
        )
            .into_response(),
        Err(err) => board_error(&err),
    }
}

/// The edit gesture's whole read→mutate→normalize→save→journal cycle, on the
/// blocking pool under the caller's shard guard. Split from the handler so
/// tests exercise the exact mutation path (atomicity, canonical bytes,
/// refusals) without standing up an `AppState`.
fn perform_edit(path: &Path, req: &EditRequest) -> anyhow::Result<serde_json::Value> {
    let mut board = chimaera_board::load(path)?;

    // The board-level gesture: set or clear `canvas.background`. Validated
    // by FORM here (an @token or a #hex literal — which token exists is the
    // render-time theme's business) so the write can never land a value
    // normalize would immediately drop; refusal is a 422, nothing written.
    if let Some(bg) = req.canvas_background.as_ref() {
        if let Some(value) = bg {
            let token = value
                .strip_prefix('@')
                .map(|t| !t.is_empty())
                .unwrap_or(false);
            if !token && chimaera_board::theme::parse_hex(value).is_none() {
                anyhow::bail!(SetRejected(format!(
                    "canvasBackground {value:?} is neither an @token nor a #rrggbb literal \
                     (nothing written)"
                )));
            }
        }
        board.canvas.background = bg.clone();
    }

    let mut prior = None;
    if let Some(object) = req.object.as_deref() {
        let mut found = false;
        for page in &mut board.pages {
            // Top-level objects only: group children are page-absolute too,
            // but moving one without its siblings is a different gesture
            // (enter-the-group), which the pane does not offer yet.
            for obj in &mut page.objects {
                if obj.id() != object {
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
                            "text applies to text and shape objects; {object:?} is a {}",
                            other.kind()
                        ),
                    }
                }
                if let Some(set) = req.set.as_ref().filter(|s| !s.is_empty()) {
                    apply_set(obj, object, set)?;
                }
            }
        }
        if !found {
            anyhow::bail!("no object {object:?} in {}", path.display());
        }
    } else if req.canvas_background.is_none() {
        anyhow::bail!("an edit names an object or a board-level field");
    }

    // Normalize (grid snap, group re-union) before the canonical save —
    // the same pipeline an agent edit goes through, so a human drag and
    // an agent Edit produce bytes of identical shape.
    chimaera_board::normalize(&mut board);
    chimaera_board::save(path, &board)?;

    let journal_seq = journal_edit(path, &board, req, prior);

    let meta = std::fs::metadata(path)?;
    let mut response = json!({
        "mtime": fs::mtime_token(&meta),
    });
    if let Some(seq) = journal_seq {
        response["journalSeq"] = json!(seq);
    }
    Ok(response)
}

/// Apply a `set` map to one object by editing its serialized form and
/// re-parsing the result — the whole-object round trip is the validity
/// gate: a value that turns a known type unparseable comes back as
/// [`chimaera_board::Object::Unknown`] with the parse reason, and the edit
/// is rejected before anything is written. Every refusal is a
/// [`SetRejected`] (422).
fn apply_set(
    obj: &mut chimaera_board::Object,
    id: &str,
    set: &std::collections::BTreeMap<String, serde_json::Value>,
) -> anyhow::Result<()> {
    let mut raw = serde_json::to_value(&*obj)?;
    for (fpath, value) in set {
        apply_field_path(&mut raw, fpath, value).map_err(|e| {
            anyhow::Error::new(SetRejected(format!("set {fpath:?} on {id:?}: {e}")))
        })?;
    }
    let new_obj: chimaera_board::Object = serde_json::from_value(raw)?;
    if let chimaera_board::Object::Unknown(u) = &new_obj {
        if let Some(reason) = &u.error {
            anyhow::bail!(SetRejected(format!(
                "set leaves {id:?} an invalid {} object (nothing written): {reason}",
                u.kind
            )));
        }
    }
    *obj = new_obj;
    Ok(())
}

/// One dot-path assignment into an object's serialized JSON. Numeric
/// segments index arrays (in bounds only — `set` edits config, it does not
/// grow collections); missing intermediate keys materialize as objects; a
/// null value removes the field (canonical serialization omits absent
/// options, so null-as-removal is what round-trips). `id`/`type` are
/// immutable and `at`/`size` belong to move/resize.
fn apply_field_path(
    root: &mut serde_json::Value,
    path: &str,
    value: &serde_json::Value,
) -> anyhow::Result<()> {
    let segments: Vec<&str> = path.split('.').collect();
    if path.is_empty() || segments.iter().any(|s| s.is_empty()) {
        anyhow::bail!("empty field path segment");
    }
    match segments[0] {
        "id" | "type" => anyhow::bail!("the field is immutable (it anchors identity)"),
        "at" | "size" => anyhow::bail!("geometry is owned by the move/resize ops"),
        _ => {}
    }
    let mut cur = root;
    for (i, seg) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        if let Ok(idx) = seg.parse::<usize>() {
            let arr = cur
                .as_array_mut()
                .ok_or_else(|| anyhow::anyhow!("segment {seg:?} indexes a non-array"))?;
            let len = arr.len();
            let slot = arr
                .get_mut(idx)
                .ok_or_else(|| anyhow::anyhow!("index {idx} out of bounds (len {len})"))?;
            if last {
                *slot = value.clone();
                return Ok(());
            }
            cur = slot;
        } else {
            let map = cur
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("segment {seg:?} crosses a non-object"))?;
            if last {
                if value.is_null() {
                    map.remove(*seg);
                } else {
                    map.insert((*seg).to_string(), value.clone());
                }
                return Ok(());
            }
            cur = map
                .entry((*seg).to_string())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        }
    }
    unreachable!("the last segment returns");
}

/// Read a dot-path back out of a serialized object; `Null` when absent —
/// which is exactly how a cleared field journals.
fn field_path_value(root: &serde_json::Value, path: &str) -> serde_json::Value {
    let mut cur = root;
    for seg in path.split('.') {
        let next = match seg.parse::<usize>() {
            Ok(idx) => cur.get(idx),
            Err(_) => cur.get(seg),
        };
        match next {
            Some(v) => cur = v,
            None => return serde_json::Value::Null,
        }
    }
    cur.clone()
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
    /// The CLI's `--charts native` as a wire flag: opt into real `c:chart`
    /// parts for charts that map cleanly (the rest degrade per-chart with the
    /// reason in their fate). pptx only — refused, like the CLI, on any other
    /// format so a stray flag never silently no-ops. Defaults to grouped
    /// (additive: an older client's request behaves exactly as before).
    #[serde(default, rename = "chartsNative")]
    pub charts_native: bool,
}

/// An export-request refusal: well-formed JSON whose parameter combination is
/// not applicable (`chartsNative` off pptx). Mapped to 422 (vs. the routes'
/// generic 400), mirroring [`SetRejected`]: "fix the parameters", not "fix
/// the request". Nothing is exported on this path.
#[derive(Debug)]
struct ExportRefused(String);

impl std::fmt::Display for ExportRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ExportRefused {}

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
    let outcome = fs::blocking_value(move || perform_export(&req)).await;

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
        Err(err) if err.downcast_ref::<ExportRefused>().is_some() => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": format!("{err:#}")})),
        )
            .into_response(),
        Err(err) => board_error(&err),
    }
}

/// The whole export cycle (resolve → load → normalize → write into
/// `.chimaera/board/exports/`), on the blocking pool. Split from the handler
/// so tests exercise the exact export path — format dispatch, the
/// `chartsNative` mapping, refusals — without standing up an `AppState`.
fn perform_export(req: &ExportRequest) -> anyhow::Result<serde_json::Value> {
    // Validated before any filesystem work, exactly like the CLI's up-front
    // `--charts` check ("--charts applies to pptx only").
    if req.charts_native && req.format != "pptx" {
        anyhow::bail!(ExportRefused("chartsNative applies to pptx only".into()));
    }
    let path = resolve_board_path(&req.path)?;
    let ws = chimaera_board::workspace_root(&path);
    let mut board = chimaera_board::load(&path)?;
    chimaera_board::normalize(&mut board);
    // Exports resolve `auto` (and an absent theme) dark — the pre-auto
    // default; the artifact is leaving the app, so there is no viewer mode
    // for it to follow.
    let theme = chimaera_board::theme::resolve_for_mode(board.theme.as_deref(), true, Some(&ws))?;
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
                anyhow::bail!("page does not apply to pdf: the whole deck exports as one document");
            }
            let pdf = chimaera_board::export::pdf::export_pdf(&board, &theme, &fonts, Some(&ws))?;
            let dest = exports_dir.join(format!("{stem}.pdf"));
            chimaera_board::write_atomic(&dest, &pdf)?;
            dest
        }
        "pptx" => {
            if req.page.is_some() {
                anyhow::bail!("page does not apply to pptx: the whole deck exports as one file");
            }
            let mut bytes = Vec::new();
            // The same PptxOptions construction as the CLI's --charts:
            // default-then-set, so new knobs never break this route.
            let mut opts = chimaera_board::export::PptxOptions::default();
            if req.charts_native {
                opts.chart_fidelity = chimaera_board::export::ChartFidelity::Native;
            }
            let report = chimaera_board::export::write_pptx_with(
                &board,
                &theme,
                &fonts,
                Some(&ws),
                &opts,
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

    let mut events = Vec::new();
    // The board-level gesture journals the SAVED value (null = cleared),
    // like every object event: the journal narrates the file, not the wire.
    if req.canvas_background.is_some() {
        let changed = [(
            "canvas.background".to_string(),
            board
                .canvas
                .background
                .as_deref()
                .map(serde_json::Value::from)
                .unwrap_or(serde_json::Value::Null),
        )]
        .into_iter()
        .collect();
        events.push(Event::new(
            Actor::Human,
            EventKind::CanvasChanged { changed },
        ));
    }

    if let Some(object) = req.object.as_deref() {
        // The journaled `to` is the *saved* geometry (post-normalize grid
        // snap), not the requested one — the journal narrates the file, never
        // the wire. A text edit carries no geometry, so the frame lookup gates
        // only the move/resize events, never the text-edited one.
        let saved_obj = board
            .objects()
            .find(|(_, o)| o.id() == object)
            .map(|(_, o)| o);
        let saved = saved_obj.and_then(|o| o.frame());
        if let Some(saved) = saved {
            if req.at.is_some() {
                events.push(Event::new(
                    Actor::Human,
                    EventKind::Move {
                        object: object.to_string(),
                        from: prior.map(|f| [f.x, f.y]).unwrap_or([saved.x, saved.y]),
                        to: [saved.x, saved.y],
                    },
                ));
            }
            if req.size.is_some() {
                events.push(Event::new(
                    Actor::Human,
                    EventKind::Resize {
                        object: object.to_string(),
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
                    object: object.to_string(),
                },
            ));
        }
        if let Some(set) = req.set.as_ref().filter(|s| !s.is_empty()) {
            // The journaled values are the SAVED object's, read back per path
            // (post-normalize) — the journal narrates the file, never the
            // wire. Null means the field was cleared.
            if let Some(raw) = saved_obj.and_then(|o| serde_json::to_value(o).ok()) {
                let changed = set
                    .keys()
                    .map(|p| (p.clone(), field_path_value(&raw, p)))
                    .collect();
                events.push(Event::new(
                    Actor::Human,
                    EventKind::Restyle {
                        object: object.to_string(),
                        changed,
                    },
                ));
            }
        }
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

/// The persisted half of a render's output: dimensions, diagnostics, and the
/// composites' child frames, written beside the PNG under the same
/// content-addressed key.
#[derive(serde::Serialize, Deserialize)]
struct RenderSidecar {
    width: u32,
    height: u32,
    diagnostics: Vec<SidecarDiag>,
    /// Composite id → its derived children's laid-out frames. `None` marks a
    /// sidecar from before the field existed — treated as a cache miss, so an
    /// upgrade re-renders once rather than serving a board whose composite
    /// children silently stopped hit-testing.
    #[serde(default)]
    child_frames: Option<std::collections::BTreeMap<String, Vec<SidecarChild>>>,
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

/// One derived child on the wire: its stable derived id (`<composite>/<part>`)
/// and `[x, y, w, h]` in page points — the same shape the pane's own frame
/// math speaks.
#[derive(serde::Serialize, Deserialize)]
struct SidecarChild {
    id: String,
    frame: [f64; 4],
}

/// The page's composite child frames in wire/sidecar form, capped at
/// [`CHILD_FRAMES_CAP`] total entries (truncation is deterministic: id order,
/// z-order within a composite — the same order the map itself carries).
fn sidecar_child_frames(
    board: &chimaera_board::Board,
    page: &chimaera_board::Page,
    theme: &Theme,
    fonts: &FontStack,
) -> std::collections::BTreeMap<String, Vec<SidecarChild>> {
    let mut budget = CHILD_FRAMES_CAP;
    let mut out = std::collections::BTreeMap::new();
    for (parent, children) in
        chimaera_board::composites::page_child_frames(board, page, theme, fonts)
    {
        if budget == 0 {
            break;
        }
        let take: Vec<SidecarChild> = children
            .into_iter()
            .take(budget)
            .map(|(id, f)| SidecarChild {
                id,
                frame: [f.x, f.y, f.w, f.h],
            })
            .collect();
        budget -= take.len();
        out.insert(parent, take);
    }
    out
}

/// What a cache hit yields: dimensions, diagnostics, and the child frames.
type SidecarHit = (
    u32,
    u32,
    Vec<chimaera_board::Diagnostic>,
    std::collections::BTreeMap<String, Vec<SidecarChild>>,
);

/// A cache hit needs both halves intact; a missing or unreadable sidecar (or
/// PNG, or a pre-`child_frames` sidecar) degrades to a re-render, never to
/// serving broken state.
fn read_sidecar(sidecar: &std::path::Path, png_path: &std::path::Path) -> Option<SidecarHit> {
    if !png_path.exists() {
        return None;
    }
    let raw = std::fs::read_to_string(sidecar).ok()?;
    let parsed: RenderSidecar = serde_json::from_str(&raw).ok()?;
    let children = parsed.child_frames?;
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
    Some((parsed.width, parsed.height, diags, children))
}

/// Best-effort: a failed sidecar write costs a re-render on the next hit,
/// nothing more.
fn write_sidecar(
    sidecar: &std::path::Path,
    width: u32,
    height: u32,
    diagnostics: &[chimaera_board::Diagnostic],
    child_frames: &std::collections::BTreeMap<String, Vec<SidecarChild>>,
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
        child_frames: Some(
            child_frames
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.iter()
                            .map(|c| SidecarChild {
                                id: c.id.clone(),
                                frame: c.frame,
                            })
                            .collect(),
                    )
                })
                .collect(),
        ),
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

    /// A fresh workspace with one bar-chart board on disk. The `.git` marker
    /// pins `workspace_root` inside the temp dir, so the journal lands where
    /// the assertions look.
    fn chart_board(label: &str) -> (PathBuf, PathBuf) {
        let ws =
            std::env::temp_dir().join(format!("chimaera-board-set-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        let path = ws.join("fig.board.json");
        let board: chimaera_board::Board = serde_json::from_str(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[960,540]},
                "pages":[{"id":"p1","objects":[
                  {"id":"bench","type":"chart","at":[80,80],"size":[400,300],
                   "data":{"origin":"stated-by-user",
                           "values":[{"tool":"a","ms":4},{"tool":"b","ms":2}]},
                   "x":{"field":"tool","type":"nominal"},
                   "y":{"field":"ms","type":"quantitative"},
                   "marks":[{"mark":"bar"}]}]}]}"#,
        )
        .unwrap();
        chimaera_board::save(&path, &board).unwrap();
        (ws, path)
    }

    fn edit_req(path: &Path, object: &str, set: serde_json::Value) -> EditRequest {
        EditRequest {
            path: path.to_string_lossy().into_owned(),
            object: Some(object.to_string()),
            at: None,
            size: None,
            text: None,
            set: Some(serde_json::from_value(set).unwrap()),
            canvas_background: None,
        }
    }

    /// The board-level gesture: set/clear `canvas.background` with no object,
    /// wire-distinguishing null (clear) from absent (not this gesture), the
    /// saved file byte-canonical, the journal carrying `canvas-changed` with
    /// the SAVED value, and a malformed reference refused with nothing
    /// written.
    #[test]
    fn canvas_background_edits_set_clear_and_refuse() {
        let (ws, path) = chart_board("canvas-bg");

        // Wire shapes: absent vs null vs value.
        let parsed: EditRequest =
            serde_json::from_str(r#"{"path":"/w/f.board.json","object":"bench"}"#).unwrap();
        assert!(
            parsed.canvas_background.is_none(),
            "absent = not this gesture"
        );
        let parsed: EditRequest =
            serde_json::from_str(r#"{"path":"/w/f.board.json","canvasBackground":null}"#).unwrap();
        assert_eq!(parsed.canvas_background, Some(None), "null = clear");
        let parsed: EditRequest =
            serde_json::from_str(r#"{"path":"/w/f.board.json","canvasBackground":"@surface"}"#)
                .unwrap();
        assert_eq!(parsed.canvas_background, Some(Some("@surface".to_string())));

        let canvas_req = |bg: Option<&str>| EditRequest {
            path: path.to_string_lossy().into_owned(),
            object: None,
            at: None,
            size: None,
            text: None,
            set: None,
            canvas_background: Some(bg.map(String::from)),
        };

        // Set: the file gets the field, byte-canonical, journaled.
        let out = perform_edit(&path, &canvas_req(Some("@surface"))).unwrap();
        let seq = out.get("journalSeq").and_then(|s| s.as_u64()).unwrap();
        let board = chimaera_board::load(&path).unwrap();
        assert_eq!(board.canvas.background.as_deref(), Some("@surface"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            chimaera_board::to_string(&board).unwrap()
        );
        let journal_path = chimaera_board::journal::journal_path(&ws, &path);
        let events = chimaera_board::journal::read_since(&journal_path, 0).unwrap();
        let ev = events.iter().find(|e| e.seq == seq).unwrap();
        match &ev.kind {
            chimaera_board::journal::EventKind::CanvasChanged { changed } => {
                assert_eq!(changed.get("canvas.background").unwrap(), "@surface");
            }
            other => panic!("expected canvas-changed, got {other:?}"),
        }

        // Clear: null drops the field back to the theme's ground.
        perform_edit(&path, &canvas_req(None)).unwrap();
        let board = chimaera_board::load(&path).unwrap();
        assert_eq!(board.canvas.background, None);

        // A malformed reference is a 422 with nothing written.
        let before = std::fs::read_to_string(&path).unwrap();
        let err = perform_edit(&path, &canvas_req(Some("cornflower"))).unwrap_err();
        assert!(err.downcast_ref::<SetRejected>().is_some(), "{err:#}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        // No object and no board-level field is a request error, not a write.
        let mut empty = canvas_req(None);
        empty.canvas_background = None;
        assert!(perform_edit(&path, &empty).is_err());

        let _ = std::fs::remove_dir_all(&ws);
    }

    /// The happy path: a chart's sort and axis label land as sparse field
    /// edits, the saved file is byte-canonical, and the journal carries one
    /// human `restyle` naming exactly the changed fields with saved values.
    #[test]
    fn set_edits_chart_sort_and_axis_label_canonically() {
        let (ws, path) = chart_board("happy");
        let req = edit_req(
            &path,
            "bench",
            serde_json::json!({"x.sort": "-y", "y.title": "Time (ms)"}),
        );
        let out = perform_edit(&path, &req).unwrap();
        assert!(out.get("mtime").is_some());
        let seq = out.get("journalSeq").and_then(|s| s.as_u64()).unwrap();

        let board = chimaera_board::load(&path).unwrap();
        let (_, obj) = board.objects().find(|(_, o)| o.id() == "bench").unwrap();
        let chimaera_board::Object::Chart(chart) = obj else {
            panic!("bench stayed a chart");
        };
        assert_eq!(chart.x.as_ref().unwrap().sort.as_deref(), Some("-y"));
        assert_eq!(
            chart.y.as_ref().unwrap().title.as_deref(),
            Some("Time (ms)")
        );
        // Byte-canonical: the file is exactly the crate writer's output.
        let bytes = std::fs::read_to_string(&path).unwrap();
        assert_eq!(bytes, chimaera_board::to_string(&board).unwrap());

        let journal_path = chimaera_board::journal::journal_path(&ws, &path);
        let events = chimaera_board::journal::read_since(&journal_path, 0).unwrap();
        let restyle = events.iter().find(|e| e.seq == seq).unwrap();
        assert_eq!(restyle.actor, chimaera_board::journal::Actor::Human);
        match &restyle.kind {
            chimaera_board::journal::EventKind::Restyle { object, changed } => {
                assert_eq!(object, "bench");
                assert_eq!(changed.get("x.sort").unwrap(), "-y");
                assert_eq!(changed.get("y.title").unwrap(), "Time (ms)");
                assert_eq!(changed.len(), 2, "exactly the set fields are named");
            }
            other => panic!("expected restyle, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// A value that turns the object unparseable is rejected with the parse
    /// reason and NOTHING is written — neither the board nor the journal.
    #[test]
    fn invalid_set_value_is_rejected_atomically() {
        let (ws, path) = chart_board("invalid");
        let before = std::fs::read_to_string(&path).unwrap();
        let req = edit_req(&path, "bench", serde_json::json!({"x.type": "bogus"}));
        let err = perform_edit(&path, &req).unwrap_err();
        assert!(
            err.downcast_ref::<SetRejected>().is_some(),
            "422 class: {err:#}"
        );
        assert!(format!("{err:#}").contains("nothing written"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "the file is untouched"
        );
        let journal_path = chimaera_board::journal::journal_path(&ws, &path);
        assert!(
            !journal_path.exists(),
            "no journal event for a refused edit"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// `id`/`type` are immutable and geometry belongs to move/resize — each
    /// refusal is a [`SetRejected`], file untouched.
    #[test]
    fn immutable_and_geometry_fields_are_refused() {
        let (ws, path) = chart_board("immutable");
        let before = std::fs::read_to_string(&path).unwrap();
        for set in [
            serde_json::json!({"id": "other"}),
            serde_json::json!({"type": "text"}),
            serde_json::json!({"at": [0.0, 0.0]}),
            serde_json::json!({"size.0": 100.0}),
        ] {
            let err = perform_edit(&path, &edit_req(&path, "bench", set)).unwrap_err();
            assert!(err.downcast_ref::<SetRejected>().is_some(), "{err:#}");
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// Two concurrent set edits on one board serialize on the shard lock the
    /// handler takes — both land, neither read-modify-write is lost.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_set_edits_serialize_on_the_shard_lock() {
        let (ws, path) = chart_board("concurrent");
        let mut tasks = Vec::new();
        for set in [
            serde_json::json!({"x.sort": "-y"}),
            serde_json::json!({"y.title": "Time (ms)"}),
        ] {
            let path = path.clone();
            tasks.push(tokio::spawn(async move {
                // The handler's exact discipline: shard guard held across the
                // whole blocking read→mutate→write.
                let _guard = edit_shard(&path).lock().await;
                let req = edit_req(&path, "bench", set);
                tokio::task::spawn_blocking(move || perform_edit(&path, &req))
                    .await
                    .unwrap()
                    .unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        let board = chimaera_board::load(&path).unwrap();
        let (_, obj) = board.objects().find(|(_, o)| o.id() == "bench").unwrap();
        let chimaera_board::Object::Chart(chart) = obj else {
            panic!("bench stayed a chart");
        };
        assert_eq!(chart.x.as_ref().unwrap().sort.as_deref(), Some("-y"));
        assert_eq!(
            chart.y.as_ref().unwrap().title.as_deref(),
            Some("Time (ms)")
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// `chartsNative` rides the wire camelCase, defaults to grouped, and maps
    /// to [`chimaera_board::export::ChartFidelity::Native`] through
    /// `write_pptx_with` — observable as the chart's fate flipping from
    /// `grouped` to `native` on the same board.
    #[test]
    fn export_charts_native_flips_the_chart_fate() {
        let (ws, path) = chart_board("export-native");
        let body = |native: bool| {
            format!(
                r#"{{"path":{:?},"format":"pptx","chartsNative":{native}}}"#,
                path.to_string_lossy()
            )
        };

        // The default (absent flag) stays the pre-flag behavior: grouped.
        let req: ExportRequest = serde_json::from_str(&format!(
            r#"{{"path":{:?},"format":"pptx"}}"#,
            path.display()
        ))
        .unwrap();
        assert!(!req.charts_native, "absent chartsNative means grouped");
        let out = perform_export(&req).unwrap();
        assert_eq!(out["filename"], "fig.pptx");
        assert_eq!(out["pageCount"], 1);
        let fate = |v: &serde_json::Value| {
            v["objects"]
                .as_array()
                .unwrap()
                .iter()
                .find(|o| o["id"] == "bench")
                .map(|o| o["tier"].as_str().unwrap().to_string())
                .unwrap()
        };
        assert_eq!(fate(&out), "grouped");

        // Opt-in: the cleanly-mappable bar chart becomes a real c:chart part.
        let req: ExportRequest = serde_json::from_str(&body(true)).unwrap();
        assert!(req.charts_native);
        let out = perform_export(&req).unwrap();
        assert_eq!(fate(&out), "native");
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// `chartsNative` off pptx is refused with the CLI's up-front semantics
    /// ("--charts applies to pptx only"): a 422-class [`ExportRefused`],
    /// checked before any filesystem work — the path is deliberately bogus.
    #[test]
    fn charts_native_off_pptx_is_refused() {
        for format in ["svg", "svg-outlined", "pdf"] {
            let req: ExportRequest = serde_json::from_str(&format!(
                r#"{{"path":"/nowhere/x.board.json","format":{format:?},"chartsNative":true}}"#
            ))
            .unwrap();
            let err = perform_export(&req).unwrap_err();
            assert!(
                err.downcast_ref::<ExportRefused>().is_some(),
                "422 class for {format}: {err:#}"
            );
            assert!(format!("{err:#}").contains("applies to pptx only"));
        }
    }

    /// A fresh workspace with a two-node diagram board on disk — the child
    /// gestures' fixture (drag a node → `nodes.<i>.at`, rename it →
    /// `nodes.<i>.label`).
    fn diagram_board(label: &str) -> (PathBuf, PathBuf) {
        let ws = std::env::temp_dir().join(format!(
            "chimaera-board-diag-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        let path = ws.join("flow.board.json");
        let board: chimaera_board::Board = serde_json::from_str(
            r#"{"format":"chimaera.board","formatVersion":1,
                "canvas":{"size":[960,540]},
                "pages":[{"id":"p1","objects":[
                  {"id":"flow","type":"diagram","at":[48,48],"size":[500,400],
                   "nodes":[{"id":"start","label":"Start"},{"id":"end","label":"End"}],
                   "edges":[{"from":"start","to":"end"}]}]}]}"#,
        )
        .unwrap();
        chimaera_board::save(&path, &board).unwrap();
        (ws, path)
    }

    /// The pane's child gestures land as sparse `set` edits on the parent
    /// diagram's node entry: a drag pins `nodes.<i>.at`, the overlay editor
    /// rewrites `nodes.<i>.label` — geometry refusal guards only the OBJECT's
    /// own at/size, and the journal's restyle names the node paths.
    #[test]
    fn set_pins_a_diagram_node_and_edits_its_label() {
        let (ws, path) = diagram_board("pin");
        let req = edit_req(
            &path,
            "flow",
            serde_json::json!({"nodes.0.at": [64.0, 80.0], "nodes.1.label": "Done"}),
        );
        let out = perform_edit(&path, &req).unwrap();
        let seq = out.get("journalSeq").and_then(|s| s.as_u64()).unwrap();

        let board = chimaera_board::load(&path).unwrap();
        let (_, obj) = board.objects().find(|(_, o)| o.id() == "flow").unwrap();
        let chimaera_board::Object::Diagram(d) = obj else {
            panic!("flow stayed a diagram");
        };
        assert_eq!(d.nodes[0].at, Some([64.0, 80.0]));
        assert_eq!(d.nodes[1].label, "Done");
        // Byte-canonical: the file is exactly the crate writer's output.
        let bytes = std::fs::read_to_string(&path).unwrap();
        assert_eq!(bytes, chimaera_board::to_string(&board).unwrap());

        let journal_path = chimaera_board::journal::journal_path(&ws, &path);
        let events = chimaera_board::journal::read_since(&journal_path, 0).unwrap();
        let restyle = events.iter().find(|e| e.seq == seq).unwrap();
        match &restyle.kind {
            chimaera_board::journal::EventKind::Restyle { object, changed } => {
                assert_eq!(object, "flow");
                assert_eq!(
                    changed.get("nodes.0.at").unwrap(),
                    &serde_json::json!([64.0, 80.0])
                );
                assert_eq!(changed.get("nodes.1.label").unwrap(), "Done");
            }
            other => panic!("expected restyle, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// An invalid pin (wrong arity) turns the diagram unparseable, so the
    /// whole-object re-parse gate refuses it atomically — nothing written.
    #[test]
    fn an_invalid_node_pin_is_rejected_atomically() {
        let (ws, path) = diagram_board("badpin");
        let before = std::fs::read_to_string(&path).unwrap();
        for set in [
            serde_json::json!({"nodes.0.at": [1.0, 2.0, 3.0]}),
            serde_json::json!({"nodes.0.label": 42}),
            serde_json::json!({"nodes.9.at": [0.0, 0.0]}),
        ] {
            let err = perform_edit(&path, &edit_req(&path, "flow", set)).unwrap_err();
            assert!(err.downcast_ref::<SetRejected>().is_some(), "{err:#}");
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "the file is untouched"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }
}
