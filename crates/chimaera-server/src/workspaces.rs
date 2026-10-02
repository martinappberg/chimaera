//! Persistent workspace registry: `{id, root, name}` records stored as JSON.

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
    /// An internal workspace the native app keeps for its own windows (a
    /// cluster's login-node terminal): never listed (`GET /workspaces`), and
    /// its sessions never enter the ledger. Absent for every user workspace.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) hidden: bool,
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
    /// store (with a warning for the corrupt case). A retired plugin's
    /// switch is dropped everywhere, and the list saved once if any was on.
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
        let mut dropped = false;
        for w in &mut items {
            let before = w.plugins_on.len();
            w.plugins_on
                .retain(|p| crate::plugins::retired::of(p).is_none());
            dropped |= w.plugins_on.len() != before;
        }
        let mut store = WorkspaceStore {
            path,
            items,
            generation: 0,
            written: Arc::new(Mutex::new(0)),
        };
        if dropped {
            // Kept in memory either way: the next save carries it.
            if let Err(err) = store.save() {
                tracing::warn!(%err, "failed to persist dropping retired plugins' switches");
            }
        }
        store
    }

    pub(crate) fn list(&self) -> Vec<Workspace> {
        self.items.clone()
    }

    /// The workspaces a user sees: every one but the app's hidden ones.
    pub(crate) fn listed(&self) -> Vec<Workspace> {
        self.items.iter().filter(|w| !w.hidden).cloned().collect()
    }

    /// Ids of the hidden workspaces (their sessions stay out of the ledger).
    pub(crate) fn hidden_ids(&self) -> std::collections::HashSet<String> {
        self.items
            .iter()
            .filter(|w| w.hidden)
            .map(|w| w.id.clone())
            .collect()
    }

    /// Register `root` (canonical) as a hidden workspace, idempotent per
    /// root like [`Self::add`]; an existing record for it becomes hidden.
    pub(crate) fn add_hidden(&mut self, root: PathBuf) -> anyhow::Result<Workspace> {
        let mut workspace = self.add(root)?;
        if !workspace.hidden {
            workspace.hidden = true;
            if let Some(existing) = self.items.iter_mut().find(|w| w.id == workspace.id) {
                existing.hidden = true;
            }
            self.save()?;
        }
        Ok(workspace)
    }

    pub(crate) fn get(&self, id: &str) -> Option<Workspace> {
        self.items.iter().find(|w| w.id == id).cloned()
    }

    /// Register `root` (must already be canonical). Idempotent per canonical
    /// root; re-registering stamps the existing entry as freshly opened.
    pub(crate) fn add(&mut self, root: PathBuf) -> anyhow::Result<Workspace> {
        if let Some(existing) = self.items.iter_mut().find(|w| w.root == root) {
            existing.last_opened_at = unix_now();
            let workspace = existing.clone();
            self.save()?;
            return Ok(workspace);
        }
        let name = workspace_name(&root);
        let id = format!("w-{}", &chimaera_core::generate_token()[..8]);
        let workspace = Workspace {
            id,
            root,
            name,
            last_opened_at: unix_now(),
            mastermind: None,
            plugins_on: Vec::new(),
            hidden: false,
        };
        self.items.push(workspace.clone());
        self.save()?;
        Ok(workspace)
    }

    /// Make sure workspace `id` exists, under exactly this id: a cluster
    /// workspace job registers the workspace it was started for under the
    /// cluster's id, so every job of that workspace shares one identity
    /// (timeline, history, preludes and the ledger key by it). Present →
    /// left as it is (the user may have renamed it); missing → inserted
    /// FIRST, so a later [`Self::add`] of the same root lands on it even if
    /// an older record for that root exists. `root` must already be
    /// canonical. Returns whether it was inserted.
    pub(crate) fn seed(&mut self, id: &str, name: &str, root: PathBuf) -> anyhow::Result<bool> {
        if self.items.iter().any(|w| w.id == id) {
            return Ok(false);
        }
        let name = match name.trim() {
            "" => workspace_name(&root),
            name => name.to_string(),
        };
        self.items.insert(
            0,
            Workspace {
                id: id.to_string(),
                root,
                name,
                last_opened_at: unix_now(),
                mastermind: None,
                plugins_on: Vec::new(),
                hidden: false,
            },
        );
        self.save()?;
        Ok(true)
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

/// Boot: register the workspace a cluster workspace job was started for —
/// the JSON `WorkspaceSeed` (`{id, name, path}`) that
/// `CHIMAERA_CLUSTER_WORKSPACE` names. A no-op without the variable; a
/// missing, oversized or malformed seed is logged and skipped (the daemon
/// still serves; the app can open the folder by path). Blocking (reads and
/// canonicalizes on a shared filesystem): run it off the reactor, before
/// the ledger resurrects sessions into the workspace.
pub(crate) fn seed_cluster_workspace(state: &crate::AppState) {
    let Some(seed_path) = std::env::var_os(chimaera_core::cluster::ENV_CLUSTER_WORKSPACE)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
    else {
        return;
    };
    match read_seed(&seed_path) {
        Ok(seed) => {
            // Every registered root is canonical (fs routes compare against
            // it); a path that can't be resolved yet is kept as given.
            let given = PathBuf::from(&seed.path);
            let root = std::fs::canonicalize(&given).unwrap_or(given);
            match crate::lock(&state.workspaces).seed(&seed.id, &seed.name, root) {
                Ok(true) => {
                    tracing::info!(id = %seed.id, "registered this job's cluster workspace")
                }
                Ok(false) => {}
                Err(err) => tracing::warn!(%err, "could not save this job's cluster workspace"),
            }
        }
        Err(err) => {
            tracing::warn!(path = %seed_path.display(), %err, "cluster workspace seed skipped");
        }
    }
}

/// The seed file, validated: an id of the shape the cluster folder mints
/// (`w-` + 8 hex — it names folders and rides shell lines elsewhere) and an
/// absolute path.
fn read_seed(path: &Path) -> anyhow::Result<chimaera_core::cluster::WorkspaceSeed> {
    use std::io::Read;
    const MAX_SEED_BYTES: u64 = 64 * 1024;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_SEED_BYTES + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() as u64 <= MAX_SEED_BYTES, "seed too large");
    let seed: chimaera_core::cluster::WorkspaceSeed = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        chimaera_core::cluster::valid_workspace_id(&seed.id),
        "invalid workspace id {:?}",
        seed.id
    );
    anyhow::ensure!(
        Path::new(&seed.path).is_absolute(),
        "workspace path {:?} is not absolute",
        seed.path
    );
    Ok(seed)
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

    /// A cluster job's seed lands under exactly its id and name, first in
    /// the list (so opening the same root by path finds it), once; an
    /// existing record of that id is left as the user has it.
    #[test]
    fn seed_inserts_once_under_the_given_id() {
        let path = test_dir("seed").join("workspaces.json");
        let root = test_dir("seed-root");
        let mut store = WorkspaceStore::load(path.clone());
        let older = store.add(root.clone()).unwrap();
        assert!(store
            .seed("w-0000abcd", "Joint fold", root.clone())
            .unwrap());
        assert!(!store.seed("w-0000abcd", "renamed?", root.clone()).unwrap());
        let reloaded = WorkspaceStore::load(path.clone());
        let seeded = reloaded.get("w-0000abcd").unwrap();
        assert_eq!(seeded.name, "Joint fold");
        assert_eq!(seeded.root, root);
        assert_eq!(reloaded.list()[0].id, "w-0000abcd");
        let mut reloaded = reloaded;
        assert_eq!(reloaded.add(root.clone()).unwrap().id, "w-0000abcd");
        assert!(reloaded.get(&older.id).is_some(), "nothing else is dropped");
        // A blank name falls back to the folder's.
        assert!(reloaded.seed("w-0000abce", " ", root.clone()).unwrap());
        assert_eq!(
            reloaded.get("w-0000abce").unwrap().name,
            workspace_name(&root)
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn seed_files_are_validated() {
        let dir = test_dir("seed-file");
        let file = dir.join("workspace.json");
        let write = |v: &str| std::fs::write(&file, v).unwrap();
        write(r#"{"id":"w-0000abcd","name":"x","path":"/data/x"}"#);
        assert_eq!(read_seed(&file).unwrap().id, "w-0000abcd");
        write(r#"{"id":"w-../../x","name":"x","path":"/data/x"}"#);
        assert!(read_seed(&file).is_err());
        write(r#"{"id":"w-0000abcd","name":"x","path":"data/x"}"#);
        assert!(read_seed(&file).is_err());
        write("not json");
        assert!(read_seed(&file).is_err());
        assert!(read_seed(&dir.join("missing.json")).is_err());
        std::fs::remove_dir_all(&dir).ok();
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

    /// A retired plugin's switch (Agent notes, built in now) goes from every
    /// workspace at load, written once; a list without one isn't rewritten.
    #[test]
    fn load_drops_retired_plugins_switches_and_saves_once() {
        let dir = test_dir("retired-switches");
        let path = dir.join("workspaces.json");
        let record = |id: &str, on: &[&str]| serde_json::json!({"id": id, "root": dir.join(id), "name": id, "plugins_on": on});
        let list = serde_json::json!([
            record("w-a", &["agent-notes", "mycelium"]),
            record("w-b", &["agent-notes"]),
            record("w-c", &["latex"]),
        ]);
        std::fs::write(&path, serde_json::to_vec_pretty(&list).unwrap()).unwrap();
        let store = WorkspaceStore::load(path.clone());
        let on = |store: &WorkspaceStore, id: &str| store.get(id).unwrap().plugins_on;
        assert_eq!(on(&store, "w-a"), ["mycelium"]);
        assert!(on(&store, "w-b").is_empty());
        assert_eq!(on(&store, "w-c"), ["latex"]);
        assert_eq!(store.generation, 1, "saved once");
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("agent-notes"), "{raw}");
        let reloaded = WorkspaceStore::load(path.clone());
        assert_eq!(on(&reloaded, "w-a"), ["mycelium"]);
        assert_eq!(reloaded.generation, 0, "nothing to drop: not rewritten");
        std::fs::remove_dir_all(&dir).ok();
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
}
