//! Persistent window registry: which windows are open, on which host and
//! workspace, and where they sit on screen — `~/.chimaera/windows.json`.
//!
//! This is what makes the window *set* survive an app restart (quit, crash,
//! self-update): on launch the shell reopens every recorded window instead
//! of collapsing to a lone home window. Each record carries a stable window
//! id that rides the window URL as `win=` — the SPA keys its daemon-side
//! view state (layout tree) on it, so a reopened window is the same window,
//! not a lookalike. Closing a window removes its record (macOS convention:
//! a deliberately closed window stays closed); quitting keeps them all.
//!
//! Geometry is stored in logical pixels so a restore lands correctly on
//! monitors with different scale factors.

use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::Context;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowRecord {
    /// Stable across app restarts; the SPA's view-state key (`win=` in the
    /// window URL hash).
    pub id: String,
    /// Host alias; `None` = the local daemon.
    pub alias: Option<String>,
    /// Workspace id; `None` = the home screen.
    pub ws: Option<String>,
    /// Outer position + inner size, logical pixels.
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
    #[serde(default)]
    pub width: Option<f64>,
    #[serde(default)]
    pub height: Option<f64>,
}

impl WindowRecord {
    pub fn new(alias: Option<String>, ws: Option<String>) -> Self {
        WindowRecord {
            // Matches the view-state key alphabet ([A-Za-z0-9_-]{1,64}).
            id: format!("w-{}", &chimaera_core::generate_token()[..16]),
            alias,
            ws,
            x: None,
            y: None,
            width: None,
            height: None,
        }
    }
}

/// The open-window set, backed by `windows.json` (save-on-change, atomic,
/// load-tolerant like every chimaera store).
pub struct WindowRegistry {
    path: PathBuf,
    items: Vec<WindowRecord>,
    /// Geometry updates only mark dirty; `save_if_dirty` (a slow tick +
    /// exit) persists them — a window drag must not write a file per event.
    dirty: bool,
}

impl WindowRegistry {
    pub fn load_default() -> Self {
        Self::load(chimaera_core::data_dir().join("windows.json"))
    }

    pub fn load(path: PathBuf) -> Self {
        let items = match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(items) => items,
                Err(err) => {
                    tracing::warn!(path = %path.display(), %err,
                        "corrupt windows.json; starting with an empty window set");
                    Vec::new()
                }
            },
            Err(err) if err.kind() == ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                tracing::warn!(path = %path.display(), %err,
                    "failed to read windows.json; starting with an empty window set");
                Vec::new()
            }
        };
        WindowRegistry {
            path,
            items,
            dirty: false,
        }
    }

    pub fn list(&self) -> Vec<WindowRecord> {
        self.items.clone()
    }

    /// Add or replace the record with `record.id`, persisting immediately
    /// (open/scope changes are rare and must survive a crash).
    pub fn upsert(&mut self, record: WindowRecord) {
        match self.items.iter_mut().find(|r| r.id == record.id) {
            Some(existing) => *existing = record,
            None => self.items.push(record),
        }
        self.persist();
    }

    /// Update what a window shows (the SPA swaps workspaces client-side).
    pub fn set_scope(&mut self, id: &str, alias: Option<String>, ws: Option<String>) {
        if let Some(record) = self.items.iter_mut().find(|r| r.id == id) {
            if record.alias != alias || record.ws != ws {
                record.alias = alias;
                record.ws = ws;
                self.persist();
            }
        }
    }

    /// Update where a window sits (logical pixels). Deferred persistence:
    /// call `save_if_dirty` on a slow cadence.
    pub fn set_geometry(&mut self, id: &str, x: f64, y: f64, width: f64, height: f64) {
        if let Some(record) = self.items.iter_mut().find(|r| r.id == id) {
            record.x = Some(x);
            record.y = Some(y);
            record.width = Some(width);
            record.height = Some(height);
            self.dirty = true;
        }
    }

    pub fn remove(&mut self, id: &str) {
        let before = self.items.len();
        self.items.retain(|r| r.id != id);
        if self.items.len() != before {
            self.persist();
        }
    }

    pub fn save_if_dirty(&mut self) {
        if self.dirty {
            self.persist();
        }
    }

    fn persist(&mut self) {
        self.dirty = false;
        if let Err(err) = self.save() {
            tracing::error!(%err, "failed to persist window registry");
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_round_trips_and_removes() {
        let dir =
            std::env::temp_dir().join(format!("chimaera-windows-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("windows.json");

        let mut reg = WindowRegistry::load(path.clone());
        assert!(reg.list().is_empty());

        let mut record = WindowRecord::new(Some("cluster".into()), Some("ws-1".into()));
        let id = record.id.clone();
        reg.upsert(record.clone());
        reg.set_geometry(&id, 10.0, 20.0, 1280.0, 840.0);
        reg.save_if_dirty();
        record.ws = Some("ws-2".into());
        reg.set_scope(&id, record.alias.clone(), record.ws.clone());

        let loaded = WindowRegistry::load(path.clone());
        let rows = loaded.list();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].alias.as_deref(), Some("cluster"));
        assert_eq!(rows[0].ws.as_deref(), Some("ws-2"));
        assert_eq!(rows[0].width, Some(1280.0));

        let mut loaded = loaded;
        loaded.remove(&id);
        assert!(WindowRegistry::load(path).list().is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }
}
