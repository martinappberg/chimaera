//! Bounded delivery evidence, independent of the lossy conversation journal.
//!
//! Dispatch is recorded before handing input to a driver. Without its echo we
//! cannot prove the agent did not receive it, even after a clean process exit.
//! A replacement must therefore refuse automatic replay. This is delivery
//! evidence, not an exactly-once guarantee for an agent's external actions.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::{model, ClientIdState, SendUncertain, RETAINED_SENDS_MAX};

pub const MAX_BYTES: usize = 32 * 1024;
const MAX_STORES: usize = 512;
const IO_WAIT: Duration = Duration::from_secs(2);
const REQUIRED: &[u8] = b"1\n";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum State {
    Dispatching,
    Confirmed,
    Withdrawn,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    id: String,
    state: State,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    session_id: String,
    entries: VecDeque<Entry>,
}

impl Snapshot {
    fn new(session_id: &str) -> Self {
        Self {
            version: 1,
            session_id: session_id.into(),
            entries: VecDeque::new(),
        }
    }

    fn validate(&self, session_id: &str) -> Result<()> {
        ensure!(
            self.version == 1 && self.session_id == session_id,
            "send state binding mismatch"
        );
        ensure!(
            self.entries.len() <= model::CLIENT_IDS_REMEMBERED + RETAINED_SENDS_MAX,
            "send state exceeds limit"
        );
        let mut ids = HashSet::new();
        let mut pending = 0;
        for entry in &self.entries {
            ensure!(
                model::valid_client_id(&entry.id) && ids.insert(&entry.id),
                "invalid send state id"
            );
            pending += usize::from(entry.state == State::Dispatching);
        }
        ensure!(
            pending <= RETAINED_SENDS_MAX,
            "unresolved send limit reached"
        );
        ensure!(
            self.entries.len() - pending <= model::CLIENT_IDS_REMEMBERED,
            "settled send limit reached"
        );
        Ok(())
    }

    fn get(&self, id: &str) -> Option<State> {
        self.entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.state)
    }

    fn merge_legacy(&mut self, legacy: &[(String, ClientIdState)]) -> Result<()> {
        for (id, confirmed) in legacy {
            if self.get(id).is_none() {
                self.remember(
                    id,
                    match confirmed {
                        ClientIdState::Confirmed => State::Confirmed,
                        ClientIdState::Cancelled => State::Withdrawn,
                        _ => State::Dispatching,
                    },
                )?;
            }
        }
        Ok(())
    }

    fn remember(&mut self, id: &str, state: State) -> Result<()> {
        ensure!(model::valid_client_id(id), "invalid send state id");
        if let Some(old) = self.get(id) {
            ensure!(
                !matches!(
                    (old, state),
                    (State::Withdrawn, State::Confirmed | State::Dispatching)
                        | (State::Confirmed, State::Withdrawn)
                        | (State::Dispatching, State::Withdrawn)
                ),
                "conflicting send evidence"
            );
            // A stale export or delayed dispatch must not weaken a receipt.
            if old == State::Confirmed || old == state {
                return Ok(());
            }
            self.entries.retain(|entry| entry.id != id);
        }
        if state == State::Dispatching {
            ensure!(
                self.entries
                    .iter()
                    .filter(|entry| entry.state == State::Dispatching)
                    .count()
                    < RETAINED_SENDS_MAX,
                "unresolved send limit reached"
            );
        }
        self.entries.push_back(Entry {
            id: id.into(),
            state,
        });
        while self
            .entries
            .iter()
            .filter(|entry| entry.state != State::Dispatching)
            .count()
            > model::CLIENT_IDS_REMEMBERED
        {
            let index = self
                .entries
                .iter()
                .position(|entry| entry.state != State::Dispatching)
                .expect("settled entry exists");
            self.entries.remove(index);
        }
        Ok(())
    }
}

/// Where a store keeps its evidence. Only a session whose work can move to
/// another machine (managed execution) needs evidence that survives a power
/// loss and a handoff, at the price of synced writes per send and failing
/// closed on damage. Every other chat keeps the same record in memory (the
/// Pass 48 contract): no disk write per send, and an earlier sidecar is read
/// when it is sound and ignored, with a log line, when it is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Receipts {
    Durable,
    Memory,
}

pub(crate) struct Store {
    path: PathBuf,
    required: PathBuf,
    snapshot: Mutex<Snapshot>,
    /// Set once by the first durable opener; never cleared while cached.
    durable: AtomicBool,
    /// A memory-mode open ignored unreadable evidence: a later durable opener
    /// must fail closed rather than build on the gap.
    damaged: bool,
    failed: AtomicBool,
    gate: Arc<Semaphore>,
    #[cfg(test)]
    before_write: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
}

type Registry = Mutex<HashMap<PathBuf, Weak<Store>>>;
static STORES: OnceLock<Registry> = OnceLock::new();
// Serialize first enrollment so concurrent sessions cannot all pass the last
// retained-store slot. Existing receipts do not take this global gate.
static ENROLLMENTS: OnceLock<Mutex<()>> = OnceLock::new();

/// Shares `store` through the registry unless another opener won the race.
fn register(
    stores: &mut HashMap<PathBuf, Weak<Store>>,
    path: PathBuf,
    store: Arc<Store>,
    receipts: Receipts,
    limit: usize,
) -> Result<Arc<Store>> {
    stores.retain(|_, store| store.strong_count() > 0);
    if let Some(existing) = stores.get(&path).and_then(Weak::upgrade) {
        return existing.reuse(receipts);
    }
    if stores.len() >= limit {
        // An in-memory record need not be shared, so a full registry never
        // refuses an ordinary chat; only durable receipts are limited.
        ensure!(receipts == Receipts::Memory, "send store limit reached");
        return Ok(store);
    }
    stores.insert(path, Arc::downgrade(&store));
    Ok(store)
}

pub(crate) fn paths(dir: &Path, session_id: &str) -> Result<(PathBuf, PathBuf)> {
    ensure!(
        !session_id.is_empty()
            && session_id.len() <= 128
            && session_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "invalid send state session"
    );
    Ok((
        dir.join(format!("{session_id}.send-state.json")),
        dir.join(format!("{session_id}.send-state-required")),
    ))
}

fn read_bounded(path: &Path, limit: usize) -> Result<Option<Vec<u8>>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(nix::libc::O_NOFOLLOW);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("send state cannot be read"),
    };
    ensure!(
        file.metadata()?.is_file(),
        "send state is not a regular file"
    );
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "send state exceeds limit");
    Ok(Some(bytes))
}

/// The sidecar's record merged with the journal's echoes; any damage is an
/// error, which only a durable opener turns into a refusal.
fn load(
    path: &Path,
    required: &Path,
    session_id: &str,
    legacy: &[(String, ClientIdState)],
) -> Result<Snapshot> {
    let marker = read_bounded(required, REQUIRED.len())?;
    ensure!(
        marker.as_deref().is_none_or(|bytes| bytes == REQUIRED),
        "send state enrollment is damaged"
    );
    let bytes = read_bounded(path, MAX_BYTES)?;
    ensure!(
        marker.is_none() || bytes.is_some(),
        "enrolled send state is missing"
    );
    let mut snapshot = match bytes {
        Some(bytes) => decode(session_id, &bytes)?,
        None => Snapshot::new(session_id),
    };
    // Legacy echoes are useful evidence, but never replace a withdrawal or
    // an unresolved dispatch imported from the independent store.
    snapshot.merge_legacy(legacy)?;
    Ok(snapshot)
}

fn decode(session_id: &str, bytes: &[u8]) -> Result<Snapshot> {
    ensure!(bytes.len() <= MAX_BYTES, "send state exceeds limit");
    let snapshot: Snapshot = serde_json::from_slice(bytes).context("send state is damaged")?;
    snapshot.validate(session_id)?;
    Ok(snapshot)
}

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

impl Store {
    pub(crate) fn open(
        dir: &Path,
        session_id: &str,
        legacy: &[(String, ClientIdState)],
        receipts: Receipts,
    ) -> Result<Arc<Self>> {
        fs::create_dir_all(dir)?;
        let dir = dir.canonicalize()?;
        let (path, required) = paths(&dir, session_id)?;
        let registry = STORES.get_or_init(|| Mutex::new(HashMap::new()));
        let cached = registry
            .lock()
            .expect("send stores lock")
            .get(&path)
            .and_then(Weak::upgrade);
        if let Some(store) = cached {
            return store.reuse(receipts);
        }
        let (snapshot, damaged) = match (load(&path, &required, session_id, legacy), receipts) {
            (Ok(snapshot), _) => (snapshot, false),
            (Err(error), Receipts::Durable) => return Err(error),
            (Err(error), Receipts::Memory) => {
                tracing::warn!(
                    session = session_id,
                    "ignoring unreadable send receipts: {error:#}"
                );
                let mut snapshot = Snapshot::new(session_id);
                if let Err(error) = snapshot.merge_legacy(legacy) {
                    tracing::warn!(
                        session = session_id,
                        "ignoring conflicting journal send evidence: {error:#}"
                    );
                    snapshot = Snapshot::new(session_id);
                }
                (snapshot, true)
            }
        };
        let store = Arc::new(Self {
            path: path.clone(),
            required,
            snapshot: Mutex::new(snapshot),
            durable: AtomicBool::new(receipts == Receipts::Durable),
            damaged,
            failed: AtomicBool::new(false),
            gate: Arc::new(Semaphore::new(1)),
            #[cfg(test)]
            before_write: Mutex::new(None),
        });
        let mut stores = registry.lock().expect("send stores lock");
        register(&mut stores, path, store, receipts, MAX_STORES)
    }

    /// A cached store serves every opener. A durable opener upgrades it: its
    /// next write persists the whole record, so nothing kept in memory is lost.
    fn reuse(self: Arc<Self>, receipts: Receipts) -> Result<Arc<Self>> {
        if receipts == Receipts::Durable {
            ensure!(!self.damaged, "send state is damaged");
            self.durable.store(true, Ordering::Release);
        }
        Ok(self)
    }

    pub(crate) fn state(&self, id: &str) -> Result<Option<ClientIdState>> {
        ensure!(
            !self.failed.load(Ordering::Acquire),
            "send evidence is unavailable"
        );
        Ok(self
            .snapshot
            .lock()
            .expect("send state lock")
            .get(id)
            .map(|state| match state {
                State::Dispatching => ClientIdState::Uncertain,
                State::Confirmed => ClientIdState::Confirmed,
                State::Withdrawn => ClientIdState::Cancelled,
            }))
    }

    fn update(&self, id: &str, state: State, withdraw_queued: bool, persist: bool) -> Result<()> {
        ensure!(
            !self.failed.load(Ordering::Acquire),
            "send evidence is unavailable"
        );
        let mut next = self.snapshot.lock().expect("send state lock").clone();
        if withdraw_queued {
            match next.get(id) {
                Some(State::Dispatching) => next.entries.retain(|entry| entry.id != id),
                Some(State::Confirmed | State::Withdrawn) => return Ok(()),
                None => anyhow::bail!("queued send evidence is missing"),
            }
        }
        next.remember(id, state)?;
        if !persist {
            *self.snapshot.lock().expect("send state lock") = next;
            return Ok(());
        }
        self.install(next)
    }

    fn install(&self, next: Snapshot) -> Result<()> {
        let bytes = serde_json::to_vec(&next)?;
        ensure!(bytes.len() <= MAX_BYTES, "send state exceeds limit");
        let enrollment = if !self.required.try_exists()? {
            Some(
                ENROLLMENTS
                    .get_or_init(|| Mutex::new(()))
                    .lock()
                    .expect("send enrollment lock"),
            )
        } else {
            None
        };
        if enrollment.is_some() {
            let mut entries = 0;
            let mut enrolled = 0;
            for entry in fs::read_dir(self.path.parent().context("send state needs a parent")?)? {
                let entry = entry?;
                entries += 1;
                ensure!(
                    entries <= MAX_STORES * 4 + 8,
                    "send state directory exceeds limit"
                );
                enrolled += usize::from(
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.ends_with(".send-state-required")),
                );
            }
            ensure!(enrolled < MAX_STORES, "retained send store limit reached");
        }
        if let Err(error) =
            atomic_write(&self.path, &bytes).and_then(|()| atomic_write(&self.required, REQUIRED))
        {
            // Rename may have happened already. Never treat a failed operation
            // as proof that no durable dispatch/withdrawal exists.
            self.failed.store(true, Ordering::Release);
            return Err(error);
        }
        *self.snapshot.lock().expect("send state lock") = next;
        Ok(())
    }

    async fn record(self: &Arc<Self>, id: &str, state: State, withdraw_queued: bool) -> Result<()> {
        ensure!(
            !self.failed.load(Ordering::Acquire),
            "send evidence is unavailable"
        );
        // The owned permit survives cancellation and timeout. Only one blocking
        // write per session can exist; a hung mount cannot create more jobs.
        let permit = match tokio::time::timeout(IO_WAIT, self.gate.clone().acquire_owned()).await {
            Ok(permit) => permit?,
            Err(_) => {
                // A canceled caller may have left a hung owned write. Latch
                // the failure so subsequent provider events do not each wait.
                if self.durable.load(Ordering::Acquire) {
                    self.failed.store(true, Ordering::Release);
                }
                anyhow::bail!("send state storage is busy");
            }
        };
        if !self.durable.load(Ordering::Acquire) {
            // In memory: nothing to wait for. The gate still orders this
            // update against a write begun after a durable opener upgraded us.
            let _permit = permit;
            return self.update(id, state, withdraw_queued, false);
        }
        let owner = self.clone();
        let id = id.to_owned();
        let write = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            #[cfg(test)]
            if let Some((entered, resume)) = owner.before_write.lock().unwrap().take() {
                let _ = entered.send(());
                let _ = resume.recv();
            }
            owner.update(&id, state, withdraw_queued, true)
        });
        match tokio::time::timeout(IO_WAIT, write).await {
            Ok(result) => result.context("send state writer stopped")?,
            Err(_) => {
                // The write may still commit; prevent further keyed admission
                // until a future daemon life reads its final durable outcome.
                self.failed.store(true, Ordering::Release);
                Err(anyhow::anyhow!("send state storage did not answer"))
            }
        }
    }

    pub(crate) async fn dispatch(self: &Arc<Self>, id: &str) -> Result<()> {
        self.record(id, State::Dispatching, false).await
    }
    pub(crate) async fn withdraw(self: &Arc<Self>, id: &str) -> Result<()> {
        self.record(id, State::Withdrawn, false).await
    }
    pub(crate) async fn cancel_queued(self: &Arc<Self>, id: &str) -> Result<()> {
        self.record(id, State::Withdrawn, true).await
    }
    pub(crate) async fn confirm(self: &Arc<Self>, id: &str) -> Result<()> {
        self.record(id, State::Confirmed, false).await
    }
}

fn full_sync(file: &File) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: a live file descriptor; F_FULLFSYNC has no third argument.
        if unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_FULLFSYNC) } == 0 {
            return Ok(());
        }
    }
    file.sync_all()?;
    Ok(())
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("send state needs a parent")?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".send-state-{:016x}.tmp", rand::random::<u64>()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&tmp)?;
    let result = (|| {
        file.write_all(bytes)?;
        full_sync(&file)?;
        fs::rename(&tmp, path)?;
        let directory = File::open(parent)?;
        match full_sync(&directory) {
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .and_then(std::io::Error::raw_os_error)
                    .is_some_and(|code| {
                        code == nix::libc::EINVAL
                            || code == nix::libc::ENOTSUP
                            || code == nix::libc::EOPNOTSUPP
                    }) =>
            {
                Ok(())
            }
            result => result,
        }
    })();
    let _ = fs::remove_file(tmp);
    result
}

pub(crate) fn validate(session_id: &str, bytes: &[u8]) -> Result<()> {
    paths(Path::new("."), session_id)?;
    decode(session_id, bytes).map(|_| ())
}

pub(crate) fn export(
    dir: &Path,
    session_id: &str,
    legacy: &[(String, ClientIdState)],
) -> Result<Vec<u8>> {
    let store = Store::open(dir, session_id, legacy, Receipts::Durable)?;
    let _permit = store
        .gate
        .clone()
        .try_acquire_owned()
        .context("send state storage is busy")?;
    ensure!(
        !store.failed.load(Ordering::Acquire),
        "send evidence is unavailable"
    );
    let mut snapshot = store.snapshot.lock().expect("send state lock").clone();
    snapshot.merge_legacy(legacy)?;
    let bytes = serde_json::to_vec(&snapshot)?;
    ensure!(bytes.len() <= MAX_BYTES, "send state exceeds limit");
    Ok(bytes)
}

/// Read-only preparation: preserve local receipts before a transaction replaces
/// its journal. The final installer merges this result again before committing.
pub(crate) fn merge(
    dir: &Path,
    session_id: &str,
    bytes: &[u8],
    legacy: &[(String, ClientIdState)],
) -> Result<Vec<u8>> {
    let incoming = decode(session_id, bytes)?;
    let merged = if dir.exists() {
        let store = Store::open(dir, session_id, legacy, Receipts::Durable)?;
        let _permit = store
            .gate
            .clone()
            .try_acquire_owned()
            .context("send state storage is busy")?;
        ensure!(
            !store.failed.load(Ordering::Acquire),
            "send evidence is unavailable"
        );
        let mut merged = store.snapshot.lock().expect("send state lock").clone();
        merged.merge_legacy(legacy)?;
        for entry in &incoming.entries {
            merged.remember(&entry.id, entry.state)?;
        }
        merged
    } else {
        let mut merged = Snapshot::new(session_id);
        merged.merge_legacy(legacy)?;
        for entry in incoming.entries {
            merged.remember(&entry.id, entry.state)?;
        }
        merged
    };
    merged.validate(session_id)?;
    let bytes = serde_json::to_vec(&merged)?;
    ensure!(bytes.len() <= MAX_BYTES, "send state exceeds limit");
    Ok(bytes)
}

pub(crate) fn import(
    dir: &Path,
    session_id: &str,
    bytes: &[u8],
    legacy: &[(String, ClientIdState)],
) -> Result<()> {
    let incoming = decode(session_id, bytes)?;
    let store = Store::open(dir, session_id, legacy, Receipts::Durable)?;
    // Imports happen only for quiescent sessions. Fail instead of racing an old
    // owned writer, including one whose caller timed out or was canceled.
    let _permit = store
        .gate
        .clone()
        .try_acquire_owned()
        .context("send state storage is busy")?;
    ensure!(
        !store.failed.load(Ordering::Acquire),
        "send evidence is unavailable"
    );
    let mut merged = store.snapshot.lock().expect("send state lock").clone();
    merged.merge_legacy(legacy)?;
    for entry in incoming.entries {
        merged.remember(&entry.id, entry.state)?;
    }
    store.install(merged)
}

/// A retired journal may be pruned, but unresolved or damaged independent
/// evidence remains until explicit recovery. It must never become a fresh send.
pub(crate) fn can_prune_evidence(dir: &Path, session_id: &str) -> Result<bool> {
    let (path, required) = paths(dir, session_id)?;
    let marker = read_bounded(&required, REQUIRED.len())?;
    ensure!(
        marker.as_deref().is_none_or(|bytes| bytes == REQUIRED),
        "send state enrollment is damaged"
    );
    let bytes = read_bounded(&path, MAX_BYTES)?;
    ensure!(
        marker.is_none() || bytes.is_some(),
        "enrolled send state is missing"
    );
    Ok(match bytes {
        Some(bytes) => !decode(session_id, &bytes)?
            .entries
            .iter()
            .any(|entry| entry.state == State::Dispatching),
        None => true,
    })
}

pub(crate) fn uncertain_error() -> anyhow::Error {
    SendUncertain.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A receipt writer can yield after the agent has consumed the input.
    /// Retries in that interval still belong to the original reservation.
    #[tokio::test]
    async fn retry_during_receipt_write_keeps_original_send_admission() {
        use crate::driver::SpawnSpec;
        use crate::model::{AgentCommand, AgentEvent, ContentBlock, UserMessageState};
        use crate::{ChatManager, SendOutcome};
        for queued in [false, true] {
            for cancelled in [false, true] {
                if cancelled && !queued {
                    continue;
                }
                let dir = tempfile::tempdir().unwrap();
                let manager = Arc::new(ChatManager::new(
                    dir.path().join("chat"),
                    Box::new(|_, _| {}),
                    Box::new(|_, _| {}),
                ));
                let commands = Arc::new(Mutex::new(None));
                let mut spec =
                    SpawnSpec::new("receipt", vec!["fixture".into()], dir.path().to_path_buf());
                spec.managed_execution = true;
                manager
                    .spawn(
                        &crate::tests::HeldCommands {
                            commands: commands.clone(),
                        },
                        spec,
                    )
                    .unwrap();
                let mut commands = commands.lock().unwrap().take().unwrap();
                let session = manager.get_session("receipt").unwrap();
                let send = || AgentCommand::Send {
                    blocks: vec![ContentBlock::Text {
                        text: "once".into(),
                    }],
                };
                assert_eq!(
                    manager
                        .send_from_client("receipt", send(), Some("client-receipt-race"))
                        .await
                        .unwrap(),
                    SendOutcome::Accepted
                );
                assert!(commands.try_recv().is_ok());
                let echo = AgentEvent::UserMessage {
                    text: "once".into(),
                    attachments: 0,
                    attachment_paths: Vec::new(),
                    id: Some("delivery".into()),
                    queued,
                    after_turn: false,
                    origin: None,
                    client_id: None,
                };
                let event = if queued {
                    manager.absorb("receipt", &session, echo).await;
                    AgentEvent::UserMessageUpdate {
                        id: "delivery".into(),
                        state: if cancelled {
                            UserMessageState::Cancelled
                        } else {
                            UserMessageState::Sent
                        },
                    }
                } else {
                    echo
                };
                let (entered, entrance) = std::sync::mpsc::channel();
                let (resume, paused) = std::sync::mpsc::channel();
                *session.send_state.before_write.lock().unwrap() = Some((entered, paused));
                let owner = manager.clone();
                let captured = session.clone();
                let settlement =
                    tokio::spawn(async move { owner.absorb("receipt", &captured, event).await });
                tokio::task::spawn_blocking(move || {
                    entrance.recv_timeout(Duration::from_secs(1)).unwrap()
                })
                .await
                .unwrap();
                // Release before asserting, so a failed assertion cannot leave
                // the original blocking writer parked at its test barrier.
                let observed = manager.client_id_state("receipt", "client-receipt-race");
                let retry = manager
                    .send_from_client("receipt", send(), Some("client-receipt-race"))
                    .await;
                let cancel = manager.cancel_send("receipt", "client-receipt-race").await;
                resume.send(()).unwrap();
                settlement.await.unwrap();
                assert_eq!(observed, Some(ClientIdState::Accepted));
                assert_eq!(retry.unwrap(), SendOutcome::Duplicate);
                assert!(!cancel.unwrap());
                assert!(
                    commands.try_recv().is_err(),
                    "retry must not reach the driver"
                );
                assert_eq!(
                    manager.client_id_state("receipt", "client-receipt-race"),
                    Some(if cancelled {
                        ClientIdState::Cancelled
                    } else {
                        ClientIdState::Confirmed
                    })
                );
                assert_eq!(session.command_budget.lock().unwrap().sends, 0);
                manager.kill("receipt");
            }
        }
    }

    /// An ordinary (not managed) chat keeps its send record in memory: no
    /// sidecar, no synced write per send, and a slow disk cannot latch it.
    #[tokio::test]
    async fn an_ordinary_chat_keeps_its_send_record_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "s-memory", &[], Receipts::Memory).unwrap();
        store.dispatch("client-delivered").await.unwrap();
        store.confirm("client-delivered").await.unwrap();
        store.dispatch("client-pending").await.unwrap();
        store.withdraw("client-withdrawn").await.unwrap();
        assert_eq!(
            store.state("client-delivered").unwrap(),
            Some(ClientIdState::Confirmed)
        );
        assert_eq!(
            store.state("client-pending").unwrap(),
            Some(ClientIdState::Uncertain)
        );
        assert_eq!(
            store.state("client-withdrawn").unwrap(),
            Some(ClientIdState::Cancelled)
        );
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            0,
            "an ordinary send writes nothing to disk"
        );
        // A durable opener (a transfer) upgrades the cached store and its
        // next write persists everything kept in memory so far.
        let upgraded = Store::open(dir.path(), "s-memory", &[], Receipts::Durable).unwrap();
        assert!(Arc::ptr_eq(&store, &upgraded));
        upgraded.dispatch("client-after-upgrade").await.unwrap();
        drop((store, upgraded));
        let reopened = Store::open(dir.path(), "s-memory", &[], Receipts::Durable).unwrap();
        assert_eq!(
            reopened.state("client-delivered").unwrap(),
            Some(ClientIdState::Confirmed)
        );
        assert_eq!(
            reopened.state("client-after-upgrade").unwrap(),
            Some(ClientIdState::Uncertain)
        );
    }

    /// A full registry refuses only durable receipts: an ordinary chat still
    /// opens, with an unshared in-memory record.
    #[test]
    fn a_full_registry_never_refuses_an_ordinary_chat() {
        let dir = tempfile::tempdir().unwrap();
        let held = Store::open(dir.path(), "s-held", &[], Receipts::Memory).unwrap();
        let mut stores = HashMap::from([(held.path.clone(), Arc::downgrade(&held))]);
        let fresh = |id: &str| {
            let other = tempfile::tempdir().unwrap();
            let store = Store::open(other.path(), id, &[], Receipts::Memory).unwrap();
            (store.path.clone(), store, other)
        };
        let (path, store, _keep) = fresh("s-memory-full");
        let opened = register(
            &mut stores,
            path.clone(),
            store.clone(),
            Receipts::Memory,
            1,
        )
        .expect("a memory store opens past the limit");
        assert!(Arc::ptr_eq(&opened, &store));
        assert!(
            !stores.contains_key(&path),
            "an overflow store is not shared"
        );
        let (path, store, _keep) = fresh("s-durable-full");
        assert!(register(&mut stores, path, store, Receipts::Durable, 1).is_err());
        assert_eq!(stores.len(), 1);
    }

    /// Damaged evidence never stops an ordinary chat from starting; it is
    /// ignored with a log line and left on disk untouched. A durable opener
    /// still refuses it, cached or not.
    #[test]
    fn an_ordinary_chat_starts_over_damaged_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let (path, marker) = paths(dir.path(), "s-ignored").unwrap();
        for (state, required) in [
            (Some(b"{".to_vec()), None),
            (Some(vec![b'x'; MAX_BYTES + 1]), None),
            (None, Some(REQUIRED.to_vec())),
            (Some(b"{}".to_vec()), Some(b"0\n".to_vec())),
        ] {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_file(&marker);
            if let Some(bytes) = &state {
                fs::write(&path, bytes).unwrap();
            }
            if let Some(bytes) = &required {
                fs::write(&marker, bytes).unwrap();
            }
            assert!(Store::open(dir.path(), "s-ignored", &[], Receipts::Durable).is_err());
            let legacy = [("client-echoed".to_string(), ClientIdState::Confirmed)];
            let store = Store::open(dir.path(), "s-ignored", &legacy, Receipts::Memory).unwrap();
            assert_eq!(
                store.state("client-echoed").unwrap(),
                Some(ClientIdState::Confirmed),
                "journal evidence still counts"
            );
            assert!(Store::open(dir.path(), "s-ignored", &[], Receipts::Durable).is_err());
            assert_eq!(fs::read(&path).ok(), state);
            assert_eq!(fs::read(&marker).ok(), required);
        }
    }

    /// The receipt mode follows the session's execution: a damaged sidecar
    /// blocks only a managed spawn, and an ordinary send writes no file.
    #[tokio::test]
    async fn only_managed_chats_write_or_require_durable_receipts() {
        use crate::driver::SpawnSpec;
        use crate::model::{AgentCommand, ContentBlock};
        use crate::ChatManager;
        let dir = tempfile::tempdir().unwrap();
        let chat = dir.path().join("chat");
        let manager = Arc::new(ChatManager::new(
            chat.clone(),
            Box::new(|_, _| {}),
            Box::new(|_, _| {}),
        ));
        let spawn = |id: &str, managed: bool| {
            let mut spec = SpawnSpec::new(id, vec!["fixture".into()], dir.path().to_path_buf());
            spec.managed_execution = managed;
            let commands = Arc::new(Mutex::new(None));
            manager
                .spawn(
                    &crate::tests::HeldCommands {
                        commands: commands.clone(),
                    },
                    spec,
                )
                .map(|_| commands)
        };
        fs::create_dir_all(&chat).unwrap();
        fs::write(chat.join("s-damaged.send-state.json"), b"{").unwrap();
        assert!(spawn("s-damaged", true).is_err());
        spawn("s-damaged", false).unwrap();
        manager.kill("s-damaged");

        // Hold the driver's command receiver so the send is delivered.
        let _held = spawn("s-ordinary", false).unwrap();
        let send = AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "hi".into() }],
        };
        manager
            .send_from_client("s-ordinary", send, Some("client-ordinary"))
            .await
            .unwrap();
        assert!(!chat.join("s-ordinary.send-state.json").exists());
        assert!(!chat.join("s-ordinary.send-state-required").exists());
        manager.kill("s-ordinary");
    }

    #[tokio::test]
    async fn withdrawals_and_unreceipted_dispatch_survive_store_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "s-evidence", &[], Receipts::Durable).unwrap();
        store.withdraw("client-withdrawn").await.unwrap();
        store.dispatch("client-unknown").await.unwrap();
        drop(store);
        let restarted = Store::open(dir.path(), "s-evidence", &[], Receipts::Durable).unwrap();
        assert_eq!(
            restarted.state("client-withdrawn").unwrap(),
            Some(ClientIdState::Cancelled)
        );
        assert_eq!(
            restarted.state("client-unknown").unwrap(),
            Some(ClientIdState::Uncertain)
        );
        let bytes = export(dir.path(), "s-evidence", &[]).unwrap();
        let destination = tempfile::tempdir().unwrap();
        import(destination.path(), "s-evidence", &bytes, &[]).unwrap();
        let restored =
            Store::open(destination.path(), "s-evidence", &[], Receipts::Durable).unwrap();
        assert_eq!(
            restored.state("client-unknown").unwrap(),
            Some(ClientIdState::Uncertain)
        );
        assert_eq!(
            restored.state("client-withdrawn").unwrap(),
            Some(ClientIdState::Cancelled)
        );
        assert!(validate("s-another", &bytes).is_err());
    }

    #[tokio::test]
    async fn outstanding_ids_never_roll_over_with_the_settled_record() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "s-limits", &[], Receipts::Durable).unwrap();
        for n in 0..RETAINED_SENDS_MAX {
            store.dispatch(&format!("pending-{n:03}")).await.unwrap();
        }
        for n in 0..model::CLIENT_IDS_REMEMBERED + 5 {
            store.withdraw(&format!("settled-{n:03}")).await.unwrap();
        }
        assert!(store.dispatch("pending-overflow").await.is_err());
        assert!(
            !store.failed.load(Ordering::Acquire),
            "capacity refusal is not storage damage"
        );
        assert_eq!(store.state("settled-000").unwrap(), None);
        for n in 0..RETAINED_SENDS_MAX {
            assert_eq!(
                store.state(&format!("pending-{n:03}")).unwrap(),
                Some(ClientIdState::Uncertain)
            );
        }
        let bytes = export(dir.path(), "s-limits", &[]).unwrap();
        assert!(bytes.len() <= MAX_BYTES);
        validate("s-limits", &bytes).unwrap();
    }

    #[tokio::test]
    async fn imports_and_delayed_writes_cannot_weaken_receipts() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "s-merge", &[], Receipts::Durable).unwrap();
        store.dispatch("client-confirmed").await.unwrap();
        let older = export(dir.path(), "s-merge", &[]).unwrap();
        store.confirm("client-confirmed").await.unwrap();
        import(dir.path(), "s-merge", &older, &[]).unwrap();
        store.dispatch("client-confirmed").await.unwrap();
        assert_eq!(
            store.state("client-confirmed").unwrap(),
            Some(ClientIdState::Confirmed)
        );
        store.withdraw("client-withdrawn").await.unwrap();
        let before = fs::read(&store.path).unwrap();
        let mut conflicting = Snapshot::new("s-merge");
        conflicting
            .remember("client-withdrawn", State::Confirmed)
            .unwrap();
        assert!(import(
            dir.path(),
            "s-merge",
            &serde_json::to_vec(&conflicting).unwrap(),
            &[]
        )
        .is_err());
        assert_eq!(fs::read(&store.path).unwrap(), before);
    }

    #[test]
    fn corrupted_oversized_and_missing_enrolled_evidence_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let (path, marker) = paths(dir.path(), "s-damaged").unwrap();
        for bytes in [b"{".to_vec(), vec![b'x'; MAX_BYTES + 1]] {
            fs::write(&path, &bytes).unwrap();
            assert!(Store::open(dir.path(), "s-damaged", &[], Receipts::Durable).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        fs::remove_file(&path).unwrap();
        fs::write(&marker, REQUIRED).unwrap();
        assert!(Store::open(dir.path(), "s-damaged", &[], Receipts::Durable).is_err());
        assert!(!path.exists());
        fs::write(&marker, b"0\n").unwrap();
        assert!(Store::open(dir.path(), "s-damaged", &[], Receipts::Durable).is_err());
        assert_eq!(fs::read(marker).unwrap(), b"0\n");
    }

    #[tokio::test]
    async fn a_failed_confirmation_retains_dispatch_and_blocks_admission() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "s-failed", &[], Receipts::Durable).unwrap();
        store.dispatch("client-unknown").await.unwrap();
        // Directory at the marker destination makes the post-rename operation
        // fail. The state write may already have committed: do not clear it.
        fs::remove_file(&store.required).unwrap();
        fs::create_dir(&store.required).unwrap();
        assert!(store.confirm("client-unknown").await.is_err());
        assert!(store.state("client-new-id").is_err());
        assert!(store.dispatch("client-new-id").await.is_err());
        assert!(export(dir.path(), "s-failed", &[]).is_err());
        assert!(fs::read(&store.path)
            .unwrap()
            .windows(14)
            .any(|bytes| bytes == b"client-unknown"));
    }

    #[tokio::test]
    async fn canceling_before_the_io_permit_leaves_no_dispatch_and_allows_retry() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "s-cancel-io", &[], Receipts::Durable).unwrap();
        let hold = store.gate.clone().acquire_owned().await.unwrap();
        let owner = store.clone();
        let task = tokio::spawn(async move { owner.dispatch("client-not-sent").await });
        tokio::task::yield_now().await;
        task.abort();
        let _ = task.await;
        assert_eq!(store.state("client-not-sent").unwrap(), None);
        assert!(!store.path.exists());
        drop(hold);
        store.dispatch("client-not-sent").await.unwrap();
        assert_eq!(
            store.state("client-not-sent").unwrap(),
            Some(ClientIdState::Uncertain)
        );
    }

    #[tokio::test]
    async fn historical_enrollment_cap_refuses_new_stores_without_erasing_old_evidence() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..MAX_STORES {
            fs::write(
                dir.path().join(format!("retired-{n}.send-state-required")),
                REQUIRED,
            )
            .unwrap();
        }
        let store = Store::open(dir.path(), "s-over-cap", &[], Receipts::Durable).unwrap();
        assert!(store.dispatch("client-over-cap").await.is_err());
        assert!(!store.failed.load(Ordering::Acquire));
        assert_eq!(store.state("client-over-cap").unwrap(), None);
        assert!(!store.path.exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), MAX_STORES);
    }

    #[tokio::test]
    async fn canceled_writer_keeps_its_gate_and_survives_process_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "s-owned-write", &[], Receipts::Durable).unwrap();
        let (entered, entrance) = std::sync::mpsc::channel();
        let (resume, paused) = std::sync::mpsc::channel();
        *store.before_write.lock().unwrap() = Some((entered, paused));
        let owner = store.clone();
        let task = tokio::spawn(async move { owner.dispatch("client-canceled-await").await });
        tokio::task::spawn_blocking(move || entrance.recv_timeout(Duration::from_secs(1)).unwrap())
            .await
            .unwrap();
        task.abort();
        let _ = task.await;
        let replacement = Store::open(dir.path(), "s-owned-write", &[], Receipts::Durable).unwrap();
        assert!(Arc::ptr_eq(&store, &replacement));
        assert!(export(dir.path(), "s-owned-write", &[]).is_err());
        let empty = serde_json::to_vec(&Snapshot::new("s-owned-write")).unwrap();
        assert!(import(dir.path(), "s-owned-write", &empty, &[]).is_err());
        resume.send(()).unwrap();
        let gate = tokio::time::timeout(IO_WAIT, replacement.gate.clone().acquire_owned())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            replacement.state("client-canceled-await").unwrap(),
            Some(ClientIdState::Uncertain)
        );
        drop(gate);
        import(dir.path(), "s-owned-write", &empty, &[]).unwrap();
        assert_eq!(
            replacement.state("client-canceled-await").unwrap(),
            Some(ClientIdState::Uncertain)
        );
    }

    #[tokio::test]
    async fn timed_out_confirmation_keeps_dispatch_unknown_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "s-slow-confirm", &[], Receipts::Durable).unwrap();
        store.dispatch("client-slow-confirm").await.unwrap();
        let (entered, entrance) = std::sync::mpsc::channel();
        let (resume, paused) = std::sync::mpsc::channel();
        *store.before_write.lock().unwrap() = Some((entered, paused));
        let owner = store.clone();
        let task = tokio::spawn(async move { owner.confirm("client-slow-confirm").await });
        tokio::task::spawn_blocking(move || entrance.recv_timeout(Duration::from_secs(1)).unwrap())
            .await
            .unwrap();
        assert!(task.await.unwrap().is_err());
        assert!(store.state("client-slow-confirm").is_err());
        let start = std::time::Instant::now();
        assert!(store.confirm("client-slow-confirm").await.is_err());
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "failed storage must not stall later events"
        );
        resume.send(()).unwrap();
        let gate = tokio::time::timeout(IO_WAIT, store.gate.clone().acquire_owned())
            .await
            .unwrap()
            .unwrap();
        drop(gate);
        tokio::time::timeout(IO_WAIT, async {
            while Arc::strong_count(&store) > 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(store);
        let restarted = Store::open(dir.path(), "s-slow-confirm", &[], Receipts::Durable).unwrap();
        assert_eq!(
            restarted.state("client-slow-confirm").unwrap(),
            Some(ClientIdState::Uncertain)
        );
    }

    #[test]
    fn read_only_preparation_captures_new_legacy_receipts_even_with_a_cached_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "s-hot-legacy", &[], Receipts::Durable).unwrap();
        let incoming = serde_json::to_vec(&Snapshot::new("s-hot-legacy")).unwrap();
        let bytes = merge(
            dir.path(),
            "s-hot-legacy",
            &incoming,
            &[("client-legacy".into(), ClientIdState::Confirmed)],
        )
        .unwrap();
        assert_eq!(
            decode("s-hot-legacy", &bytes).unwrap().get("client-legacy"),
            Some(State::Confirmed)
        );
        assert_eq!(
            store.state("client-legacy").unwrap(),
            None,
            "preparation is read-only"
        );
        assert!(!store.path.exists());
        import(dir.path(), "s-hot-legacy", &bytes, &[]).unwrap();
        assert_eq!(
            store.state("client-legacy").unwrap(),
            Some(ClientIdState::Confirmed)
        );
    }
}
