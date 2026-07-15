//! Environment preludes: user-configured startup commands (`module load`,
//! `conda activate`, `export …`) run once per new session, after the user's
//! own rc files and before the shell or agent takes over. The text is opaque
//! POSIX shell — the daemon never parses it, which is what keeps every env
//! tool (lmod, conda, spack, venv, nix) working with zero tool-specific code.
//!
//! Scopes concatenate (host ⊕ workspace ⊕ launch) rather than override, and
//! the result lands in a per-session file that the shell-integration rc and
//! the agent login-wrapper source via `CHIMAERA_PRELUDE` (guarded by
//! `CHIMAERA_PRELUDE_DONE`, so nested shells never re-run it and reconnects
//! — which are not spawns — never see it at all).
//!
//! Persisted at `~/.config/chimaera/env-profiles.json`; hand-edits are
//! first-class (reads re-stat the file, like `settings`). Entries are
//! objects, not bare strings, so named profiles can land later without a
//! shape break. No /ws/events frame in v1 — the Environment panel fetches on
//! mount and saves explicitly; concurrent editors are last-write-wins.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;

/// Per-scope text cap: generous for "commands you'd type into a fresh
/// terminal", tiny next to the daemon's budgets.
const MAX_SCOPE_BYTES: usize = 32 * 1024;
/// Raw PUT body / on-disk file cap (matches the settings store).
const MAX_FILE_BYTES: usize = 256 * 1024;

/// One scope's prelude. An object rather than a bare string so later slices
/// (named profiles) can add fields without a shape break.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct PreludeEntry {
    #[serde(default)]
    pub(crate) text: String,
}

/// The whole store: one host-wide prelude plus per-workspace ones, keyed by
/// workspace id. Deleted workspaces are pruned on explicit delete only — no
/// boot-time sweep, because a corrupt/missing `workspaces.json` loads as an
/// empty list and a sweep against it would wipe every workspace prelude; an
/// orphaned entry is a few KB of inert text.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct EnvPreludes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) host: Option<PreludeEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) workspaces: BTreeMap<String, PreludeEntry>,
}

impl EnvPreludes {
    /// The effective prelude for one spawn: host ⊕ workspace ⊕ launch, in
    /// that order (concatenation, not override — commands run in sequence,
    /// which is the HPC mental model). Empty scopes are skipped; empty
    /// result means "set no CHIMAERA_PRELUDE at all".
    pub(crate) fn effective(&self, workspace_id: &str, launch: Option<&str>) -> String {
        let host = self.host.as_ref().map(|e| e.text.as_str());
        let workspace = self.workspaces.get(workspace_id).map(|e| e.text.as_str());
        let parts = [("host", host), ("workspace", workspace), ("launch", launch)];
        let mut out = String::new();
        for (scope, text) in parts {
            let Some(text) = text.filter(|t| !t.trim().is_empty()) else {
                continue;
            };
            out.push_str("# chimaera prelude: ");
            out.push_str(scope);
            out.push('\n');
            out.push_str(text);
            if !text.ends_with('\n') {
                out.push('\n');
            }
        }
        out
    }

    /// Drop empty entries so "saved an empty editor" removes the scope
    /// rather than persisting `{text: ""}` husks.
    fn normalized(mut self) -> Self {
        if self.host.as_ref().is_some_and(|e| e.text.trim().is_empty()) {
            self.host = None;
        }
        self.workspaces.retain(|_, e| !e.text.trim().is_empty());
        self
    }
}

/// In-memory prelude store backed by `env-profiles.json`, mtime-checked on
/// read so external edits (vim over SSH) surface without a daemon restart.
pub(crate) struct EnvPreludeStore {
    path: PathBuf,
    data: EnvPreludes,
    /// mtime of the file the cached data was read from (None = no file).
    mtime: Option<SystemTime>,
}

impl EnvPreludeStore {
    /// Load from `path`. Missing, oversized, or corrupt files yield an empty
    /// store (with a warning for the corrupt case) — preludes must never
    /// brick the daemon.
    pub(crate) fn load(path: PathBuf) -> Self {
        let mut store = EnvPreludeStore {
            path,
            data: EnvPreludes::default(),
            mtime: None,
        };
        store.read_from_disk();
        store
    }

    fn read_from_disk(&mut self) {
        let (data, mtime) = match std::fs::read(&self.path) {
            Ok(bytes) if bytes.len() > MAX_FILE_BYTES => {
                tracing::warn!(path = %self.path.display(), "env-profiles.json exceeds {MAX_FILE_BYTES} bytes; ignoring");
                (EnvPreludes::default(), file_mtime(&self.path))
            }
            Ok(bytes) => match serde_json::from_slice::<EnvPreludes>(&bytes) {
                Ok(data) => (data, file_mtime(&self.path)),
                Err(err) => {
                    tracing::warn!(path = %self.path.display(), %err, "corrupt env-profiles.json; ignoring");
                    (EnvPreludes::default(), file_mtime(&self.path))
                }
            },
            Err(err) if err.kind() == ErrorKind::NotFound => (EnvPreludes::default(), None),
            Err(err) => {
                tracing::warn!(path = %self.path.display(), %err, "failed to read env-profiles.json");
                (EnvPreludes::default(), None)
            }
        };
        self.data = data;
        self.mtime = mtime;
    }

    /// The current preludes (mtime-checked against on-disk edits).
    pub(crate) fn current(&mut self) -> &EnvPreludes {
        if file_mtime(&self.path) != self.mtime {
            self.read_from_disk();
        }
        &self.data
    }

    /// Replace the whole store and persist (pretty-printed, atomic rename).
    pub(crate) fn put(&mut self, data: EnvPreludes) -> anyhow::Result<()> {
        let data = data.normalized();
        let mut body = serde_json::to_vec_pretty(&data)?;
        body.push(b'\n');
        crate::persist::atomic_write_json(&self.path, body)?;
        self.data = data;
        self.mtime = file_mtime(&self.path);
        Ok(())
    }

    /// Drop one workspace's entry (the explicit workspace-delete hook).
    pub(crate) fn remove_workspace(&mut self, workspace_id: &str) {
        let mut data = self.current().clone();
        if data.workspaces.remove(workspace_id).is_some() {
            if let Err(err) = self.put(data) {
                tracing::warn!(%err, "failed to prune workspace prelude");
            }
        }
    }

    /// Invalidate the mtime cache so the next read hits the disk. Tests
    /// rewrite the file within one mtime granule; real edits never do.
    #[cfg(test)]
    pub(crate) fn force_stale_for_tests(&mut self) {
        self.mtime = Some(std::time::UNIX_EPOCH);
    }
}

fn file_mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Compose and write the per-session prelude file; returns its path, or
/// None when no prelude applies (then no env var is set — zero behavior
/// delta for users without preludes). Best-effort: a failed write degrades
/// to "no prelude" with a warning, never a failed spawn. Lives under the
/// runtime dir — hot state, reconstructible, night-scrub is fine because
/// the file is only read once at spawn.
pub(crate) fn materialize_prelude(
    state: &AppState,
    session_id: &str,
    workspace_id: &str,
    launch: Option<&str>,
) -> Option<PathBuf> {
    let text = crate::lock(&state.env_preludes)
        .current()
        .effective(workspace_id, launch);
    if text.is_empty() {
        return None;
    }
    let dir = chimaera_core::runtime_dir().join("preludes");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        tracing::warn!(%err, "cannot create prelude dir; session spawns without its prelude");
        return None;
    }
    let path = dir.join(format!("{session_id}.sh"));
    match std::fs::write(&path, text) {
        Ok(()) => Some(path),
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "cannot write prelude; session spawns without it");
            None
        }
    }
}

/// Best-effort removal of a session's prelude file (session teardown).
pub(crate) fn remove_prelude_file(session_id: &str) {
    let path = chimaera_core::runtime_dir()
        .join("preludes")
        .join(format!("{session_id}.sh"));
    if let Err(err) = std::fs::remove_file(&path) {
        if err.kind() != ErrorKind::NotFound {
            tracing::debug!(%err, path = %path.display(), "prelude file cleanup failed");
        }
    }
}

/// GET /api/v1/environment — the whole prelude map.
pub(crate) async fn get_environment(State(state): State<Arc<AppState>>) -> Response {
    let data = crate::lock(&state.env_preludes).current().clone();
    Json(data).into_response()
}

/// PUT /api/v1/environment — replace the whole map; 204 on success. The
/// client sends back everything it fetched (whole-map semantics like
/// settings), so partial writes can't silently drop other workspaces.
pub(crate) async fn put_environment(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    if body.len() > MAX_FILE_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({"error": format!("environment preludes exceed {MAX_FILE_BYTES} bytes")})),
        )
            .into_response();
    }
    let data = match serde_json::from_slice::<EnvPreludes>(&body) {
        Ok(data) => data,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("invalid environment JSON: {err}")})),
            )
                .into_response();
        }
    };
    let texts = std::iter::once(&data.host)
        .filter_map(|h| h.as_ref())
        .map(|e| &e.text)
        .chain(data.workspaces.values().map(|e| &e.text));
    for text in texts {
        if text.len() > MAX_SCOPE_BYTES {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({"error": format!("a prelude exceeds {MAX_SCOPE_BYTES} bytes")})),
            )
                .into_response();
        }
        if text.contains('\0') {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "prelude text contains a NUL byte"})),
            )
                .into_response();
        }
    }
    if let Err(err) = crate::lock(&state.env_preludes).put(data) {
        tracing::error!(%err, "failed to persist environment preludes");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_concatenates_in_scope_order_and_skips_empties() {
        let mut data = EnvPreludes::default();
        assert_eq!(data.effective("w-1", None), "");

        data.host = Some(PreludeEntry {
            text: "ml bcftools".into(),
        });
        data.workspaces.insert(
            "w-1".into(),
            PreludeEntry {
                text: "conda activate hello\n".into(),
            },
        );
        data.workspaces.insert(
            "w-blank".into(),
            PreludeEntry {
                text: "  \n".into(),
            },
        );

        let text = data.effective("w-1", Some("export DEBUG=1"));
        let host = text.find("ml bcftools").unwrap();
        let ws = text.find("conda activate hello").unwrap();
        let launch = text.find("export DEBUG=1").unwrap();
        assert!(
            host < ws && ws < launch,
            "order must be host<workspace<launch"
        );
        assert!(text.ends_with('\n'));

        // Unknown workspace + blank-text workspace both contribute nothing.
        assert!(!data.effective("w-other", None).contains("conda"));
        assert!(!data
            .effective("w-blank", None)
            .contains("prelude: workspace"));

        // Launch-only works with no stored preludes at all.
        let launch_only = EnvPreludes::default().effective("w-1", Some("echo hi"));
        assert!(launch_only.contains("echo hi"));
    }

    #[test]
    fn normalized_drops_empty_entries() {
        let data = EnvPreludes {
            host: Some(PreludeEntry { text: "  ".into() }),
            workspaces: BTreeMap::from([
                (
                    "w-1".into(),
                    PreludeEntry {
                        text: String::new(),
                    },
                ),
                (
                    "w-2".into(),
                    PreludeEntry {
                        text: "ml git".into(),
                    },
                ),
            ]),
        }
        .normalized();
        assert!(data.host.is_none());
        assert_eq!(data.workspaces.len(), 1);
        assert!(data.workspaces.contains_key("w-2"));
    }
}
