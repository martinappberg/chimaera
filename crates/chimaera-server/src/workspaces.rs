//! Persistent workspace registry: `{id, root, name}` records stored as JSON.

pub(crate) mod identity;

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How the workspace Mastermind's act-tier MCP tools are gated by its own
/// harness (the dashboard plan §6): `ask` pre-allows only the read tools
/// (every act call raises the agent's native permission prompt), `auto`
/// pre-allows the whole chimaera server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum MastermindMode {
    Ask,
    Auto,
}

/// The workspace's bound Mastermind: exactly one privileged chat session per
/// workspace (picked by the user), the only principal the act-tier MCP tools
/// answer to. Persisted on the Workspace so a daemon restart resurrects the
/// session with the same mode.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct MastermindCfg {
    pub(crate) session_id: String,
    pub(crate) mode: MastermindMode,
    /// The agent CLI behind the binding ("claude"/"codex"). Additive (empty
    /// for pre-upgrade records): the UI's mode-switch re-PUT must know the
    /// bound vendor even when the roster row is momentarily absent — the
    /// gone state, a restart gap — or a fallback guess would silently
    /// rotate a codex Mastermind into a claude one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) agent: String,
}

/// A registered workspace: a canonicalized directory the user opened.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Workspace {
    pub(crate) id: String,
    pub(crate) root: PathBuf,
    pub(crate) name: String,
    /// Unix seconds of the last open/activity; 0 for pre-upgrade records.
    #[serde(default)]
    pub(crate) last_opened_at: u64,
    /// The bound Mastermind, if the user appointed one. Additive wire field:
    /// absent for unbound workspaces (and for every pre-upgrade record).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mastermind: Option<MastermindCfg>,
    /// Workbench plugins the user switched on for THIS workspace (`plugins`)
    /// — the per-workspace Plugins page is where the switch lives, so the
    /// switch is per workspace. Additive wire field: absent when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) plugins_on: Vec<String>,
    /// Daemon-owned setup, never a user project or a mirror/adoption candidate.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) cloud_internal: bool,
}

/// What the caller learned from the folder (off the store's lock) for
/// [`WorkspaceStore::add_identified`].
pub(crate) struct FolderIdentity<'a> {
    /// The folder's own marker, if it has a well-formed one.
    pub(crate) marker: Option<&'a identity::Marker>,
    /// The root the registry holds under the marker's id, when the caller
    /// found it missing on disk: the only evidence that a folder was MOVED
    /// rather than duplicated.
    pub(crate) gone_root: Option<&'a Path>,
}

/// The outcome of [`WorkspaceStore::add_identified`].
pub(crate) struct Registered {
    pub(crate) workspace: Workspace,
    /// The folder does not yet say this id: write its marker, off the lock.
    pub(crate) write_marker: bool,
    /// The folder is a project that already existed (an entry of this
    /// registry, the id its marker names, or a moved entry), not a freshly
    /// minted id: opening it is the user picking that project up here.
    pub(crate) known: bool,
}

/// In-memory workspace list backed by a JSON file (save-on-change).
pub(crate) struct WorkspaceStore {
    path: PathBuf,
    items: Vec<Workspace>,
    /// Bumped per snapshot: what `written` compares.
    generation: u64,
    /// The generation last on disk, behind the one lock every write takes.
    written: Arc<Mutex<u64>>,
}

/// The list as of one change, to write off the store's lock (a plugin
/// switch: `workspaces.json` can live on NFS, and every route reads this
/// store).
pub(crate) struct Snapshot {
    path: PathBuf,
    bytes: Vec<u8>,
    generation: u64,
    written: Arc<Mutex<u64>>,
}

impl Snapshot {
    /// Write it — unless a later snapshot already reached the disk (it holds
    /// this change too): a slow write must never put an older list back.
    /// One write at a time; they share the temp file.
    pub(crate) fn write(self) -> anyhow::Result<()> {
        let mut written = crate::lock(&self.written);
        if *written >= self.generation {
            return Ok(());
        }
        crate::persist::atomic_write_json(&self.path, &self.bytes)?;
        *written = self.generation;
        Ok(())
    }
}

/// A plugin switch applied in memory, with the snapshot that makes it
/// durable; `undo_plugin_on(.., undo)` takes it back if that write fails.
pub(crate) struct PluginSwitch {
    pub(crate) workspace: Workspace,
    pub(crate) snapshot: Snapshot,
    pub(crate) undo: SwitchUndo,
}

/// What taking a staged switch back needs.
#[derive(Clone, Copy)]
pub(crate) struct SwitchUndo {
    on: bool,
    was_on: bool,
    generation: u64,
}

impl WorkspaceStore {
    /// Load the store from `path`. A missing or corrupt file yields an empty
    /// store (with a warning for the corrupt case).
    pub(crate) fn load(path: PathBuf) -> Self {
        let mut items: Vec<Workspace> = match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(items) => items,
                Err(err) => {
                    tracing::warn!(path = %path.display(), %err, "corrupt workspaces.json; starting with an empty workspace list");
                    Vec::new()
                }
            },
            Err(err) if err.kind() == ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "failed to read workspaces.json; starting with an empty workspace list");
                Vec::new()
            }
        };
        let mut migrated = false;
        for workspace in &mut items {
            // Older workers recorded only this daemon-reserved absolute path.
            // Persist the purpose once so ordinary names never drive filtering.
            if !workspace.cloud_internal && crate::cloud::is_onboarding_workspace(workspace) {
                workspace.cloud_internal = true;
                migrated = true;
            }
        }
        let mut store = WorkspaceStore {
            path,
            items,
            generation: 0,
            written: Arc::new(Mutex::new(0)),
        };
        if migrated {
            if let Err(error) = store.save() {
                tracing::warn!(%error, "could not persist internal workspace purpose");
            }
        }
        store
    }

    pub(crate) fn list(&self) -> Vec<Workspace> {
        self.items.clone()
    }

    pub(crate) fn get(&self, id: &str) -> Option<Workspace> {
        self.items.iter().find(|w| w.id == id).cloned()
    }

    /// Register `root` (must already be canonical). Idempotent per canonical
    /// root; re-registering stamps the existing entry as freshly opened. Mints
    /// a new id and reads no folder identity — the route registers through
    /// [`Self::add_identified`].
    pub(crate) fn add(&mut self, root: PathBuf) -> anyhow::Result<Workspace> {
        if let Some(existing) = self.items.iter_mut().find(|w| w.root == root) {
            existing.last_opened_at = unix_now();
            let workspace = existing.clone();
            self.save()?;
            return Ok(workspace);
        }
        let id = self.mint_id();
        self.push_new(id, root)
    }

    /// Register `root` (must already be canonical) the way the folder says
    /// it should be: a folder carries its workspace id, so a reinstall, a
    /// state reset or a second daemon reopens the same project instead of
    /// minting a stranger the cloud copy can never match. The id follows the
    /// folder to other computers too. The caller read the folder's marker off
    /// the reactor; the entry is decided here:
    ///
    /// | the folder and the registry | result | `write_marker` |
    /// |---|---|---|
    /// | `root` already registered | that entry, stamped opened | when the marker is missing or names another id |
    /// | no marker | fresh id | yes |
    /// | marker id not registered | registered under the marker's id | no |
    /// | marker id registered under another root that still exists | fresh id (this folder is a local duplicate) | yes |
    /// | marker id registered under another root that is gone | THAT entry moves to `root`, id kept (Pro state survives a move) | yes |
    ///
    /// "Gone" is the caller's finding ([`FolderIdentity::gone_root`]): a stat
    /// the store never makes under its own lock. Anything short of a
    /// confirmed missing root reads as a duplicate, which never disturbs the
    /// other entry. The caller writes the marker AFTER releasing the lock.
    pub(crate) fn add_identified(
        &mut self,
        root: PathBuf,
        folder: FolderIdentity<'_>,
    ) -> anyhow::Result<Registered> {
        if let Some(existing) = self.items.iter_mut().find(|w| w.root == root) {
            existing.last_opened_at = unix_now();
            let workspace = existing.clone();
            self.save()?;
            let write_marker = folder.marker.is_none_or(|marker| marker.id != workspace.id);
            return Ok(Registered {
                workspace,
                write_marker,
                known: true,
            });
        }
        let Some(marker) = folder.marker else {
            let id = self.mint_id();
            return Ok(Registered {
                workspace: self.push_new(id, root)?,
                write_marker: true,
                known: false,
            });
        };
        let Some(holder) = self.items.iter().position(|w| w.id == marker.id) else {
            return Ok(Registered {
                workspace: self.push_new(marker.id.clone(), root)?,
                write_marker: false,
                known: true,
            });
        };
        if folder.gone_root != Some(self.items[holder].root.as_path()) {
            let id = self.mint_id();
            return Ok(Registered {
                workspace: self.push_new(id, root)?,
                write_marker: true,
                known: false,
            });
        }
        let entry = &mut self.items[holder];
        // A name the user never changed follows the folder.
        if entry.name == workspace_name(&entry.root) {
            entry.name = workspace_name(&root);
        }
        entry.root = root;
        entry.last_opened_at = unix_now();
        let workspace = entry.clone();
        self.save()?;
        Ok(Registered {
            workspace,
            write_marker: true,
            known: true,
        })
    }

    fn mint_id(&self) -> String {
        loop {
            let id = format!("w-{}", &chimaera_core::generate_token()[..8]);
            if !self.items.iter().any(|w| w.id == id) {
                return id;
            }
        }
    }

    fn push_new(&mut self, id: String, root: PathBuf) -> anyhow::Result<Workspace> {
        let workspace = Workspace {
            id,
            name: workspace_name(&root),
            root,
            last_opened_at: unix_now(),
            mastermind: None,
            plugins_on: Vec::new(),
            cloud_internal: false,
        };
        self.items.push(workspace.clone());
        self.save()?;
        Ok(workspace)
    }

    pub(crate) fn add_internal(&mut self, root: PathBuf) -> anyhow::Result<Workspace> {
        let added = self.add(root)?;
        let entry = self.items.iter_mut().find(|w| w.id == added.id).unwrap();
        entry.cloud_internal = true;
        let workspace = entry.clone();
        self.save()?;
        Ok(workspace)
    }

    /// Transfer preserves IDs; a conflicting root/ID is never silently merged.
    pub(crate) fn import_exact(&mut self, workspace: Workspace) -> anyhow::Result<()> {
        if let Some(existing) = self
            .items
            .iter()
            .find(|entry| entry.id == workspace.id || entry.root == workspace.root)
        {
            if existing.id != workspace.id || existing.root != workspace.root {
                anyhow::bail!("workspace identity conflicts with an existing workspace");
            }
            return Ok(());
        }
        self.items.push(workspace);
        if let Err(error) = self.save() {
            self.items.pop();
            return Err(error);
        }
        Ok(())
    }

    /// Stamp `id` as freshly opened. Returns the workspace, or None if
    /// unknown.
    pub(crate) fn touch(&mut self, id: &str) -> Option<Workspace> {
        let entry = self.items.iter_mut().find(|w| w.id == id)?;
        entry.last_opened_at = unix_now();
        let workspace = entry.clone();
        if let Err(err) = self.save() {
            tracing::warn!(%err, "failed to persist workspace touch");
        }
        Some(workspace)
    }

    /// Set (or clear) `id`'s Mastermind binding, persisting on change.
    /// `Ok(None)` = unknown workspace; `Ok(Some(ws))` = applied AND durable;
    /// `Err` = the in-memory change stuck but the on-disk file could not be
    /// written. A binding that isn't durable is worse than a rejected one —
    /// it grants privileges the next restart forgets (or resurrects the wrong
    /// one) — so callers changing privilege MUST surface the error and roll
    /// the memory back, not report success (unlike `touch`, whose lost
    /// timestamp is cosmetic).
    pub(crate) fn set_mastermind(
        &mut self,
        id: &str,
        cfg: Option<MastermindCfg>,
    ) -> anyhow::Result<Option<Workspace>> {
        let Some(entry) = self.items.iter_mut().find(|w| w.id == id) else {
            return Ok(None);
        };
        entry.mastermind = cfg;
        let workspace = entry.clone();
        self.save()?;
        Ok(Some(workspace))
    }

    /// Switch plugin `pid` on or off for workspace `id`, in memory; the
    /// caller writes the snapshot off this lock and, if that fails, calls
    /// [`Self::undo_plugin_on`] — same durability contract as
    /// [`Self::set_mastermind`]: an agent-visible toggle the next restart
    /// forgets is worse than a refused one. `Ok(None)` = unknown workspace.
    pub(crate) fn stage_plugin_on(
        &mut self,
        id: &str,
        pid: &str,
        on: bool,
    ) -> anyhow::Result<Option<PluginSwitch>> {
        let Some(entry) = self.items.iter_mut().find(|w| w.id == id) else {
            return Ok(None);
        };
        let was_on = entry.plugins_on.iter().any(|p| p == pid);
        self.put_switch(id, pid, on);
        let Some(workspace) = self.get(id) else {
            return Ok(None);
        };
        let snapshot = match self.snapshot() {
            Ok(snapshot) => snapshot,
            Err(err) => {
                self.put_switch(id, pid, was_on);
                return Err(err);
            }
        };
        let undo = SwitchUndo {
            on,
            was_on,
            generation: snapshot.generation,
        };
        Ok(Some(PluginSwitch {
            workspace,
            snapshot,
            undo,
        }))
    }

    /// Take back a staged switch whose write failed — unless the list
    /// changed since: a later snapshot carries this switch as it is now,
    /// and whether that one lands is its writer's to say.
    pub(crate) fn undo_plugin_on(&mut self, id: &str, pid: &str, undo: SwitchUndo) {
        if self.generation == undo.generation && undo.on != undo.was_on {
            self.put_switch(id, pid, undo.was_on);
        }
    }

    fn put_switch(&mut self, id: &str, pid: &str, on: bool) {
        let Some(entry) = self.items.iter_mut().find(|w| w.id == id) else {
            return;
        };
        entry.plugins_on.retain(|p| p != pid);
        if on {
            entry.plugins_on.push(pid.to_string());
            entry.plugins_on.sort();
        }
    }

    /// Switch `pid` on or off and write it, under this lock. Tests only:
    /// the route writes off the lock.
    #[cfg(test)]
    pub(crate) fn set_plugin_on(
        &mut self,
        id: &str,
        pid: &str,
        on: bool,
    ) -> anyhow::Result<Option<Workspace>> {
        let Some(switch) = self.stage_plugin_on(id, pid, on)? else {
            return Ok(None);
        };
        if let Err(err) = switch.snapshot.write() {
            self.undo_plugin_on(id, pid, switch.undo);
            return Err(err);
        }
        Ok(Some(switch.workspace))
    }

    /// Switch `pid` off in every workspace — a plugin that is gone, or new
    /// under an id another publisher's plugin had: its switch must not bind
    /// whatever is installed under that id next. The snapshot to write, or
    /// None when no workspace had it on.
    pub(crate) fn clear_plugin(&mut self, pid: &str) -> anyhow::Result<Option<Snapshot>> {
        let mut changed = false;
        for w in &mut self.items {
            let before = w.plugins_on.len();
            w.plugins_on.retain(|p| p != pid);
            changed |= w.plugins_on.len() != before;
        }
        if !changed {
            return Ok(None);
        }
        self.snapshot().map(Some)
    }

    /// Clear `workspace_id`'s Mastermind binding IF it names `session_id`
    /// (the retire path: a dead Mastermind must not stay bound). Returns
    /// whether it did. Best-effort persistence: this runs on self-exit
    /// cleanup (`recents::retire`) where the session is already gone, so a
    /// failed write is logged, not propagated — there is no caller to abort.
    pub(crate) fn clear_mastermind_if(&mut self, workspace_id: &str, session_id: &str) -> bool {
        let bound = self.items.iter().any(|w| {
            w.id == workspace_id
                && w.mastermind
                    .as_ref()
                    .is_some_and(|m| m.session_id == session_id)
        });
        if bound {
            if let Err(err) = self.set_mastermind(workspace_id, None) {
                tracing::warn!(%err, "failed to persist mastermind unbind on retire");
            }
        }
        bound
    }

    /// workspace id -> bound Mastermind session id, for the roster snapshot
    /// (the additive `mastermind` wire flag is computed per snapshot, so it
    /// can never disagree with the store).
    pub(crate) fn mastermind_bindings(&self) -> std::collections::HashMap<String, String> {
        self.items
            .iter()
            .filter_map(|w| {
                w.mastermind
                    .as_ref()
                    .map(|m| (w.id.clone(), m.session_id.clone()))
            })
            .collect()
    }

    /// Unregister `id` (never touches the directory). Returns whether it
    /// existed.
    pub(crate) fn remove(&mut self, id: &str) -> anyhow::Result<bool> {
        let before = self.items.len();
        self.items.retain(|w| w.id != id);
        let removed = self.items.len() != before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// The list as it is now, stamped with the next generation.
    fn snapshot(&mut self) -> anyhow::Result<Snapshot> {
        let bytes = serde_json::to_vec_pretty(&self.items)?;
        self.generation += 1;
        Ok(Snapshot {
            path: self.path.clone(),
            bytes,
            generation: self.generation,
            written: self.written.clone(),
        })
    }

    /// Atomically persist the list (tmp file + rename), under this lock.
    fn save(&mut self) -> anyhow::Result<()> {
        self.snapshot()?.write()
    }
}

/// Display name for a workspace root: its basename, falling back to the full
/// path for roots like `/`.
fn workspace_name(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `session_id`'s Mastermind mode when it is `workspace_id`'s bound
/// Mastermind (`None` otherwise). The one binding lookup every respawn path
/// shares — the mode (not just the flag) because the codex spawn carries it
/// in argv.
pub(crate) fn workspace_mastermind_mode(
    state: &crate::AppState,
    workspace_id: &str,
    session_id: &str,
) -> Option<MastermindMode> {
    crate::lock(&state.workspaces)
        .get(workspace_id)
        .and_then(|w| w.mastermind)
        .filter(|m| m.session_id == session_id)
        .map(|m| m.mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("chimaera-ws-store-{label}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The Mastermind binding round-trips the store file: set persists, a
    /// fresh load reads it back (serde-lowercase mode included), clear
    /// persists too. Pre-binding records load with `mastermind: None`.
    #[test]
    fn mastermind_binding_round_trips_persistence() {
        let path = test_dir("mm-roundtrip").join("workspaces.json");
        let root = test_dir("mm-root");
        let mut store = WorkspaceStore::load(path.clone());
        let ws = store.add(root).unwrap();
        assert!(ws.mastermind.is_none());

        let cfg = MastermindCfg {
            session_id: "s-mm000001".to_string(),
            mode: MastermindMode::Auto,
            agent: "claude".to_string(),
        };
        let updated = store.set_mastermind(&ws.id, Some(cfg)).unwrap().unwrap();
        assert_eq!(
            updated.mastermind.as_ref().unwrap().session_id,
            "s-mm000001"
        );

        // The mode serializes lowercase (the wire + store contract).
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"mode\": \"auto\""), "{raw}");

        let reloaded = WorkspaceStore::load(path.clone());
        let back = reloaded.get(&ws.id).unwrap().mastermind.unwrap();
        assert_eq!(back.session_id, "s-mm000001");
        assert_eq!(back.mode, MastermindMode::Auto);
        assert_eq!(
            reloaded.mastermind_bindings(),
            std::collections::HashMap::from([(ws.id.clone(), "s-mm000001".to_string())])
        );

        // clear_mastermind_if only clears a MATCHING binding.
        let mut store = WorkspaceStore::load(path.clone());
        assert!(!store.clear_mastermind_if(&ws.id, "s-other"));
        assert!(store.clear_mastermind_if(&ws.id, "s-mm000001"));
        let reloaded = WorkspaceStore::load(path);
        assert!(reloaded.get(&ws.id).unwrap().mastermind.is_none());

        std::fs::remove_file(reloaded.path.clone()).ok();
    }

    /// A plugin switch is written off the store's lock, so two writes can
    /// land in either order: an older snapshot reaching the disk after a
    /// newer one is dropped (the newer one holds its change too).
    #[test]
    fn an_older_snapshot_never_overwrites_a_newer_one() {
        let path = test_dir("snap-order").join("workspaces.json");
        let mut store = WorkspaceStore::load(path.clone());
        let ws = store.add(test_dir("snap-order-root")).unwrap();
        let first = store.stage_plugin_on(&ws.id, "a", true).unwrap().unwrap();
        let second = store.stage_plugin_on(&ws.id, "b", true).unwrap().unwrap();
        second.snapshot.write().unwrap();
        first.snapshot.write().unwrap();
        let reloaded = WorkspaceStore::load(path.clone());
        assert_eq!(reloaded.get(&ws.id).unwrap().plugins_on, ["a", "b"]);
        std::fs::remove_file(path).ok();
    }

    /// A switch whose write failed is taken back — unless the list changed
    /// since (that later snapshot carries it; its writer decides).
    #[test]
    fn undo_takes_back_a_switch_only_when_nothing_changed_since() {
        let mut store = WorkspaceStore::load(test_dir("snap-undo").join("workspaces.json"));
        let ws = store.add(test_dir("snap-undo-root")).unwrap();
        let on = store.stage_plugin_on(&ws.id, "p", true).unwrap().unwrap();
        store.undo_plugin_on(&ws.id, "p", on.undo);
        assert!(store.get(&ws.id).unwrap().plugins_on.is_empty());

        let on = store.stage_plugin_on(&ws.id, "p", true).unwrap().unwrap();
        store.touch(&ws.id);
        store.undo_plugin_on(&ws.id, "p", on.undo);
        assert_eq!(
            store.get(&ws.id).unwrap().plugins_on,
            ["p"],
            "changed since: kept"
        );
    }

    /// A binding change whose persistence FAILS surfaces as `Err`, never a
    /// silent success — a privileged Mastermind that the disk never recorded
    /// would be forgotten (or the wrong one resurrected) on the next restart.
    #[test]
    fn set_mastermind_propagates_save_failure() {
        let dir = test_dir("mm-savefail");
        let mut store = WorkspaceStore::load(dir.join("workspaces.json"));
        let ws = store.add(test_dir("mm-savefail-root")).unwrap();
        // Redirect the store at a path whose parent is a regular file, so the
        // atomic temp-write + rename can't create its tempfile.
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"not a dir").unwrap();
        store.path = blocker.join("workspaces.json");
        let cfg = MastermindCfg {
            session_id: "s-mm000002".to_string(),
            mode: MastermindMode::Ask,
            agent: "claude".to_string(),
        };
        assert!(
            store.set_mastermind(&ws.id, Some(cfg)).is_err(),
            "a failed persist must surface as Err, not silent success"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    fn marker(id: &str) -> identity::Marker {
        identity::Marker {
            id: id.to_owned(),
            written_at: 1,
        }
    }

    /// A store standing in for one daemon's registry (a new label is a new
    /// daemon, e.g. after a reinstall or a state reset).
    fn daemon(label: &str) -> WorkspaceStore {
        WorkspaceStore::load(test_dir(label).join("workspaces.json"))
    }

    fn register(
        store: &mut WorkspaceStore,
        root: &Path,
        marker: Option<&identity::Marker>,
        gone_root: Option<&Path>,
    ) -> Registered {
        store
            .add_identified(
                root.to_path_buf(),
                FolderIdentity {
                    marker: marker.filter(|m| identity::valid_id(&m.id)),
                    gone_root,
                },
            )
            .unwrap()
    }

    /// Row: no marker. A fresh id, and the folder is asked to carry it.
    #[test]
    fn a_folder_without_a_marker_gets_a_fresh_id_and_a_marker() {
        let mut store = daemon("id-none");
        let root = test_dir("id-none-root");
        let registered = register(&mut store, &root, None, None);
        assert!(registered.write_marker);
        assert!(
            !registered.known,
            "a freshly minted id is not an existing project"
        );
        assert!(registered.workspace.id.starts_with("w-"));
        assert_eq!(registered.workspace.id.len(), 10);
        assert_eq!(store.get(&registered.workspace.id).unwrap().root, root);
    }

    /// Row: root already registered. Today's behaviour (idempotent, stamped
    /// opened); the marker is rewritten only when missing or naming another id.
    #[test]
    fn an_already_registered_root_keeps_its_entry_and_repairs_its_marker() {
        let mut store = daemon("id-known");
        let root = test_dir("id-known-root");
        let first = register(&mut store, &root, None, None).workspace;
        let matching = marker(&first.id);
        let again = register(&mut store, &root, Some(&matching), None);
        assert_eq!(again.workspace.id, first.id);
        assert!(again.known);
        assert!(!again.write_marker, "the folder already says so");
        let missing = register(&mut store, &root, None, None);
        assert_eq!(missing.workspace.id, first.id);
        assert!(missing.write_marker);
        // The registry is this daemon's truth for a root it has: a marker
        // naming another id is repaired, never followed.
        let other = marker("w-otherone");
        let wrong = register(&mut store, &root, Some(&other), None);
        assert_eq!(wrong.workspace.id, first.id);
        assert!(wrong.write_marker);
        assert_eq!(store.list().len(), 1);
    }

    /// Row: marker id not registered. The reinstall / state reset / second
    /// daemon case: the same folder is the same project, on any computer.
    #[test]
    fn a_marker_whose_id_is_not_registered_is_reused() {
        let root = test_dir("id-reuse-root");
        let mut first_daemon = daemon("id-reuse-1");
        let original = register(&mut first_daemon, &root, None, None).workspace;
        let carried = marker(&original.id);

        // A fresh registry (another install of the daemon, or another computer).
        let mut second_daemon = daemon("id-reuse-2");
        let reopened = register(&mut second_daemon, &root, Some(&carried), None);
        assert_eq!(reopened.workspace.id, original.id);
        assert!(reopened.known, "the folder names an existing project");
        assert!(!reopened.write_marker, "nothing to rewrite");
        assert_eq!(reopened.workspace.name, workspace_name(&root));
        assert!(reopened.workspace.last_opened_at > 0);
        // It is a real registration: it persists across a restart.
        let restarted = WorkspaceStore::load(second_daemon.path.clone());
        assert_eq!(restarted.get(&original.id).unwrap().root, root);
    }

    /// Row: marker id registered under another root that still exists. The
    /// folder is a local duplicate: fresh id, the original untouched.
    #[test]
    fn a_duplicate_of_a_registered_folder_gets_its_own_id() {
        let mut store = daemon("id-dup");
        let original_root = test_dir("id-dup-original");
        let copy_root = test_dir("id-dup-copy");
        let original = register(&mut store, &original_root, None, None).workspace;
        let carried = marker(&original.id);
        // The original is still on disk, so the caller found nothing gone.
        let copy = register(&mut store, &copy_root, Some(&carried), None);
        assert_ne!(copy.workspace.id, original.id);
        assert!(!copy.known, "a duplicate is a new project");
        assert!(copy.write_marker, "the copy is told its own id");
        assert_eq!(store.get(&original.id).unwrap().root, original_root);
        assert_eq!(store.list().len(), 2);
    }

    /// Row: marker id registered under a root that is gone. The folder moved:
    /// that entry follows it and keeps its id, name choice and bindings.
    #[test]
    fn a_moved_folder_keeps_its_workspace() {
        let mut store = daemon("id-move");
        let old_root = test_dir("id-move-old");
        let new_root = test_dir("id-move-new-name");
        let original = register(&mut store, &old_root, None, None).workspace;
        store.set_plugin_on(&original.id, "p", true).unwrap();
        let carried = marker(&original.id);
        let moved = register(&mut store, &new_root, Some(&carried), Some(&old_root));
        assert_eq!(moved.workspace.id, original.id);
        assert!(moved.known);
        assert!(moved.write_marker);
        assert_eq!(moved.workspace.root, new_root);
        assert_eq!(moved.workspace.plugins_on, ["p"], "the entry itself moved");
        assert_eq!(
            moved.workspace.name,
            workspace_name(&new_root),
            "a default name follows the folder"
        );
        assert_eq!(store.list().len(), 1);
        assert!(store.list().iter().all(|w| w.root != old_root));
        // A name the user chose stays.
        let mut named = daemon("id-move-named");
        let from = test_dir("id-move-named-from");
        let to = test_dir("id-move-named-to");
        let entry = register(&mut named, &from, None, None).workspace;
        named.items[0].name = "My thesis".to_owned();
        let carried = marker(&entry.id);
        let moved = register(&mut named, &to, Some(&carried), Some(&from));
        assert_eq!(moved.workspace.name, "My thesis");
        // The move survives a restart.
        let restarted = WorkspaceStore::load(named.path.clone());
        assert_eq!(restarted.get(&entry.id).unwrap().root, to);
    }

    /// "Gone" must name the root the registry actually holds: a stale or
    /// unrelated finding never moves an entry.
    #[test]
    fn only_a_matching_gone_root_moves_an_entry() {
        let mut store = daemon("id-gone-mismatch");
        let held = test_dir("id-gone-held");
        let unrelated = test_dir("id-gone-unrelated");
        let target = test_dir("id-gone-target");
        let original = register(&mut store, &held, None, None).workspace;
        let carried = marker(&original.id);
        let registered = register(&mut store, &target, Some(&carried), Some(&unrelated));
        assert_ne!(registered.workspace.id, original.id);
        assert_eq!(store.get(&original.id).unwrap().root, held);
    }

    /// Every registered id stays unique whatever the marker says.
    #[test]
    fn a_fresh_id_never_collides_with_a_registered_one() {
        let mut store = daemon("id-unique");
        let mut seen = std::collections::HashSet::new();
        for index in 0..64 {
            let root = test_dir(&format!("id-unique-{index}"));
            assert!(seen.insert(register(&mut store, &root, None, None).workspace.id));
        }
    }
}
