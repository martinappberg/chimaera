//! Bounded private durable parking receipt. It never carries a secret value,
//! process snapshot or authority to resume; restart keeps conversations manual.
use crate::{ledger::LedgerEntry, AppState};
use anyhow::{ensure, Context, Result};
use chimaera_core::project_secret_idle::{Leader, Prepare, Prepared, Reply};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::OpenOptions,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::PathBuf,
};
#[cfg(test)]
use std::{fs::File, io::Write};
const MAX: usize = 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    version: u16,
    pub(super) prepare: Prepare,
    pub(super) fence_id: String,
    pub(super) leaders: Vec<Leader>,
    entries: Vec<serde_json::Value>,
}
impl Record {
    #[cfg(test)]
    pub(super) fn new(
        prepare: Prepare,
        fence_id: String,
        leaders: Vec<Leader>,
        entries: &[LedgerEntry],
    ) -> Result<Self> {
        let record = Self {
            version: 1,
            prepare,
            fence_id,
            leaders,
            entries: entries.iter().map(LedgerEntry::to_json).collect(),
        };
        record.validate()?;
        Ok(record)
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1 && self.entries.len() <= 64,
            "maintenance park record invalid"
        );
        chimaera_core::project_secret_idle::Request::Prepare(self.prepare.clone()).validate()?;
        Reply::Prepared(Prepared {
            version: 1,
            request_id: self.prepare.request_id,
            binding: self.prepare.binding.clone(),
            attempt_id: self.prepare.attempt_id.clone(),
            operation_id: self.prepare.operation_id.clone(),
            pending_id: self.prepare.pending_id.clone(),
            expected_applied_revision: self.prepare.expected_applied_revision,
            fence_id: self.fence_id.clone(),
            remaining_ms: 1,
            leaders: self.leaders.clone(),
        })
        .validate()?;
        let entries = self.sessions()?;
        let ids: BTreeSet<_> = entries.iter().map(|entry| entry.id.as_str()).collect();
        ensure!(
            ids.len() == entries.len()
                && entries.iter().all(|entry| entry.workspace_id
                    == self.prepare.binding.workspace_id
                    && crate::workspaces::identity::valid_id(&entry.id)
                    && entry.agent.as_ref().is_some_and(|agent| agent.ui
                        == chimaera_agent::model::SessionUi::Chat
                        && agent
                            .resume
                            .as_ref()
                            .is_some_and(|id| crate::codex_notify::valid_thread_id(id)))),
            "maintenance park sessions invalid"
        );
        ensure!(
            self.leaders.len() == entries.len()
                && self
                    .leaders
                    .iter()
                    .all(|leader| ids.contains(leader.session_id.as_str())),
            "maintenance park leaders invalid"
        );
        Ok(())
    }
    pub(super) fn sessions(&self) -> Result<Vec<LedgerEntry>> {
        self.entries
            .iter()
            .map(|value| LedgerEntry::from_json(value).context("maintenance park session invalid"))
            .collect()
    }
}
fn path(state: &AppState) -> PathBuf {
    state.pro.root.join("maintenance-park.json")
}
pub(super) fn read(state: &AppState) -> Result<Option<Record>> {
    let path = path(state);
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => anyhow::bail!("maintenance park record unavailable"),
    };
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.len() <= MAX as u64
            && meta.uid() == unsafe { nix::libc::geteuid() }
            && meta.mode() & 0o077 == 0,
        "maintenance park storage invalid"
    );
    let mut bytes = Vec::new();
    file.take(MAX as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX, "maintenance park record oversized");
    let record: Record = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("maintenance park record invalid"))?;
    record.validate()?;
    Ok(Some(record))
}
#[cfg(test)]
pub(super) fn write(state: &AppState, record: &Record) -> Result<()> {
    record.validate()?;
    let bytes = serde_json::to_vec(record)?;
    ensure!(bytes.len() <= MAX, "maintenance park record oversized");
    if let Some(previous) = read(state)? {
        ensure!(
            previous.prepare.identity() == record.prepare.identity()
                && previous.fence_id == record.fence_id,
            "maintenance park attempt retained"
        );
    }
    let target = path(state);
    let parent = target.parent().context("maintenance storage missing")?;
    let parent_meta = std::fs::symlink_metadata(parent)?;
    ensure!(
        parent_meta.is_dir()
            && parent_meta.uid() == unsafe { nix::libc::geteuid() }
            && parent_meta.mode() & 0o077 == 0,
        "maintenance storage invalid"
    );
    let temporary = parent.join(format!(
        ".maintenance-{}.tmp",
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
        std::fs::rename(&temporary, &target)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    let _ = std::fs::remove_file(temporary);
    result
}
#[cfg(test)]
pub(super) fn remove(state: &AppState, record: &Record) -> Result<()> {
    if let Some(current) = read(state)? {
        ensure!(
            current.prepare.identity() == record.prepare.identity()
                && current.fence_id == record.fence_id,
            "maintenance park identity changed"
        );
        let target = path(state);
        std::fs::remove_file(&target)?;
        File::open(target.parent().context("maintenance storage missing")?)?.sync_all()?;
    }
    Ok(())
}

/// Receipt before-images supersede the ordinary ledger on restart. They never
/// resume a leader or turn an old launch into a new maintenance authorization.
pub(in crate::pro) fn overlay_boot(
    state: &AppState,
    boot: &mut crate::ledger::BootLedger,
) -> Result<()> {
    let Some(record) = read(state)? else {
        return Ok(());
    };
    for mut entry in record.sessions()? {
        entry.suspended = true;
        entry.manual_resume_reason = Some("project_secrets_idle".into());
        if let Some(previous) = boot
            .sessions
            .iter_mut()
            .find(|previous| previous.id == entry.id)
        {
            ensure!(
                previous.workspace_id == entry.workspace_id,
                "maintenance park session changed"
            );
            *previous = entry;
        } else {
            ensure!(
                boot.sessions.len() < 512,
                "maintenance park session capacity"
            );
            boot.sessions.push(entry);
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::pro::execution::legacy_parking_fixture::Fixture;
    use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
    fn record(fixture: &Fixture) -> Record {
        // No agent is still positive process-census work for the supervisor;
        // the empty receipt is not itself proof of idle.
        Record::new(fixture.prepare(), "A".repeat(43), Vec::new(), &[]).unwrap()
    }
    #[tokio::test]
    async fn receipt_is_exact_bounded_and_durably_removed() {
        let fixture = Fixture::new().await;
        let record = record(&fixture);
        assert!(read(&fixture.state).unwrap().is_none());
        write(&fixture.state, &record).unwrap();
        assert_eq!(
            read(&fixture.state).unwrap().unwrap().fence_id,
            record.fence_id
        );
        let mut changed = record.clone();
        changed.prepare.attempt_id = "55555555-5555-4555-8555-555555555555".into();
        assert!(write(&fixture.state, &changed).is_err());
        assert!(remove(&fixture.state, &changed).is_err());
        remove(&fixture.state, &record).unwrap();
        remove(&fixture.state, &record).unwrap();
        assert!(read(&fixture.state).unwrap().is_none());
    }
    #[tokio::test]
    async fn malformed_trailing_oversized_and_unsafe_storage_never_becomes_missing() {
        let fixture = Fixture::new().await;
        let record = record(&fixture);
        write(&fixture.state, &record).unwrap();
        let target = path(&fixture.state);
        let bytes = std::fs::read(&target).unwrap();
        for bad in [
            b"{".to_vec(),
            [bytes.as_slice(), b" true"].concat(),
            vec![b' '; MAX + 1],
        ] {
            std::fs::write(&target, bad).unwrap();
            assert!(read(&fixture.state).is_err());
        }
        std::fs::write(&target, &bytes).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read(&fixture.state).is_err());
        std::fs::remove_file(&target).unwrap();
        std::os::unix::fs::symlink("missing", &target).unwrap();
        assert!(read(&fixture.state).is_err());
        std::fs::remove_file(&target).unwrap();
        let c = std::ffi::CString::new(target.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { nix::libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let before = std::time::Instant::now();
        assert!(read(&fixture.state).is_err());
        assert!(before.elapsed() < std::time::Duration::from_secs(1));
    }
    #[tokio::test]
    async fn durable_receipt_cannot_restore_automatically_after_partial_ledger_write() {
        let fixture = Fixture::new().await;
        let mut record = record(&fixture);
        let entry = LedgerEntry::from_json(&serde_json::json!({
            "id":"s-fixture", "workspace_id":"w-a", "cwd":"/synthetic",
            "cols":80,"rows":24,"theme":"dark", "agent":{"kind":"claude",
                "resume":"11111111-1111-4111-8111-111111111111", "title":"fixture", "ui":"chat"}
        }))
        .unwrap();
        record.entries = vec![entry.to_json()];
        record.leaders = vec![Leader {
            session_id: entry.id.clone(),
            namespace_pid: 42,
            start_ticks: 7,
        }];
        write(&fixture.state, &record).unwrap();
        let mut boot = crate::ledger::BootLedger::default();
        overlay_boot(&fixture.state, &mut boot).unwrap();
        assert_eq!(boot.sessions.len(), 1);
        assert!(boot.sessions[0].suspended);
        assert_eq!(
            boot.sessions[0].manual_resume_reason.as_deref(),
            Some("project_secrets_idle")
        );
        assert_eq!(
            boot.sessions[0].agent.as_ref().unwrap().resume,
            entry.agent.as_ref().unwrap().resume
        );
    }
}
