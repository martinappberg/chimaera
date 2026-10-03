//! A failed public import is recoverable work, never executable history.
//! Load its bounded fence before ledger restoration; hot admission does no I/O.
use super::*;
use std::sync::Mutex;

const MAX_PENDING: usize = 64;
const MAX_PENDING_BYTES: u64 = 256 * 1024;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pending {
    pub binding: crate::pro::install::Binding,
    pub id: String,
    pub destination: PathBuf,
    pub native: Option<String>,
    pub fork: bool,
    pub origin: Origin,
    pub defer_start: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    version: u32,
    entries: BTreeMap<String, Pending>,
}
struct State {
    store: Store,
    invalid: bool,
    readers: BTreeMap<u64, (String, String, Option<String>)>,
    next_reader: u64,
}
pub(crate) struct SessionAdmission<'a> {
    _state: std::sync::MutexGuard<'a, State>,
}
pub(crate) struct ReadAdmission<'a> {
    store: &'a PendingImports,
    token: u64,
}
impl Drop for ReadAdmission<'_> {
    fn drop(&mut self) {
        crate::lock(&self.store.state).readers.remove(&self.token);
    }
}
pub(crate) struct PendingImports {
    path: PathBuf,
    state: Mutex<State>,
    writer: Mutex<()>,
}
#[derive(Debug)]
pub(super) struct PendingError;
impl std::fmt::Display for PendingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Session import needs recovery. Retry the same archive and import options; retained originals must not be deleted.")
    }
}
impl std::error::Error for PendingError {}

fn valid(entry: &Pending) -> bool {
    valid_id(&entry.id)
        && valid_id(&entry.binding.workspace)
        && entry.binding.endpoint.len() <= 2048
        && entry.binding.account.as_deref().is_none_or(valid_id)
        && entry.binding.receipt.as_ref().is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
        })
        && entry.destination.is_absolute()
        && entry.destination.as_os_str().len() <= 4096
        && !entry
            .destination
            .components()
            .any(|p| matches!(p, std::path::Component::ParentDir))
        && entry
            .native
            .as_deref()
            .is_none_or(crate::codex_notify::valid_thread_id)
}
impl PendingImports {
    pub(crate) fn load(data: &Path) -> Self {
        let data = std::fs::canonicalize(data).unwrap_or_else(|_| data.to_owned());
        let path = data.join("bundles/pending.json");
        let read = (|| -> Result<Option<Store>> {
            let parent = path.parent().unwrap();
            match std::fs::symlink_metadata(parent) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.into()),
                Ok(metadata) if !metadata.is_dir() => bail!("invalid import recovery directory"),
                Ok(_) => {}
            }
            let directory = crate::pro::install::directory(parent)?;
            let file = match rustix::fs::openat(
                &directory,
                "pending.json",
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::NONBLOCK
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            ) {
                Ok(file) => File::from(file),
                Err(rustix::io::Errno::NOENT) => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.len() > MAX_PENDING_BYTES {
                bail!("invalid import recovery record");
            }
            let mut bytes = Vec::new();
            file.take(MAX_PENDING_BYTES + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_PENDING_BYTES {
                bail!("import recovery record grew beyond limit");
            }
            let store: Store = serde_json::from_slice(&bytes)?;
            if store.version != 1
                || store.entries.len() > MAX_PENDING
                || store
                    .entries
                    .iter()
                    .any(|(id, entry)| id != &entry.id || !valid(entry))
            {
                bail!("invalid import recovery binding");
            }
            Ok(Some(store))
        })();
        let (store, invalid) = match read {
            Ok(Some(store)) => (store, false),
            Ok(None) => (
                Store {
                    version: 1,
                    entries: BTreeMap::new(),
                },
                false,
            ),
            Err(_) => (
                Store {
                    version: 1,
                    entries: BTreeMap::new(),
                },
                true,
            ),
        };
        Self {
            path,
            writer: Mutex::new(()),
            state: Mutex::new(State {
                store,
                invalid,
                readers: BTreeMap::new(),
                next_reader: 0,
            }),
        }
    }
    pub(crate) fn blocks_workspace(&self, workspace: &str) -> bool {
        let state = crate::lock(&self.state);
        state.invalid
            || state
                .store
                .entries
                .values()
                .any(|e| e.binding.workspace == workspace)
    }
    pub(crate) fn check_session(&self, id: &str, native: Option<&str>) -> Result<()> {
        let state = crate::lock(&self.state);
        if state.invalid
            || state.store.entries.contains_key(id)
            || native.is_some_and(|native| {
                state
                    .store
                    .entries
                    .values()
                    .any(|e| e.native.as_deref() == Some(native))
            })
        {
            return Err(PendingError.into());
        }
        Ok(())
    }
    /// The no-await spawn holds this short reservation until registration,
    /// making pending publication atomic against an already admitted launch.
    pub(crate) fn admit(
        &self,
        workspace: &str,
        id: &str,
        native: Option<&str>,
    ) -> Result<SessionAdmission<'_>> {
        let state = crate::lock(&self.state);
        if state.invalid
            || state.store.entries.contains_key(id)
            || state.store.entries.values().any(|e| {
                e.binding.workspace == workspace
                    || native.is_some_and(|n| e.native.as_deref() == Some(n))
            })
        {
            return Err(PendingError.into());
        }
        Ok(SessionAdmission { _state: state })
    }
    /// A blocking exporter retains only a reader token, not the hot lock. Its
    /// cancellation/drop settles the same token; no filesystem wait blocks reads
    /// of admission state on reactor workers.
    pub(crate) fn read_admission(
        &self,
        workspace: &str,
        id: &str,
        native: Option<&str>,
    ) -> Result<ReadAdmission<'_>> {
        let mut state = crate::lock(&self.state);
        if state.invalid
            || state.store.entries.values().any(|e| {
                e.id == id
                    || e.binding.workspace == workspace
                    || native.is_some_and(|n| e.native.as_deref() == Some(n))
            })
        {
            return Err(PendingError.into());
        }
        if state.readers.len() >= 64 {
            bail!("bundle read reservation limit reached");
        }
        state.next_reader = state
            .next_reader
            .checked_add(1)
            .context("bundle read reservation limit reached")?;
        let token = state.next_reader;
        state.readers.insert(
            token,
            (workspace.into(), id.into(), native.map(str::to_owned)),
        );
        Ok(ReadAdmission { store: self, token })
    }
    pub(super) fn matching(&self, incoming: &Pending) -> Result<bool> {
        let state = crate::lock(&self.state);
        if state.invalid {
            return Err(PendingError.into());
        }
        match state.store.entries.get(&incoming.id) {
            None => Ok(false),
            Some(old) if old == incoming => Ok(true),
            Some(_) => Err(PendingError.into()),
        }
    }
    fn persist(&self, store: &Store) -> Result<()> {
        let bytes = serde_json::to_vec(store)?;
        if bytes.len() as u64 > MAX_PENDING_BYTES {
            return Err(PendingError.into());
        }
        crate::pro::install::write_state(&self.path, &bytes)
    }
    /// Called under the owned configuration reservation on the blocking lane.
    /// Even a failed write keeps its hot fence; no canonical mutation follows.
    pub(super) fn begin(&self, incoming: Pending) -> Result<()> {
        let _writer = crate::lock(&self.writer);
        let mut state = crate::lock(&self.state);
        if state.invalid || !valid(&incoming) {
            return Err(PendingError.into());
        }
        if state.readers.values().any(|(workspace, id, native)| {
            workspace == &incoming.binding.workspace
                || id == &incoming.id
                || incoming
                    .native
                    .as_ref()
                    .is_some_and(|n| native.as_ref() == Some(n))
        }) {
            bail!("bundle snapshot already in progress");
        }
        if let Some(old) = state.store.entries.get(&incoming.id) {
            if old != &incoming {
                return Err(PendingError.into());
            }
        } else if state.store.entries.len() >= MAX_PENDING {
            return Err(PendingError.into());
        }
        state.store.entries.insert(incoming.id.clone(), incoming);
        let snapshot = state.store.clone();
        drop(state);
        self.persist(&snapshot)
    }
    /// A removal is published to hot admission only after the new whole record
    /// is durable. A delayed/erroring writer cannot clear another request.
    pub(super) fn finish(&self, incoming: &Pending) -> Result<()> {
        let _writer = crate::lock(&self.writer);
        let state = crate::lock(&self.state);
        if state.invalid || state.store.entries.get(&incoming.id) != Some(incoming) {
            return Err(PendingError.into());
        }
        let mut next = state.store.clone();
        next.entries.remove(&incoming.id);
        drop(state);
        self.persist(&next)?;
        crate::lock(&self.state).store = next;
        Ok(())
    }
    pub(crate) fn view(&self, workspace: &str) -> Option<Value> {
        let state = crate::lock(&self.state);
        let ids: Vec<_> = state
            .store
            .entries
            .values()
            .filter(|e| e.binding.workspace == workspace)
            .map(|e| e.id.clone())
            .collect();
        (state.invalid || !ids.is_empty())
            .then(|| json!({"state":"recovery_needed","sessions":ids,"damaged":state.invalid}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(index: usize) -> Pending {
        Pending {
            binding: crate::pro::install::Binding {
                endpoint: String::new(),
                account: None,
                workspace: format!("w-{index}"),
                epoch: 1,
                receipt: Some("a".repeat(64)),
            },
            id: format!("s-{index}"),
            destination: PathBuf::from("/tmp"),
            native: None,
            fork: false,
            origin: Origin::Home,
            defer_start: true,
        }
    }
    #[test]
    fn cap_and_exact_retirement_survive_reload_without_forgetting_fences() {
        let data = std::env::temp_dir().join(format!(
            "chimaera-pending-cap-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&data).unwrap();
        let pending = PendingImports::load(&data);
        for index in 0..MAX_PENDING {
            pending.begin(item(index)).unwrap();
        }
        assert!(pending.begin(item(MAX_PENDING)).is_err());
        let mut wrong = item(0);
        wrong.binding.epoch = 2;
        assert!(pending.finish(&wrong).is_err());
        let reload = PendingImports::load(&data);
        assert!(reload.blocks_workspace("w-0"));
        assert!(reload.blocks_workspace("w-63"));
        assert!(!reload.blocks_workspace("ordinary-free"));
        reload.finish(&item(0)).unwrap();
        reload.begin(item(64)).unwrap();
        let reload = PendingImports::load(&data);
        assert!(!reload.blocks_workspace("w-0"));
        assert!(reload.blocks_workspace("w-64"));
        std::fs::remove_dir_all(data).unwrap();
    }
    #[test]
    fn persistence_failure_never_publishes_hot_unfenced_state() {
        let data = std::env::temp_dir().join(format!(
            "chimaera-pending-failure-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&data).unwrap();
        let pending = PendingImports::load(&data);
        std::fs::create_dir_all(data.join("bundles/pending.json")).unwrap();
        assert!(pending.begin(item(0)).is_err());
        assert!(pending.blocks_workspace("w-0"));
        assert!(pending.finish(&item(0)).is_err());
        assert!(pending.blocks_workspace("w-0"));
        assert!(PendingImports::load(&data).blocks_workspace("ordinary-free"));
        std::fs::remove_dir_all(data).unwrap();
    }
}

#[cfg(test)]
mod reader_tests {
    use super::*;
    #[test]
    fn reader_excludes_same_workspace_or_native_install_without_holding_hot_lock() {
        let data = std::env::temp_dir().join(format!(
            "chimaera-pending-reader-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&data).unwrap();
        let pending = PendingImports::load(&data);
        let native = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
        let reader = pending
            .read_admission("w-read", "s-read", Some(native))
            .unwrap();
        assert!(!pending.blocks_workspace("w-read"));
        let mut incoming = Pending {
            binding: crate::pro::install::Binding {
                endpoint: String::new(),
                account: None,
                workspace: "w-read".into(),
                epoch: 1,
                receipt: Some("a".repeat(64)),
            },
            id: "s-incoming".into(),
            destination: PathBuf::from("/tmp"),
            native: None,
            fork: false,
            origin: Origin::Home,
            defer_start: true,
        };
        assert!(pending.begin(incoming.clone()).is_err());
        assert!(!pending.blocks_workspace("w-read"));
        incoming.binding.workspace = "w-other".into();
        incoming.native = Some(native.into());
        assert!(pending.begin(incoming.clone()).is_err());
        drop(reader);
        pending.begin(incoming).unwrap();
        assert!(pending
            .read_admission("w-read", "s-another", Some(native))
            .is_err());
        assert!(pending
            .read_admission("w-other", "s-another", None)
            .is_err());
        assert!(pending
            .read_admission("ordinary-free", "ordinary", None)
            .is_ok());
        std::fs::remove_dir_all(data).unwrap();
    }
}
