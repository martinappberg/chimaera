//! Original immutable portable-repository data. These values grant no filesystem,
//! account or live-install authority; the host validates prepared writes again.
use serde::{Deserialize, Serialize};
use std::path::Path;

pub use super::install::Write;
use super::transfer_host::MirrorGrant;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub head: Option<String>,
    pub config: Vec<Entry>,
    /// Absent on older snapshots: unknown staging, never an empty index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staging: Option<Descriptor>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Entry {
    pub key: String,
    pub value: String,
}

#[derive(Default)]
pub struct Described {
    /// The checked-out branch (`refs/heads/…`); none on a detached HEAD.
    pub branch: Option<String>,
    /// A safe `origin` URL (`mirror::repository_origin`).
    pub origin: Option<String>,
    /// HEAD and the portable configuration; none for a plain folder.
    pub snapshot: Option<Snapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StagingStatus {
    Uncaptured,
    Synced,
    Conflicts {
        paths: Vec<String>,
        total: usize,
        recovery: String,
    },
}

pub struct Prepared {
    pub branches: Vec<String>,
    pub writes: Vec<Write>,
    pub staging: StagingStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub version: u8,
    pub index: String,
}

pub struct ReturnRepository<'a> {
    pub original: &'a Path,
    pub checkout: &'a Path,
    pub stage: &'a Path,
    pub incoming: Incoming<'a>,
    pub published_handoff: Option<&'a str>,
    pub copy_checkpoint: Option<&'a super::execution::wire::Checkpoint>,
    pub check: &'a (dyn Fn() -> anyhow::Result<()> + Sync),
}

pub struct Incoming<'a> {
    pub cache: &'a Path,
    pub credentials: &'a MirrorGrant,
    pub branch: Option<&'a str>,
    pub origin: Option<&'a str>,
    pub snapshot: Option<&'a Snapshot>,
    pub staging: Option<StagingIncoming<'a>>,
}
pub struct StagingIncoming<'a> {
    pub handoff: &'a Path,
    pub baseline: Option<(&'a Path, &'a Descriptor)>,
}
