//! Data surfaces: data a plugin publishes in a versioned shape that core
//! draws with views it owns (problems in the editor, a build's result, jumps
//! between a source and its PDF). Design: docs/design/plugin-platform-plan.md §4.
//!
//! - `publish(surface, key, data)` is checked here against the surface's
//!   shape and cap; the latest per (plugin, workspace, surface, key) is
//!   kept, and a small `{"type":"surface", …}` frame tells that
//!   workspace's windows to fetch it again.
//! - `diagnostics/1` and `output/1` are small and live in memory;
//!   `sourcemap/1`, `knowledge/1` and `references/1` (up to 4 MiB) are
//!   written to the plugin's output folder (`.surfaces/`) and read back per
//!   request, so megabytes never sit in the daemon.
//! - `references/1` is ids a plugin answers for (`\label` keys, issue
//!   numbers): the client's reference registry turns them into chips in
//!   chats, previews and the Timeline.
//! - A plugin keeps at most `KEYS_MAX` keys per surface and workspace.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path as AxPath, Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::AppState;

pub(crate) const SURFACES: &[&str] = &[
    "diagnostics/1",
    "output/1",
    "sourcemap/1",
    "knowledge/1",
    "references/1",
];
const KEYS_MAX: usize = 256;
const KEY_MAX: usize = 512;
/// Problems per file, and per published key.
const DIAGNOSTICS_PER_FILE: usize = 200;
const DIAGNOSTICS_MAX: usize = 2000;
const TEXT_MAX: usize = 2048;
/// A source map or a Knowledge snapshot.
const LARGE_MAX: usize = 4 << 20;
/// A small surface's JSON.
const SMALL_MAX: usize = 512 << 10;
/// What one (plugin, workspace) may hold in memory across its small
/// surfaces: the daemon's RSS target is ~150 MB for everything.
const INLINE_BUDGET: usize = 2 << 20;
/// What every plugin in every workspace holds in memory together (JSON
/// bytes; parsed, several times that).
const INLINE_TOTAL: usize = 8 << 20;
const PAGES_MAX: usize = 2000;
/// references/1: id shapes per key, ids per key, and a shape's regex
/// source (the client's registry refuses longer ones, and any group).
const SHAPES_MAX: usize = 16;
const REF_IDS_MAX: usize = 5000;
const PATTERN_MAX: usize = 80;
/// Unbounded repeats one alternative of a shape may have: each more one
/// next to another multiplies the backtracking a scan of a long word does
/// (the UI's `usable` in `shared/references.ts` holds the same line).
const UNBOUNDED_MAX: usize = 2;

/// The most unbounded repeats (`*`, `+`, `{n,}`) in any one `|`
/// alternative of a regex source without groups.
fn unbounded_per_alternative(pattern: &str) -> usize {
    let bytes = pattern.as_bytes();
    let (mut most, mut here, mut i, mut in_class) = (0, 0, 0, false);
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b'[' if !in_class => in_class = true,
            b']' if in_class => in_class = false,
            _ if in_class => {}
            b'*' | b'+' => here += 1,
            b'{' => {
                if let Some(end) = pattern[i..].find('}') {
                    if pattern[i + 1..i + end].ends_with(',') {
                        here += 1;
                    }
                    i += end + 1;
                    continue;
                }
            }
            b'|' => {
                most = most.max(here);
                here = 0;
            }
            _ => {}
        }
        i += 1;
    }
    most.max(here)
}

/// What one surface key holds: the data (small surfaces), or where it was
/// written (large ones).
#[derive(Clone)]
enum Held {
    /// The data and its JSON's size (counted against `INLINE_BUDGET`).
    Inline(Arc<Value>, usize),
    Stored(PathBuf),
}

/// (plugin, workspace) → (surface, key) → what it holds.
#[derive(Default)]
pub(crate) struct Surfaces {
    by_pair: HashMap<(String, String), BTreeMap<(String, String), Held>>,
}

impl Surfaces {
    pub(crate) fn forget_plugin(&mut self, plugin: &str) {
        self.by_pair.retain(|(p, _), _| p != plugin);
    }

    /// Switched off in `ws`: what it published there goes (it publishes
    /// again when it is on and runs).
    pub(crate) fn forget_pair(&mut self, plugin: &str, ws: &str) {
        self.by_pair.remove(&(plugin.to_string(), ws.to_string()));
    }

    /// The JSON bytes every small surface holds in memory, but `except`.
    fn inline_total(&self, except: (&(String, String), &(String, String))) -> usize {
        self.by_pair
            .iter()
            .flat_map(|(pair, m)| m.iter().map(move |(k, h)| (pair, k, h)))
            .filter(|(pair, k, _)| (*pair, *k) != except)
            .map(|(_, _, h)| match h {
                Held::Inline(_, n) => *n,
                Held::Stored(_) => 0,
            })
            .sum()
    }

    pub(crate) fn forget_workspace(&mut self, ws: &str) {
        self.by_pair.retain(|(_, w), _| w != ws);
    }
}

fn large(surface: &str) -> bool {
    matches!(surface, "sourcemap/1" | "knowledge/1" | "references/1")
}

/// Text of 1..=`max` bytes.
fn short_text(item: &Value, key: &str, path: &str, max: usize) -> Result<(), String> {
    let s = str_field(item, key, path)?;
    if s.is_empty() || s.len() > max {
        return Err(format!("{path}.{key} must be 1–{max} bytes"));
    }
    Ok(())
}

fn str_field<'a>(item: &'a Value, key: &str, path: &str) -> Result<&'a str, String> {
    item.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{path}.{key} must be text"))
}

fn opt_u64(item: &Value, key: &str, path: &str) -> Result<(), String> {
    match item.get(key) {
        None | Some(Value::Null) => Ok(()),
        Some(v) if v.as_u64().is_some() => Ok(()),
        Some(_) => Err(format!("{path}.{key} must be a whole number")),
    }
}

fn opt_text(item: &Value, key: &str, path: &str, max: usize) -> Result<(), String> {
    match item.get(key) {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(s)) if s.len() <= max => Ok(()),
        Some(_) => Err(format!("{path}.{key} must be text of at most {max} bytes")),
    }
}

/// A workspace-relative path, or `output:<path>`.
fn place(item: &Value, key: &str, path: &str) -> Result<(), String> {
    let p = str_field(item, key, path)?;
    let inner = p.strip_prefix("output:").unwrap_or(p);
    let plain = !inner.is_empty()
        && std::path::Path::new(inner)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
    if !plain || p.len() > 1024 {
        return Err(format!(
            "{path}.{key} must be a workspace path or output:<path>"
        ));
    }
    Ok(())
}

/// `data` fits `surface`'s shape (the documented `…/1` schemas).
pub(crate) fn check(surface: &str, data: &Value) -> Result<(), String> {
    match surface {
        "diagnostics/1" => {
            let items = data
                .get("items")
                .and_then(Value::as_array)
                .ok_or("diagnostics/1: `items` must be a list")?;
            if items.len() > DIAGNOSTICS_MAX {
                return Err(format!(
                    "diagnostics/1: at most {DIAGNOSTICS_MAX} items per key"
                ));
            }
            let mut per_file: HashMap<&str, usize> = HashMap::new();
            for (i, item) in items.iter().enumerate() {
                let at = format!("items[{i}]");
                place(item, "file", &at)?;
                let file = item["file"].as_str().unwrap_or_default();
                let n = per_file.entry(file).or_default();
                *n += 1;
                if *n > DIAGNOSTICS_PER_FILE {
                    return Err(format!(
                        "diagnostics/1: at most {DIAGNOSTICS_PER_FILE} items for {file}"
                    ));
                }
                match item.get("severity").and_then(Value::as_str) {
                    Some("error" | "warning" | "info" | "hint") => {}
                    _ => return Err(format!("{at}.severity is error, warning, info or hint")),
                }
                if item
                    .get("line")
                    .and_then(Value::as_u64)
                    .is_none_or(|l| l == 0)
                {
                    return Err(format!("{at}.line must be a line number (from 1)"));
                }
                for key in ["column", "end_line", "end_column"] {
                    opt_u64(item, key, &at)?;
                }
                let message = str_field(item, "message", &at)?;
                if message.is_empty() || message.len() > TEXT_MAX {
                    return Err(format!("{at}.message must be 1–{TEXT_MAX} bytes"));
                }
                opt_text(item, "context", &at, TEXT_MAX)?;
                opt_text(item, "source", &at, 64)?;
            }
            Ok(())
        }
        "output/1" => {
            place(data, "source", "output/1")?;
            place(data, "output", "output/1")?;
            match data.get("state").and_then(Value::as_str) {
                Some("building" | "ok" | "errors" | "failed") => {}
                _ => return Err("output/1.state is building, ok, errors or failed".into()),
            }
            opt_text(data, "label", "output/1", 200)?;
            opt_u64(data, "finished_ms", "output/1")?;
            if let Some(log) = data.get("log").filter(|l| !l.is_null()) {
                let _ = log;
                place(data, "log", "output/1")?;
            }
            match data.get("changed_pages") {
                None | Some(Value::Null) => {}
                Some(Value::Array(pages))
                    if pages.len() <= PAGES_MAX && pages.iter().all(|p| p.as_u64().is_some()) => {}
                Some(_) => {
                    return Err(format!(
                        "output/1.changed_pages is a list of at most {PAGES_MAX} page numbers"
                    ))
                }
            }
            Ok(())
        }
        "sourcemap/1" => {
            place(data, "output", "sourcemap/1")?;
            let files = data
                .get("files")
                .and_then(Value::as_array)
                .ok_or("sourcemap/1.files must be a list of paths")?;
            if files.iter().any(|f| !f.is_string()) {
                return Err("sourcemap/1.files must be a list of paths".into());
            }
            // Each record: [file index, line, page, x, y, width, height].
            let records = data
                .get("records")
                .and_then(Value::as_array)
                .ok_or("sourcemap/1.records must be a list")?;
            for (i, r) in records.iter().enumerate() {
                let ok = r
                    .as_array()
                    .is_some_and(|a| a.len() == 7 && a.iter().all(Value::is_number));
                if !ok {
                    return Err(format!(
                        "sourcemap/1.records[{i}] is [file, line, page, x, y, width, height]"
                    ));
                }
            }
            Ok(())
        }
        "knowledge/1" => {
            if !data.is_object() {
                return Err("knowledge/1 is an object (the Knowledge snapshot)".into());
            }
            Ok(())
        }
        "references/1" => {
            let shapes = data
                .get("shapes")
                .and_then(Value::as_array)
                .ok_or("references/1.shapes must be a list")?;
            if shapes.is_empty() || shapes.len() > SHAPES_MAX {
                return Err(format!("references/1: 1–{SHAPES_MAX} shapes per key"));
            }
            for (i, shape) in shapes.iter().enumerate() {
                let at = format!("shapes[{i}]");
                short_text(shape, "kind", &at, 64)?;
                // A plain regex source: the client joins every source's
                // shapes into one scan over chat text, so no groups (and so
                // no nested quantifiers) and no anchors.
                let pattern = str_field(shape, "pattern", &at)?;
                if pattern.is_empty()
                    || pattern.len() > PATTERN_MAX
                    || pattern.contains(['(', ')', '^', '$'])
                {
                    return Err(format!(
                        "{at}.pattern is a regex source of 1–{PATTERN_MAX} bytes \
                         without groups or anchors"
                    ));
                }
                if unbounded_per_alternative(pattern) > UNBOUNDED_MAX {
                    return Err(format!(
                        "{at}.pattern has more than {UNBOUNDED_MAX} unbounded repeats \
                         (`*`, `+`, `{{n,}}`) in one alternative"
                    ));
                }
            }
            let ids = data
                .get("ids")
                .and_then(Value::as_array)
                .ok_or("references/1.ids must be a list")?;
            if ids.len() > REF_IDS_MAX {
                return Err(format!("references/1: at most {REF_IDS_MAX} ids per key"));
            }
            for (i, item) in ids.iter().enumerate() {
                let at = format!("ids[{i}]");
                short_text(item, "id", &at, 128)?;
                short_text(item, "key", &at, 256)?;
                short_text(item, "kind", &at, 64)?;
                short_text(item, "title", &at, 500)?;
                opt_text(item, "view", &at, 64)?;
                if let Some(span) = item.get("span").filter(|s| !s.is_null()) {
                    let at = format!("{at}.span");
                    place(span, "path", &at)?;
                    let line = span.get("line").and_then(Value::as_u64).unwrap_or(0);
                    let end = span.get("end_line").and_then(Value::as_u64);
                    if line == 0 || end.is_none_or(|e| e != 0 && e < line) {
                        return Err(format!(
                            "{at} is {{path, line, end_line}}: lines from 1, end_line \
                             ≥ line (0: to the end)"
                        ));
                    }
                }
            }
            Ok(())
        }
        other => Err(format!(
            "no surface {other:?} (this chimaera draws {})",
            SURFACES.join(", ")
        )),
    }
}

/// Where a large surface's key is written in the plugin's output folder.
fn stored_path(output: &std::path::Path, surface: &str, key: &str) -> PathBuf {
    let name = crate::fs::sha256_hex(format!("{surface}\n{key}").as_bytes());
    output.join(".surfaces").join(format!(
        "{}-{}.json",
        surface.replace('/', "-"),
        &name[..24]
    ))
}

/// `publish` from `plugin` in `ws`: checked, kept, announced. `data`
/// `null` removes the key.
pub(crate) async fn publish(
    state: &AppState,
    plugin: &str,
    ws: &str,
    surface: &str,
    key: &str,
    text: &str,
) -> Result<(), String> {
    if !SURFACES.contains(&surface) {
        return Err(format!(
            "no surface {surface:?} (this chimaera draws {})",
            SURFACES.join(", ")
        ));
    }
    if key.is_empty() || key.len() > KEY_MAX {
        return Err(format!("a surface key is 1–{KEY_MAX} bytes"));
    }
    let max = if large(surface) { LARGE_MAX } else { SMALL_MAX };
    if text.len() > max {
        return Err(format!(
            "{surface} is at most {} KiB ({} KiB sent)",
            max >> 10,
            text.len() >> 10
        ));
    }
    let owned = text.to_string();
    let surface_owned = surface.to_string();
    let data = tokio::task::spawn_blocking(move || {
        let data: Value =
            serde_json::from_str(&owned).map_err(|e| format!("{surface_owned}: not JSON ({e})"))?;
        if !data.is_null() {
            check(&surface_owned, &data)?;
        }
        Ok::<_, String>(data)
    })
    .await
    .map_err(|e| format!("checking the surface failed: {e}"))??;
    let pair = (plugin.to_string(), ws.to_string());
    let skey = (surface.to_string(), key.to_string());
    let output = super::output::folder(&state.plugin_platform.output_root, plugin, ws);
    let held = if data.is_null() {
        None
    } else if large(surface) {
        let file = stored_path(&output, surface, key);
        let rel = file
            .strip_prefix(&output)
            .map(PathBuf::from)
            .unwrap_or_default();
        let bytes = text.as_bytes().to_vec();
        let dir = output.clone();
        tokio::task::spawn_blocking(move || super::output::write(&dir, &rel, &bytes))
            .await
            .map_err(|e| format!("storing the surface failed: {e}"))??;
        Some(Held::Stored(file))
    } else {
        Some(Held::Inline(Arc::new(data), text.len()))
    };
    // A stored file unpublished below, removed once the lock is let go.
    let mut dropped: Option<PathBuf> = None;
    {
        let mut surfaces = crate::lock(&state.plugin_platform.surfaces);
        let everyone_else = surfaces.inline_total((&pair, &skey));
        let held_map = surfaces.by_pair.entry(pair.clone()).or_default();
        match held {
            Some(held) => {
                let count = held_map.keys().filter(|(s, _)| s == surface).count();
                if !held_map.contains_key(&skey) && count >= KEYS_MAX {
                    return Err(format!(
                        "{surface}: at most {KEYS_MAX} keys per workspace (unpublish old ones)"
                    ));
                }
                if let Held::Inline(_, size) = &held {
                    let others: usize = held_map
                        .iter()
                        .filter(|(k, _)| **k != skey)
                        .map(|(_, h)| match h {
                            Held::Inline(_, n) => *n,
                            Held::Stored(_) => 0,
                        })
                        .sum();
                    if others + size > INLINE_BUDGET {
                        return Err(format!(
                            "{surface}: this plugin's published data here is capped at {} MiB \
                             (unpublish old keys)",
                            INLINE_BUDGET >> 20
                        ));
                    }
                    if everyone_else + size > INLINE_TOTAL {
                        return Err(format!(
                            "{surface}: the plugins' published data is capped at {} MiB on \
                             this host; try again later",
                            INLINE_TOTAL >> 20
                        ));
                    }
                }
                held_map.insert(skey, held);
            }
            None => {
                if let Some(Held::Stored(file)) = held_map.remove(&skey) {
                    dropped = Some(file);
                }
                if held_map.is_empty() {
                    surfaces.by_pair.remove(&pair);
                }
            }
        }
    }
    if let Some((dir, rel)) = dropped.as_deref().and_then(stored_parts) {
        let _ = tokio::task::spawn_blocking(move || super::output::remove(&dir, &rel)).await;
    }
    let frame = json!({
        "type": "surface",
        "plugin": plugin,
        "workspace": ws,
        "surface": surface,
        "key": key,
    });
    state.plugin_runtime.push_event(ws, frame.to_string());
    state.changes.notify_waiters();
    Ok(())
}

/// A stored surface's file as the output folder and the path beneath it
/// (`stored_path` puts it at `<output>/.surfaces/<name>`).
fn stored_parts(file: &std::path::Path) -> Option<(PathBuf, PathBuf)> {
    let dir = file.parent()?.parent()?.to_path_buf();
    let rel = file.strip_prefix(&dir).ok()?.to_path_buf();
    Some((dir, rel))
}

/// What `held` holds. A stored one is read again from a folder the
/// plugin's own programs can write: beneath it, never through a link,
/// capped, and checked again as the surface it claims to be.
async fn read_held(surface: &str, held: Held) -> Option<Value> {
    match held {
        Held::Inline(v, _) => Some((*v).clone()),
        Held::Stored(file) => {
            let surface = surface.to_string();
            tokio::task::spawn_blocking(move || {
                let (dir, rel) = stored_parts(&file)?;
                let bytes = super::output::read(&dir, &rel, 0, LARGE_MAX + 1).ok()?;
                if bytes.len() > LARGE_MAX {
                    return None;
                }
                let data: Value = serde_json::from_slice(&bytes).ok()?;
                check(&surface, &data).ok()?;
                Some(data)
            })
            .await
            .ok()
            .flatten()
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct SurfaceQuery {
    #[serde(default)]
    key: Option<String>,
    /// diagnostics/1: only this file's items; output/1: only this source's.
    #[serde(default)]
    file: Option<String>,
}

/// `GET /workspaces/{id}/surfaces/{kind}/{version}?key=&file=`: every
/// active plugin's published data of one surface in this workspace, as
/// `{"items": [{plugin, key, data}]}` (`file` narrows diagnostics to one
/// file and outputs to one source).
pub(crate) async fn route(
    State(state): State<Arc<AppState>>,
    AxPath((ws, kind, version)): AxPath<(String, String, String)>,
    Query(q): Query<SurfaceQuery>,
) -> Response {
    let surface = format!("{kind}/{version}");
    if !SURFACES.contains(&surface.as_str()) {
        return super::not_found(&format!("surface {surface}"));
    }
    if crate::lock(&state.workspaces).get(&ws).is_none() {
        return super::not_found(&format!("workspace {ws}"));
    }
    let active: Vec<String> = super::active(&state, &ws)
        .await
        .iter()
        .map(|m| m.id.clone())
        .collect();
    let wanted: Vec<(String, String, Held)> = {
        let surfaces = crate::lock(&state.plugin_platform.surfaces);
        active
            .iter()
            .filter_map(|p| {
                surfaces
                    .by_pair
                    .get(&(p.clone(), ws.clone()))
                    .map(|m| (p, m))
            })
            .flat_map(|(p, m)| {
                m.iter()
                    .filter(|((s, k), _)| {
                        *s == surface && q.key.as_ref().is_none_or(|want| want == k)
                    })
                    .map(move |((_, k), h)| (p.clone(), k.clone(), h.clone()))
            })
            .collect()
    };
    let mut items = Vec::new();
    for (plugin, key, held) in wanted {
        let Some(mut data) = read_held(&surface, held).await else {
            continue;
        };
        if let Some(file) = &q.file {
            match surface.as_str() {
                "diagnostics/1" => {
                    if let Some(list) = data.get_mut("items").and_then(Value::as_array_mut) {
                        list.retain(|i| i.get("file").and_then(Value::as_str) == Some(file));
                        if list.is_empty() {
                            continue;
                        }
                    }
                }
                "output/1" if data.get("source").and_then(Value::as_str) != Some(file) => continue,
                _ => {}
            }
        }
        items.push(json!({"plugin": plugin, "key": key, "data": data}));
    }
    Json(json!({"surface": surface, "items": items})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_are_checked() {
        let good = json!({"items": [{"file": "main.tex", "severity": "error", "line": 3, "message": "Undefined control sequence"}]});
        check("diagnostics/1", &good).unwrap();
        for (bad, why) in [
            (
                json!({"items": [{"file": "../x", "severity": "error", "line": 1, "message": "m"}]}),
                "workspace path",
            ),
            (
                json!({"items": [{"file": "a", "severity": "fatal", "line": 1, "message": "m"}]}),
                "severity",
            ),
            (
                json!({"items": [{"file": "a", "severity": "error", "line": 0, "message": "m"}]}),
                "line",
            ),
            (
                json!({"items": [{"file": "a", "severity": "error", "line": 1}]}),
                "message",
            ),
        ] {
            let err = check("diagnostics/1", &bad).unwrap_err();
            assert!(err.contains(why), "{bad}: {err}");
        }
        let many: Vec<Value> = (0..201)
            .map(|i| json!({"file": "a", "severity": "info", "line": i + 1, "message": "m"}))
            .collect();
        assert!(check("diagnostics/1", &json!({"items": many})).is_err());
        check(
            "output/1",
            &json!({"source": "main.tex", "output": "output:main.pdf", "state": "ok", "changed_pages": [1, 3]}),
        )
        .unwrap();
        assert!(check(
            "output/1",
            &json!({"source": "main.tex", "output": "/etc/x", "state": "ok"})
        )
        .is_err());
        check(
            "sourcemap/1",
            &json!({"output": "output:main.pdf", "files": ["main.tex"], "records": [[0, 1, 1, 10.0, 20.0, 100.0, 12.0]]}),
        )
        .unwrap();
        assert!(check(
            "sourcemap/1",
            &json!({"output": "output:m.pdf", "files": [], "records": [[1, 2]]})
        )
        .is_err());
        assert!(check("nope/1", &json!({})).is_err());
    }

    #[test]
    fn references_are_ids_with_plain_shapes() {
        let good = json!({
            "shapes": [{"kind": "label", "pattern": "sec:[a-z0-9-]+|fig:[a-z0-9-]+"}],
            "ids": [
                {"id": "sec:intro", "key": "main.tex#sec:intro", "kind": "label",
                 "title": "Introduction", "span": {"path": "main.tex", "line": 5, "end_line": 9}},
                {"id": "fig:dose", "key": "fig:dose", "kind": "label", "title": "Dose response",
                 "view": "document"},
                {"id": "sec:all", "key": "k", "kind": "label", "title": "t",
                 "span": {"path": "ch/one.tex", "line": 3, "end_line": 0}},
            ],
        });
        check("references/1", &good).unwrap();
        // Two repeats in one alternative (Mycelium's topic ids) are fine;
        // a class keeps its brackets' `*` and `+` to itself.
        assert_eq!(unbounded_per_alternative("T-\\d*[A-Za-z][A-Za-z0-9]*"), 2);
        assert_eq!(
            unbounded_per_alternative("sec:[a-z0-9-]+|fig:[a-z0-9-]+"),
            1
        );
        assert_eq!(unbounded_per_alternative("F-\\d{1,4}|[*+]x"), 0);
        assert_eq!(unbounded_per_alternative("a{2,}b\\+c+"), 2);
        for (bad, why) in [
            (json!({"shapes": [], "ids": []}), "shapes per key"),
            (
                json!({"shapes": [{"kind": "x", "pattern": "(a+)+"}], "ids": []}),
                "groups",
            ),
            (
                json!({"shapes": [{"kind": "x", "pattern": "\\w*\\w*\\w*Z"}], "ids": []}),
                "unbounded repeats",
            ),
            (
                json!({"shapes": [{"kind": "x", "pattern": "^F-\\d+$"}], "ids": []}),
                "without groups",
            ),
            (
                json!({"shapes": [{"kind": "x", "pattern": "F-\\d+"}],
                       "ids": [{"id": "F-1", "key": "k", "kind": "x", "title": "t",
                                "span": {"path": "../etc/passwd", "line": 1, "end_line": 1}}]}),
                "workspace path",
            ),
            (
                json!({"shapes": [{"kind": "x", "pattern": "F-\\d+"}],
                       "ids": [{"id": "F-1", "key": "k", "kind": "x", "title": "t",
                                "span": {"path": "a", "line": 4, "end_line": 2}}]}),
                "end_line",
            ),
            (
                json!({"shapes": [{"kind": "x", "pattern": "F-\\d+"}],
                       "ids": [{"id": "", "key": "k", "kind": "x", "title": "t"}]}),
                "ids[0].id",
            ),
        ] {
            let err = check("references/1", &bad).unwrap_err();
            assert!(err.contains(why), "{bad}: {err}");
        }
        let many: Vec<Value> = (0..5001)
            .map(|i| json!({"id": format!("F-{i}"), "key": i.to_string(), "kind": "x", "title": "t"}))
            .collect();
        assert!(check(
            "references/1",
            &json!({"shapes": [{"kind": "x", "pattern": "F-\\d+"}], "ids": many})
        )
        .is_err());
    }
}
