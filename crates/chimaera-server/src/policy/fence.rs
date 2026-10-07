//! The durable fence an extension-less daemon still honours. A composed
//! daemon that handed a project to another machine leaves that fact in its
//! own state files; a later daemon without the extension must not start the
//! project's agents or change its files here, or the work would run twice.
//! Reading the files stays possible, so the user can always recover them.
//!
//! Read once at boot, bounded, never written. Any I/O error, damage or an
//! unknown format fences nothing: a free daemon is never blocked by state it
//! cannot read (it logs why instead).
use std::{
    collections::HashSet,
    io::Read,
    path::{Path, PathBuf},
};

use serde_json::Value;

const MAX_STATE: u64 = 1024 * 1024;
const MAX_COPIES: u64 = 16 * 1024;
const MAX_FENCED: usize = 512;

/// The projects a previous composed daemon recorded as running elsewhere,
/// moving, or as read-only copies.
#[derive(Default)]
pub struct Fence {
    fenced: HashSet<String>,
}

impl Fence {
    pub fn fenced(&self, workspace: &str) -> bool {
        !self.fenced.is_empty() && self.fenced.contains(workspace)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.fenced.len()
    }

    /// `<data>/pro/state.json` and `<data>/pro/copy-authority.json`, the
    /// records a composed daemon writes. Absent files are the ordinary case.
    pub fn load(data_dir: &Path) -> Self {
        let root = data_dir.join("pro");
        let mut fenced = HashSet::new();
        match read_bounded(&root.join("state.json"), MAX_STATE) {
            Ok(Some(bytes)) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(state) => from_state(&state, &mut fenced),
                Err(_) => tracing::warn!("ignoring an unreadable project fence record"),
            },
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "ignoring an unreadable project fence record"),
        }
        match read_bounded(&root.join("copy-authority.json"), MAX_COPIES) {
            Ok(Some(bytes)) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(copies) => {
                    for id in strings(&copies["workspaces"]) {
                        fenced.insert(id);
                    }
                }
                Err(_) => tracing::warn!("ignoring an unreadable read-only copy record"),
            },
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "ignoring an unreadable read-only copy record"),
        }
        let fenced = fenced
            .into_iter()
            .filter(|id| valid_id(id))
            .take(MAX_FENCED)
            .collect();
        Self { fenced }
    }
}

/// What still fences after a restart without the extension: ownership held
/// by another machine or in the middle of arriving, a project handed to the
/// cloud on quit, a registration waiting for a folder, a read-only copy.
/// Work this machine held (`local`, a transfer it never finished) is
/// ordinary local work again.
fn from_state(state: &Value, fenced: &mut HashSet<String>) {
    let parked: HashSet<String> = strings(&state["parked"]).collect();
    if let Some(ownership) = state["ownership"].as_object() {
        for (id, owner) in ownership {
            let fence = match owner["state"].as_str() {
                Some("remote" | "privacy_disabled" | "hydrating" | "setting_up") => true,
                Some("transferring") => parked.contains(id),
                _ => false,
            };
            if fence {
                fenced.insert(id.clone());
            }
        }
    }
    fenced.extend(strings(&state["legacy_pending"]));
    if let Some(roots) = state["import_roots"].as_object() {
        fenced.extend(roots.keys().cloned());
    }
    if let Some(preferences) = state["preferences"].as_object() {
        for (id, preference) in preferences {
            if !preference["copy"].is_null() {
                fenced.insert(id.clone());
            }
        }
    }
}

fn strings(value: &Value) -> impl Iterator<Item = String> + '_ {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| id.as_str().map(str::to_owned))
}

/// Same rule as the ids the daemon accepts for workspaces.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn read_bounded(path: &PathBuf, cap: u64) -> std::io::Result<Option<Vec<u8>>> {
    let file = match std::fs::File::open(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        other => other?,
    };
    let mut bytes = Vec::new();
    file.take(cap + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(std::io::Error::other("record exceeds its cap"));
    }
    Ok(Some(bytes))
}
