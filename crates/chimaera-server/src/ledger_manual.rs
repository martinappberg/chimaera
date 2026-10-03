//! Bounded provenance for explicit same-ID manual resume. A live process alone
//! never proves a duplicate request originally resumed a manual conversation.
use crate::{agents::AgentKind, ledger::LedgerEntry, AppState};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::PathBuf,
};
const MAX: usize = 128 * 1024;
const LIMIT: usize = 64;
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipt {
    session_id: String,
    workspace_id: String,
    native_id: String,
    kind: String,
}
impl Receipt {
    pub(crate) fn for_entry(entry: &LedgerEntry) -> Result<Self> {
        let agent = entry
            .agent
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("manual resume agent missing"))?;
        let receipt = Self {
            session_id: entry.id.clone(),
            workspace_id: entry.workspace_id.clone(),
            native_id: agent
                .resume
                .clone()
                .ok_or_else(|| anyhow::anyhow!("manual resume native identity missing"))?,
            kind: agent.kind.as_str().into(),
        };
        receipt.validate()?;
        Ok(receipt)
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            crate::workspaces::identity::valid_id(&self.session_id)
                && crate::workspaces::identity::valid_id(&self.workspace_id)
                && crate::codex_notify::valid_thread_id(&self.native_id)
                && matches!(
                    AgentKind::parse(&self.kind),
                    Some(AgentKind::Claude | AgentKind::Codex)
                ),
            "manual resume identity invalid"
        );
        Ok(())
    }
    pub(crate) fn matches(&self, entry: &LedgerEntry) -> bool {
        Self::for_entry(entry).is_ok_and(|other| self == &other)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipts {
    version: u16,
    entries: Vec<Receipt>,
}
impl Receipts {
    pub(crate) fn contains(&self, entry: &LedgerEntry) -> bool {
        self.entries.iter().any(|receipt| receipt.matches(entry))
    }
    pub(crate) fn retain(&mut self, state: &AppState, entry: &LedgerEntry) -> Result<()> {
        if self.contains(entry) {
            return Ok(());
        }
        // Only positive missing/dead originals may be forgotten. An in-flight
        // same-ID lifecycle still retains its receipt, even between registries.
        self.entries.retain(|receipt| {
            state
                .chat
                .get(&receipt.session_id)
                .is_some_and(|row| row.alive)
                || state
                    .sessions
                    .get(&receipt.session_id)
                    .is_some_and(|row| row.alive)
                || crate::lock(&state.deferred_sessions).contains_key(&receipt.session_id)
                || crate::lock(&state.chat_switching).contains_key(&receipt.session_id)
        });
        ensure!(
            self.entries.len() < LIMIT,
            "manual resume receipt capacity reached"
        );
        ensure!(
            !self
                .entries
                .iter()
                .any(|receipt| receipt.session_id == entry.id),
            "manual resume identity changed"
        );
        self.entries.push(Receipt::for_entry(entry)?);
        Ok(())
    }
}
fn path(state: &AppState) -> PathBuf {
    crate::pro::manual_resume_storage(state).join("manual-resumes.json")
}
pub(crate) fn load(state: &AppState) -> Result<Receipts> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(path(state))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Receipts {
                version: 1,
                entries: Vec::new(),
            })
        }
        Err(_) => anyhow::bail!("manual resume receipt unavailable"),
    };
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.len() <= MAX as u64
            && meta.uid() == unsafe { nix::libc::geteuid() }
            && meta.mode() & 0o077 == 0,
        "manual resume receipt storage invalid"
    );
    let mut bytes = Vec::new();
    file.take(MAX as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX, "manual resume receipt oversized");
    let receipts: Receipts = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("manual resume receipt invalid"))?;
    ensure!(
        receipts.version == 1 && receipts.entries.len() <= LIMIT,
        "manual resume receipt invalid"
    );
    let mut ids = BTreeSet::new();
    for receipt in &receipts.entries {
        receipt.validate()?;
        ensure!(
            ids.insert(&receipt.session_id),
            "manual resume receipt duplicate"
        );
    }
    Ok(receipts)
}
pub(crate) fn save(state: &AppState, receipts: &Receipts) -> Result<()> {
    let bytes = serde_json::to_vec(receipts)?;
    ensure!(bytes.len() <= MAX, "manual resume receipt oversized");
    let parent = crate::pro::manual_resume_storage(state);
    let meta = std::fs::symlink_metadata(parent)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { nix::libc::geteuid() } && meta.mode() & 0o077 == 0,
        "manual resume storage invalid"
    );
    let temporary = parent.join(format!(
        ".manual-resume-{}.tmp",
        chimaera_core::generate_token()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path(state))?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    let _ = std::fs::remove_file(temporary);
    result
}
