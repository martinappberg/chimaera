//! Persistent workspace registry: `{id, root, name}` records stored as JSON.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
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
}

/// In-memory workspace list backed by a JSON file (save-on-change).
pub(crate) struct WorkspaceStore {
    path: PathBuf,
    items: Vec<Workspace>,
}

impl WorkspaceStore {
    /// Load the store from `path`. A missing or corrupt file yields an empty
    /// store (with a warning for the corrupt case).
    pub(crate) fn load(path: PathBuf) -> Self {
        let items = match std::fs::read_to_string(&path) {
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
        WorkspaceStore { path, items }
    }

    pub(crate) fn list(&self) -> Vec<Workspace> {
        self.items.clone()
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
        };
        self.items.push(workspace.clone());
        self.save()?;
        Ok(workspace)
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

    /// Atomically persist the list (tmp file + rename).
    fn save(&self) -> anyhow::Result<()> {
        crate::persist::atomic_write_json(&self.path, serde_json::to_vec_pretty(&self.items)?)
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
