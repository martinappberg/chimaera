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
//! first-class and read stale-while-revalidate, like `settings`: hand-edits
//! reach the cache within `settings::SETTINGS_POLL` (the shared off-reactor
//! poll, [`refresh`]); routes refresh on read; PUT/PATCH install at once.
//! Spawn paths ([`materialize_prelude`], [`materialize_probe_prelude`]) and
//! [`EnvPreludeStore::data_cached`] readers compose from the cache — no stat
//! or read under the store mutex. Entries are
//! objects, not bare strings, so named profiles can land later without a
//! shape break. No /ws/events frame in v1 — the Environment panel fetches on
//! mount and saves explicitly. PATCH changes only the scopes it names, so two
//! windows editing different scopes both keep their edit; two edits of the
//! SAME scope are last-write-wins.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::persist::{DiskGate, Encode, Hook, Look};
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

/// In-memory prelude store backed by `env-profiles.json`; external edits
/// (vim over SSH) surface without a daemon restart through [`refresh`].
///
/// All disk work after boot runs on the blocking pool, one job at a time
/// (`gate`), with this struct's mutex held only to snapshot and to install —
/// never across a stat, read or write. Readers get the cache.
pub(crate) struct EnvPreludeStore {
    path: PathBuf,
    data: EnvPreludes,
    /// mtime of the file the cached data was read from (None = no file).
    mtime: Option<SystemTime>,
    gate: DiskGate,
    /// Test hook run on the blocking pool before each off-lock read (see
    /// [`crate::persist::Hook`]).
    before_read: Option<Hook>,
}

/// One look at env-profiles.json (settings.rs's store reads the same way).
type Disk = Look<EnvPreludes>;

/// [`crate::persist::look`] for env-profiles.json: a corrupt file is empty.
fn look(path: &Path, recorded: Option<SystemTime>, hook: Option<&Hook>) -> Disk {
    crate::persist::look(
        path,
        recorded,
        MAX_FILE_BYTES,
        "env-profiles.json",
        hook,
        |bytes| {
            serde_json::from_slice(bytes).unwrap_or_else(|err| {
                tracing::warn!(path = %path.display(), %err, "corrupt env-profiles.json; ignoring");
                EnvPreludes::default()
            })
        },
    )
}

/// The pretty-printed file body, refused over the read cap: a file the
/// next read would ignore must never be written.
fn encode(data: &EnvPreludes) -> Result<Vec<u8>, WriteError> {
    crate::persist::encode_pretty(data, MAX_FILE_BYTES).map_err(|err| match err {
        Encode::TooLarge => WriteError::TooLarge,
        Encode::Failed(err) => WriteError::Failed(err.into()),
    })
}

/// Why a write didn't land.
enum WriteError {
    TooLarge,
    Unreadable,
    Failed(anyhow::Error),
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
            gate: DiskGate::default(),
            before_read: None,
        };
        store.refresh();
        store
    }

    /// Fold one look in; true when the preludes changed.
    fn install(&mut self, disk: Disk) -> bool {
        let Disk::Loaded(data, mtime) = disk else {
            return false;
        };
        self.mtime = mtime;
        if data == self.data {
            return false;
        }
        self.data = data;
        true
    }

    /// Blocking: only [`Self::load`], before the store is shared.
    fn refresh(&mut self) {
        let disk = look(&self.path, self.mtime, None);
        self.install(disk);
    }

    /// The cached preludes, never a disk re-stat — safe on the reactor and
    /// under any lock. Hand-edits arrive through [`refresh`] (the shared
    /// poll); a caller that must see one made in the last poll period awaits
    /// [`snapshot`] instead.
    pub(crate) fn data_cached(&self) -> &EnvPreludes {
        &self.data
    }

    /// Invalidate the mtime cache so the next read hits the disk. Tests
    /// rewrite the file within one mtime granule; real edits never do.
    #[cfg(test)]
    pub(crate) fn force_stale_for_tests(&mut self) {
        self.mtime = Some(std::time::UNIX_EPOCH);
    }

    /// Run `hook` on the blocking pool right before each off-lock read.
    #[cfg(test)]
    pub(crate) fn set_before_read_for_tests(&mut self, hook: Hook) {
        self.before_read = Some(hook);
    }
}

/// What one refresh found.
enum Refreshed {
    /// The cache is current; `changed` when this refresh moved it.
    Fresh { changed: bool },
    /// The file couldn't be read: the cache is kept but may be stale.
    Unreadable,
}

/// One off-lock refresh. Blocking; runs inside the store's [`DiskGate`]. The
/// mutex is taken twice, briefly: to snapshot the path and recorded mtime,
/// and to install. Every install runs inside the same gate, so the mtime
/// re-check before installing is a guard, not a race anyone expects to lose.
fn refresh_job(state: &AppState) -> Refreshed {
    let (path, recorded, hook) = {
        let store = crate::lock(&state.env_preludes);
        (store.path.clone(), store.mtime, store.before_read.clone())
    };
    match look(&path, recorded, hook.as_ref()) {
        Disk::Same => Refreshed::Fresh { changed: false },
        Disk::Unreadable => Refreshed::Unreadable,
        disk => {
            let mut store = crate::lock(&state.env_preludes);
            let changed = store.mtime == recorded && store.install(disk);
            Refreshed::Fresh { changed }
        }
    }
}

/// Write `data` (already normalized) and install it. Blocking; inside the gate.
fn write_job(state: &AppState, data: EnvPreludes) -> Result<(), WriteError> {
    let body = encode(&data)?;
    let path = crate::lock(&state.env_preludes).path.clone();
    let mtime =
        crate::persist::atomic_write_json_modified(&path, body).map_err(WriteError::Failed)?;
    crate::lock(&state.env_preludes).install(Disk::Loaded(data, mtime));
    Ok(())
}

/// Run `job` as this store's next disk job.
async fn run_job<T: Send + 'static>(
    state: &Arc<AppState>,
    job: impl FnOnce(&AppState) -> T + Send + 'static,
) -> anyhow::Result<T> {
    let gate = crate::lock(&state.env_preludes).gate.clone();
    let job_state = state.clone();
    gate.run(move || job(&job_state)).await
}

/// Fold an on-disk edit into the store, off the reactor; true when the
/// preludes changed. Clones nothing: the shared poll runs it every
/// `settings::SETTINGS_POLL`.
pub(crate) async fn refresh(state: &Arc<AppState>) -> bool {
    match run_job(state, refresh_job).await {
        Ok(Refreshed::Fresh { changed }) => changed,
        Ok(Refreshed::Unreadable) => false,
        Err(err) => {
            tracing::warn!(%err, "environment refresh failed");
            false
        }
    }
}

/// The current preludes, refreshed from disk off the reactor.
pub(crate) async fn snapshot(state: &Arc<AppState>) -> EnvPreludes {
    refresh(state).await;
    crate::lock(&state.env_preludes).data.clone()
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
/// read by the async caller. The stored scopes come from the cache (the
/// module header says how stale it can be). Best-effort: a failed write
/// degrades to "no prelude" with a warning, never a failed spawn. Lives under
/// the runtime dir — hot state, reconstructible, night-scrub is fine because
/// the file is only read once at spawn.
pub(crate) fn materialize_prelude(
    state: &AppState,
    session_id: &str,
    workspace_id: &str,
    launch: Option<&str>,
    startup: Option<&str>,
) -> Option<PathBuf> {
    let text = crate::lock(&state.env_preludes)
        .data
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
            .data
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
    Json(snapshot(&state).await).into_response()
}

fn error(status: StatusCode, message: String) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

/// The checks every scope's text passes, on PUT and PATCH alike: the
/// refusal to answer, if any.
fn check_texts<'a>(texts: impl Iterator<Item = &'a str>) -> Option<Response> {
    for text in texts {
        if text.len() > MAX_SCOPE_BYTES {
            return Some(error(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("a prelude exceeds {MAX_SCOPE_BYTES} bytes"),
            ));
        }
        if text.contains('\0') {
            return Some(error(
                StatusCode::BAD_REQUEST,
                "prelude text contains a NUL byte".into(),
            ));
        }
    }
    None
}

fn body_too_large(body: &Bytes) -> Option<Response> {
    (body.len() > MAX_FILE_BYTES).then(|| {
        error(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("environment preludes exceed {MAX_FILE_BYTES} bytes"),
        )
    })
}

/// The answer to a finished write job.
fn written(outcome: anyhow::Result<Result<(), WriteError>>) -> Response {
    match outcome {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(WriteError::TooLarge)) => error(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("environment preludes exceed {MAX_FILE_BYTES} bytes"),
        ),
        Ok(Err(WriteError::Unreadable)) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "env-profiles.json can't be read right now; nothing was changed".into(),
        ),
        Ok(Err(WriteError::Failed(err))) | Err(err) => {
            tracing::error!(%err, "failed to persist environment preludes");
            error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
        }
    }
}

/// PUT /api/v1/environment — replace the whole map; 204 on success. Kept
/// for older clients; a window that edits one scope sends PATCH, so it
/// can't put back another window's older text for a scope it never touched.
pub(crate) async fn put_environment(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    if let Some(refused) = body_too_large(&body) {
        return refused;
    }
    let data = match serde_json::from_slice::<EnvPreludes>(&body) {
        Ok(data) => data,
        Err(err) => {
            return error(
                StatusCode::BAD_REQUEST,
                format!("invalid environment JSON: {err}"),
            );
        }
    };
    let texts = data
        .host
        .iter()
        .chain(data.workspaces.values())
        .map(|e| e.text.as_str());
    if let Some(refused) = check_texts(texts) {
        return refused;
    }
    let data = data.normalized();
    written(run_job(&state, move |state| write_job(state, data)).await)
}

/// A PATCH body: a scope present replaces it, `null` deletes it, absent
/// leaves it alone. Unknown top-level keys are refused, so a misspelt scope
/// can't answer 204 having changed nothing.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnvPatch {
    #[serde(default, deserialize_with = "present")]
    host: Option<Option<PreludeEntry>>,
    #[serde(default)]
    workspaces: BTreeMap<String, Option<PreludeEntry>>,
}

/// Wraps whatever the key held in `Some`, so `"host": null` reads as
/// `Some(None)` while an absent key keeps `default`'s `None`.
fn present<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}

impl EnvPatch {
    fn texts(&self) -> impl Iterator<Item = &str> {
        self.host
            .iter()
            .flatten()
            .chain(self.workspaces.values().flatten())
            .map(|e| e.text.as_str())
    }

    fn apply(self, mut data: EnvPreludes) -> EnvPreludes {
        if let Some(host) = self.host {
            data.host = host;
        }
        for (id, entry) in self.workspaces {
            match entry {
                Some(entry) => {
                    data.workspaces.insert(id, entry);
                }
                None => {
                    data.workspaces.remove(&id);
                }
            }
        }
        data.normalized()
    }
}

/// The PATCH's read-modify-write. Blocking; inside the store's gate, so two
/// windows' patches apply one after the other, each to the other's result.
/// The file is re-read first: a hand-edit since the last read is kept.
fn patch_job(state: &AppState, patch: EnvPatch) -> Result<(), WriteError> {
    if let Refreshed::Unreadable = refresh_job(state) {
        return Err(WriteError::Unreadable);
    }
    let current = crate::lock(&state.env_preludes).data.clone();
    let next = patch.apply(current.clone());
    if next == current {
        return Ok(());
    }
    write_job(state, next)
}

/// Drop one workspace's entry (the explicit workspace-delete hook): a PATCH
/// of that scope to null, so it queues behind — never races — a window's
/// PATCH that read the store before the delete.
pub(crate) async fn remove_workspace(state: &Arc<AppState>, workspace_id: &str) {
    let patch = EnvPatch {
        host: None,
        workspaces: BTreeMap::from([(workspace_id.to_string(), None)]),
    };
    match run_job(state, move |state| patch_job(state, patch)).await {
        Ok(Ok(())) => {}
        Ok(Err(WriteError::Failed(err))) | Err(err) => {
            tracing::warn!(%err, "failed to prune workspace prelude");
        }
        Ok(Err(WriteError::TooLarge | WriteError::Unreadable)) => {
            tracing::warn!("env-profiles.json can't be rewritten; workspace prelude kept");
        }
    }
}

/// PATCH /api/v1/environment — change only the scopes named; 204 on success.
pub(crate) async fn patch_environment(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    if let Some(refused) = body_too_large(&body) {
        return refused;
    }
    let patch = match serde_json::from_slice::<EnvPatch>(&body) {
        Ok(patch) => patch,
        Err(err) => {
            return error(
                StatusCode::BAD_REQUEST,
                format!("invalid environment JSON: {err}"),
            );
        }
    };
    if let Some(refused) = check_texts(patch.texts()) {
        return refused;
    }
    written(run_job(&state, move |state| patch_job(state, patch)).await)
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
