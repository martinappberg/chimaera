//! Saved remote hosts: the ssh aliases this machine has connected to (or the
//! user has added), stored as JSON at `~/.chimaera/hosts.json`. This is
//! client-side state — aliases resolve through the user's `~/.ssh/config`,
//! never through anything chimaera stores.

use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// A remembered remote host.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostEntry {
    /// The ssh alias (as found in `~/.ssh/config`) or `user@host` spec.
    pub alias: String,
    /// Explicit binary to deploy on this host, overriding dist lookup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<PathBuf>,
    pub added_at: u64,
    #[serde(default)]
    pub last_connected_at: Option<u64>,
}

/// In-memory host list backed by a JSON file (save-on-change).
pub struct HostsStore {
    path: PathBuf,
    items: Vec<HostEntry>,
}

impl HostsStore {
    /// Load from the default location (`~/.chimaera/hosts.json`).
    pub fn load_default() -> Self {
        Self::load(chimaera_core::data_dir().join("hosts.json"))
    }

    /// Load the store from `path`. A missing or corrupt file yields an empty
    /// store (with a warning for the corrupt case).
    pub fn load(path: PathBuf) -> Self {
        let items = match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(items) => items,
                Err(err) => {
                    tracing::warn!(path = %path.display(), %err, "corrupt hosts.json; starting with an empty host list");
                    Vec::new()
                }
            },
            Err(err) if err.kind() == ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "failed to read hosts.json; starting with an empty host list");
                Vec::new()
            }
        };
        HostsStore { path, items }
    }

    /// All hosts, most recently connected first (never-connected last, by
    /// added order).
    pub fn list(&self) -> Vec<HostEntry> {
        let mut out = self.items.clone();
        out.sort_by(|a, b| b.last_connected_at.cmp(&a.last_connected_at));
        out
    }

    pub fn get(&self, alias: &str) -> Option<HostEntry> {
        self.items.iter().find(|h| h.alias == alias).cloned()
    }

    /// Add `alias` (idempotent; an existing entry is returned unchanged,
    /// though a newly provided binary path replaces a missing one).
    pub fn add(&mut self, alias: &str, binary: Option<PathBuf>) -> anyhow::Result<HostEntry> {
        if let Some(existing) = self.items.iter_mut().find(|h| h.alias == alias) {
            if existing.binary.is_none() && binary.is_some() {
                existing.binary = binary;
                let entry = existing.clone();
                self.save()?;
                return Ok(entry);
            }
            return Ok(existing.clone());
        }
        let entry = HostEntry {
            alias: alias.to_string(),
            binary,
            added_at: unix_now(),
            last_connected_at: None,
        };
        self.items.push(entry.clone());
        self.save()?;
        Ok(entry)
    }

    /// Forget `alias`. Returns whether it existed.
    pub fn remove(&mut self, alias: &str) -> anyhow::Result<bool> {
        let before = self.items.len();
        self.items.retain(|h| h.alias != alias);
        let removed = self.items.len() != before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// Stamp a successful connection to `alias`, adding it if unknown.
    pub fn record_connected(&mut self, alias: &str) -> anyhow::Result<()> {
        match self.items.iter_mut().find(|h| h.alias == alias) {
            Some(entry) => entry.last_connected_at = Some(unix_now()),
            None => {
                self.items.push(HostEntry {
                    alias: alias.to_string(),
                    binary: None,
                    added_at: unix_now(),
                    last_connected_at: Some(unix_now()),
                });
            }
        }
        self.save()
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

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(tag: &str) -> (HostsStore, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("chimaera-hosts-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hosts.json");
        (HostsStore::load(path.clone()), dir)
    }

    #[test]
    fn add_list_remove_round_trip() {
        let (mut store, dir) = tmp_store("round-trip");
        assert!(store.list().is_empty());

        store.add("cluster", None).unwrap();
        store.add("cluster", None).unwrap(); // idempotent
        store.add("hpc2", Some(PathBuf::from("/tmp/bin"))).unwrap();
        assert_eq!(store.list().len(), 2);

        store.record_connected("cluster").unwrap();
        let reloaded = HostsStore::load(dir.join("hosts.json"));
        let list = reloaded.list();
        assert_eq!(list[0].alias, "cluster", "connected sorts first");
        assert!(list[0].last_connected_at.is_some());
        assert_eq!(list[1].binary, Some(PathBuf::from("/tmp/bin")));

        let mut reloaded = reloaded;
        assert!(reloaded.remove("cluster").unwrap());
        assert!(!reloaded.remove("cluster").unwrap());
        assert_eq!(reloaded.list().len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn record_connected_adds_unknown_alias() {
        let (mut store, dir) = tmp_store("record");
        store.record_connected("fresh").unwrap();
        assert_eq!(store.get("fresh").unwrap().alias, "fresh");
        std::fs::remove_dir_all(&dir).ok();
    }
}
