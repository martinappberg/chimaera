//! Environment preludes: user-configured startup commands (`module load`,
//! `conda activate`, `export …`) run once per new session, after the user's
//! own rc files and before the shell or agent takes over. The text is opaque
//! POSIX shell — the daemon never parses it, which is what keeps every env
//! tool (lmod, conda, spack, venv, nix) working with zero tool-specific code.
//!
//! Scopes concatenate (startup ⊕ host ⊕ workspace ⊕ launch) rather than
//! override, and the result lands in a per-session file that the
//! shell-integration rc and the agent login-wrapper source via
//! `CHIMAERA_PRELUDE` (guarded by `CHIMAERA_PRELUDE_DONE`, so nested shells
//! never re-run it and reconnects — which are not spawns — never see it at
//! all).
//!
//! The outermost `startup` scope exists only in a cluster workspace job: the
//! job's startup commands (the cluster default ⊕ the workspace's ⊕ this
//! run's, composed by the app into the file `CHIMAERA_HOST_PRELUDE_FILE`
//! names). The job script deliberately doesn't run them itself; the daemon
//! applies them per spawn like every other scope. The file is read once per
//! spawn (edits apply to the next one), capped, off the reactor; without the
//! variable nothing is read at all.
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
/// terminal", tiny next to the daemon's budgets. Also the cap on the
/// launch-scope text a session-create request may carry.
pub(crate) const MAX_SCOPE_BYTES: usize = 32 * 1024;
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
    /// The effective prelude for one spawn: startup ⊕ host ⊕ workspace ⊕
    /// launch, in that order (concatenation, not override — commands run in
    /// sequence, which is the HPC mental model). `startup` is the cluster
    /// job's startup commands ([`job_startup`]), `None` everywhere else.
    /// Empty scopes are skipped; empty result means "set no
    /// CHIMAERA_PRELUDE at all".
    pub(crate) fn effective(
        &self,
        startup: Option<&str>,
        workspace_id: &str,
        launch: Option<&str>,
    ) -> String {
        self.compose(startup, Some(workspace_id), launch)
    }

    /// The startup and host scopes alone (a plugin tool's setup, which no
    /// workspace owns).
    pub(crate) fn host_only(&self, startup: Option<&str>) -> String {
        self.compose(startup, None, None)
    }

    /// [`Self::effective`] with the workspace scope explicit: None is the
    /// host scope alone, never "whatever an empty id happens to match".
    fn compose(
        &self,
        startup: Option<&str>,
        workspace_id: Option<&str>,
        launch: Option<&str>,
    ) -> String {
        let host = self.host.as_ref().map(|e| e.text.as_str());
        let workspace = workspace_id
            .and_then(|id| self.workspaces.get(id))
            .map(|e| e.text.as_str());
        let parts = [
            ("startup", startup),
            ("host", host),
            ("workspace", workspace),
            ("launch", launch),
        ];
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

/// The cluster job's startup commands: the text of the file
/// `CHIMAERA_HOST_PRELUDE_FILE` names, read now (so an edit applies to the
/// next spawn), capped at [`MAX_SCOPE_BYTES`]. `None` without the variable —
/// every daemon that isn't a cluster workspace job — or when the file is
/// missing, empty, or oversized. Blocking: call it off the reactor (or use
/// [`job_startup`]).
pub(crate) fn job_startup_blocking() -> Option<String> {
    let path = std::env::var_os(chimaera_core::cluster::ENV_HOST_PRELUDE_FILE)
        .filter(|p| !p.is_empty())?;
    read_startup(std::path::Path::new(&path))
}

/// [`job_startup_blocking`] from async code: no blocking hop at all without
/// the variable, a `spawn_blocking` read with it.
pub(crate) async fn job_startup() -> Option<String> {
    std::env::var_os(chimaera_core::cluster::ENV_HOST_PRELUDE_FILE).filter(|p| !p.is_empty())?;
    tokio::task::spawn_blocking(job_startup_blocking)
        .await
        .ok()
        .flatten()
}

fn read_startup(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        // A job whose startup file was never written runs with none.
        Err(err) if err.kind() == ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "cannot read the job's startup commands; spawning without them");
            return None;
        }
    };
    let mut bytes = Vec::new();
    if let Err(err) = file
        .take(MAX_SCOPE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
    {
        tracing::warn!(path = %path.display(), %err, "cannot read the job's startup commands; spawning without them");
        return None;
    }
    // Over the cap, the whole scope is dropped rather than cut mid-command:
    // half a startup script is worse than none.
    if bytes.len() > MAX_SCOPE_BYTES {
        tracing::warn!(path = %path.display(), "the job's startup commands exceed {MAX_SCOPE_BYTES} bytes; spawning without them");
        return None;
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    (!text.trim().is_empty() && !text.contains('\0')).then_some(text)
}

/// Compose and write the per-session prelude file; returns its path, or
/// None when no prelude applies (then no env var is set — zero behavior
/// delta for users without preludes). `startup` is [`job_startup`]'s text,
/// read by the async caller. Best-effort: a failed write degrades to "no
/// prelude" with a warning, never a failed spawn. Lives under the runtime
/// dir — hot state, reconstructible, night-scrub is fine because the file is
/// only read once at spawn.
pub(crate) fn materialize_prelude(
    state: &AppState,
    session_id: &str,
    workspace_id: &str,
    launch: Option<&str>,
    startup: Option<&str>,
) -> Option<PathBuf> {
    let text = crate::lock(&state.env_preludes)
        .current()
        .effective(startup, workspace_id, launch);
    write_prelude(&format!("{session_id}.sh"), &text)
}

/// [`materialize_prelude`] for an agent probe (`agent_probe`): the startup
/// and host scopes, plus the workspace's when the answer is per-workspace.
/// The name is unique per call — concurrent probes and sessions never share
/// a file — and the caller removes it with [`remove_prelude_path`] when
/// done. Blocking (it reads the startup file): callers run it off the
/// reactor.
pub(crate) fn materialize_probe_prelude(
    state: &AppState,
    workspace_id: Option<&str>,
) -> Option<PathBuf> {
    let startup = job_startup_blocking();
    let text =
        crate::lock(&state.env_preludes)
            .current()
            .compose(startup.as_deref(), workspace_id, None);
    let name = format!("probe-{}.sh", chimaera_core::generate_token());
    write_prelude(&name, &text)
}

fn write_prelude(file_name: &str, text: &str) -> Option<PathBuf> {
    if text.is_empty() {
        return None;
    }
    let dir = chimaera_core::runtime_dir().join("preludes");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        tracing::warn!(%err, "cannot create prelude dir; spawning without the prelude");
        return None;
    }
    let path = dir.join(file_name);
    match std::fs::write(&path, text) {
        Ok(()) => Some(path),
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "cannot write prelude; spawning without it");
            None
        }
    }
}

/// Best-effort removal of a session's prelude file (session teardown).
pub(crate) fn remove_prelude_file(session_id: &str) {
    let path = chimaera_core::runtime_dir()
        .join("preludes")
        .join(format!("{session_id}.sh"));
    remove_prelude_path(&path);
}

/// Best-effort removal of a materialized prelude file.
pub(crate) fn remove_prelude_path(path: &std::path::Path) {
    if let Err(err) = std::fs::remove_file(path) {
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
        assert_eq!(data.effective(None, "w-1", None), "");

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

        let text = data.effective(Some("ml slurm-tools\n"), "w-1", Some("export DEBUG=1"));
        let startup = text.find("ml slurm-tools").unwrap();
        let host = text.find("ml bcftools").unwrap();
        let ws = text.find("conda activate hello").unwrap();
        let launch = text.find("export DEBUG=1").unwrap();
        assert!(
            startup < host && host < ws && ws < launch,
            "order must be startup<host<workspace<launch"
        );
        assert!(text.starts_with("# chimaera prelude: startup\n"));
        assert!(text.ends_with('\n'));

        // Unknown workspace + blank-text workspace both contribute nothing.
        assert!(!data.effective(None, "w-other", None).contains("conda"));
        assert!(!data
            .effective(None, "w-blank", None)
            .contains("prelude: workspace"));
        // A blank startup scope is skipped like any other.
        assert!(!data
            .effective(Some(" \n"), "w-1", None)
            .contains("prelude: startup"));

        // Launch-only works with no stored preludes at all.
        let launch_only = EnvPreludes::default().effective(None, "w-1", Some("echo hi"));
        assert!(launch_only.contains("echo hi"));
    }

    #[test]
    fn compose_without_a_workspace_is_the_host_scope_alone() {
        let data = EnvPreludes {
            host: Some(PreludeEntry {
                text: "ml nodejs".into(),
            }),
            workspaces: BTreeMap::from([(
                String::new(),
                PreludeEntry {
                    text: "conda activate stray".into(),
                },
            )]),
        };
        let host_only = data.compose(None, None, None);
        assert!(host_only.contains("ml nodejs"));
        assert!(!host_only.contains("conda"), "{host_only}");
        assert!(data.compose(None, Some(""), None).contains("conda"));
        let with_startup = data.host_only(Some("ml cluster-default"));
        assert!(
            with_startup.find("ml cluster-default").unwrap()
                < with_startup.find("ml nodejs").unwrap()
        );
    }

    #[test]
    fn startup_file_reads_capped_and_whole() {
        let dir = std::env::temp_dir().join(format!("chimaera-startup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("startup.sh");
        std::fs::write(&file, "ml python\n").unwrap();
        assert_eq!(read_startup(&file).as_deref(), Some("ml python\n"));
        std::fs::write(&file, "  \n").unwrap();
        assert_eq!(read_startup(&file), None, "blank is no scope");
        std::fs::write(&file, "x".repeat(MAX_SCOPE_BYTES)).unwrap();
        assert!(read_startup(&file).is_some(), "the cap itself fits");
        std::fs::write(&file, "x".repeat(MAX_SCOPE_BYTES + 1)).unwrap();
        assert_eq!(read_startup(&file), None, "never half a script");
        assert_eq!(read_startup(&dir.join("missing.sh")), None);
        std::fs::remove_dir_all(&dir).ok();
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
