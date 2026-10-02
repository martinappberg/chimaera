//! The activity log: what each plugin did that the user should be able to
//! look back on — installs, updates, rollbacks and removals, trust granted
//! and withdrawn, blocks — and, once plugins run programs, every run and
//! download. Design: docs/design/plugin-platform-plan.md §2 ("The activity log").
//!
//! Append-only JSONL per plugin under `<data dir>/plugins/.activity/<id>.jsonl`
//! (a dot-name: the catalog's scan never reads it), capped at `FILE_MAX`:
//! past it the file becomes `<id>.jsonl.1` (replacing the older one) and a
//! fresh one starts. The card's Activity shows the last `SHOWN` entries.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use axum::extract::{Path as AxPath, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::AppState;

const DIR: &str = ".activity";
const FILE_MAX: u64 = 256 << 10;
/// One entry's cap: its fields are the host's own words and short values.
const ENTRY_MAX: usize = 4 << 10;
pub(crate) const SHOWN: usize = 50;

/// Appends are serialized daemon-wide (a rotation must not race a write).
static APPENDS: Mutex<()> = Mutex::new(());

fn file(root: &Path, id: &str) -> PathBuf {
    root.join(DIR).join(format!("{id}.jsonl"))
}

/// Append one entry for `id` (blocking): `entry` plus `ts`, one line.
pub(crate) fn append(root: &Path, id: &str, mut entry: Value) -> std::io::Result<()> {
    if !super::valid_id(id) {
        return Ok(());
    }
    entry["ts"] = json!(crate::timeline::now_ms());
    let mut line = entry.to_string();
    if line.len() > ENTRY_MAX {
        line =
            json!({"ts": entry["ts"], "kind": entry["kind"], "note": "entry too long"}).to_string();
    }
    line.push('\n');
    let _one = crate::lock(&APPENDS);
    let path = file(root, id);
    std::fs::create_dir_all(root.join(DIR))?;
    if std::fs::metadata(&path).is_ok_and(|m| m.len() + line.len() as u64 > FILE_MAX) {
        std::fs::rename(&path, path.with_extension("jsonl.1"))?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(line.as_bytes())
}

/// Record an entry for `id`, off the reactor. A failed write is logged,
/// never fatal: the change it describes already happened.
pub(crate) async fn record(state: &AppState, id: &str, entry: Value) {
    let (root, id) = (state.plugin_catalog.root.clone(), id.to_string());
    let written = tokio::task::spawn_blocking(move || append(&root, &id, entry)).await;
    if !matches!(written, Ok(Ok(()))) {
        tracing::warn!("a plugin activity entry could not be written");
    }
}

/// The last `n` entries for `id`, newest first (blocking).
pub(crate) fn recent(root: &Path, id: &str, n: usize) -> Vec<Value> {
    let path = file(root, id);
    let mut out: Vec<Value> = Vec::new();
    for p in [path.clone(), path.with_extension("jsonl.1")] {
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        out.extend(
            text.lines()
                .rev()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok()),
        );
        if out.len() >= n {
            break;
        }
    }
    out.truncate(n);
    out
}

/// GET /plugins/{pid}/activity — the card's Activity: the last entries,
/// newest first. Kept after a Remove (and so answered for any valid id).
pub(crate) async fn activity_route(
    State(state): State<std::sync::Arc<AppState>>,
    AxPath(pid): AxPath<String>,
) -> Response {
    if !super::valid_id(&pid) {
        return super::not_found("unknown plugin");
    }
    let root = state.plugin_catalog.root.clone();
    let id = pid.clone();
    let entries = tokio::task::spawn_blocking(move || recent(&root, &id, SHOWN))
        .await
        .unwrap_or_default();
    Json(json!({"id": pid, "entries": entries})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_append_rotate_and_read_back_newest_first() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-activity-{}-{}",
            std::process::id(),
            crate::timeline::now_ms()
        ));
        for i in 0..3 {
            append(&root, "demo", json!({"kind": "install", "n": i})).unwrap();
        }
        let got = recent(&root, "demo", 2);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0]["n"], 2);
        assert!(got[0]["ts"].as_u64().is_some());
        // Past the cap the file rotates; both halves still read.
        let big = "x".repeat(3000);
        for _ in 0..120 {
            append(&root, "demo", json!({"kind": "run", "args": big})).unwrap();
        }
        assert!(file(&root, "demo").with_extension("jsonl.1").is_file());
        assert!(std::fs::metadata(file(&root, "demo")).unwrap().len() <= FILE_MAX);
        assert_eq!(recent(&root, "demo", SHOWN).len(), SHOWN);
        // A path-shaped id writes nothing.
        append(&root, "../x", json!({"kind": "x"})).unwrap();
        assert!(!root.join("x.jsonl").exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
