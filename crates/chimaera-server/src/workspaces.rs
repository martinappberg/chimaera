//! Persistent workspace registry: `{id, root, name}` records stored as JSON.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// A registered workspace: a canonicalized directory the user opened.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Workspace {
    pub(crate) id: String,
    pub(crate) root: PathBuf,
    pub(crate) name: String,
    /// Unix seconds of the last open/activity; 0 for pre-upgrade records.
    #[serde(default)]
    pub(crate) last_opened_at: u64,
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
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.items)?)
            .with_context(|| format!("failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("failed to rename into {}", self.path.display()))?;
        Ok(())
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
