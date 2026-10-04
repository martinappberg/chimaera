//! Startup-only Ready ownership. Verification retains the capability but never
//! opens execution: fixed credential/CLI consumers are a separate disabled gate.
use super::provider_startup::Pending;
use crate::{lock, AppState};
use chimaera_core::provider_runtime as wire;
use std::{
    path::Path,
    sync::{atomic::Ordering, Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::watch,
};
use zeroize::Zeroizing;

const SOCKET: &str = "/run/chimaera/providers.sock";
const BUDGET: Duration = Duration::from_secs(5);
const RETRY: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Staged,
    Checking,
    Verified,
    Closed,
}
struct Inner {
    phase: Phase,
    // Moved into the one task while Checking, returned only by exact Ready.
    payload: Option<wire::StartupPayload>,
    work: Option<Arc<Work>>,
    consumers: Vec<Arc<Work>>,
    children: Vec<Arc<Work>>,
}
pub(super) struct Work {
    pub(super) generation: u64,
    pub(super) deadline: Instant,
    pub(super) cancel: watch::Sender<bool>,
    pub(super) done: watch::Sender<bool>,
}
pub(super) struct State {
    inner: Mutex<Inner>,
    #[cfg(any(test, feature = "daemon-extension-fixture"))]
    transport: Mutex<Option<(std::path::PathBuf, Duration)>>,
}
impl State {
    pub(super) fn new(payload: wire::StartupPayload) -> Self {
        Self {
            inner: Mutex::new(Inner {
                phase: Phase::Staged,
                payload: Some(payload),
                work: None,
                consumers: Vec::new(),
                children: Vec::new(),
            }),
            #[cfg(any(test, feature = "daemon-extension-fixture"))]
            transport: Mutex::default(),
        }
    }
    fn retire(&self) {
        let mut inner = lock(&self.inner);
        inner.phase = Phase::Closed;
        inner.payload = None;
        cancel(&inner);
    }
    fn active(&self) -> usize {
        let inner = lock(&self.inner);
        inner
            .work
            .iter()
            .chain(inner.consumers.iter())
            .chain(inner.children.iter())
            .filter(|work| !*work.done.borrow())
            .count()
    }
    #[cfg(test)]
    pub(super) fn inspect_fixture(&self, inspect: impl FnOnce(&wire::StartupPayload)) {
        inspect(lock(&self.inner).payload.as_ref().unwrap());
    }
}
fn pending(state: &AppState) -> Option<Arc<Pending>> {
    lock(&state.pro.execution.provider_pending).clone()
}
pub(in crate::pro) fn active(state: &AppState) -> usize {
    pending(state).map_or(0, |pending| pending.ready.active())
}
/// Publication of Closed precedes configuration replacement or stopping IO.
/// Neither retirement nor a subsequent Configure removes the startup fence.
pub(in crate::pro) fn retire(state: &AppState) {
    if let Some(pending) = pending(state) {
        pending.ready.retire();
    }
}
/// The first Configure drains the ordinary engine too; it must preserve a
/// capability which has not yet reached its successful Configure transition.
pub(in crate::pro) fn replacing(state: &AppState) {
    if let Some(pending) = pending(state) {
        let mut inner = lock(&pending.ready.inner);
        if inner.phase != Phase::Staged {
            inner.phase = Phase::Closed;
            inner.payload = None;
            cancel(&inner);
        }
    }
}
pub(in crate::pro) async fn stop(state: &AppState) -> anyhow::Result<()> {
    retire(state);
    settle(state).await
}
pub(in crate::pro) async fn settle(state: &AppState) -> anyhow::Result<()> {
    let works = pending(state).map_or_else(Vec::new, |pending| {
        let inner = lock(&pending.ready.inner);
        inner
            .work
            .iter()
            .chain(inner.consumers.iter())
            .chain(inner.children.iter())
            .cloned()
            .collect::<Vec<_>>()
    });
    // This bounds cleanup observation only. Cancellation has already closed
    // authority. Ready keeps its original five seconds; consumers retain their
    // own bounded IO and any unresolved child cleanup until a positive receipt.
    tokio::time::timeout(Duration::from_secs(1), async {
        for work in works {
            let mut done = work.done.subscribe();
            while !*done.borrow_and_update() {
                done.changed().await.map_err(|_| ())?;
            }
        }
        Ok::<_, ()>(())
    })
    .await
    .map_err(|_| anyhow::anyhow!("provider runtime cleanup unconfirmed"))?
    .map_err(|_| anyhow::anyhow!("provider runtime cleanup unconfirmed"))
}

/// Called only at the first successful real Configure tail, not by polls or
/// same-identity credential refresh. No IO or await delays its existing ACK.
pub(in crate::pro) fn configured(state: &Arc<AppState>) {
    let Some(pending) = pending(state) else {
        return;
    };
    let generation = state.pro.generation.load(Ordering::Acquire);
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        pending.ready.retire();
        return;
    };
    let (path, budget) = (std::path::PathBuf::from(SOCKET), BUDGET);
    #[cfg(any(test, feature = "daemon-extension-fixture"))]
    let (path, budget) = lock(&pending.ready.transport)
        .clone()
        .unwrap_or((path, budget));
    let (payload, work) = {
        let mut inner = lock(&pending.ready.inner);
        if inner.phase != Phase::Staged {
            return;
        }
        if !current(state, &pending, generation) {
            inner.phase = Phase::Closed;
            inner.payload = None;
            return;
        }
        let Some(payload) = inner.payload.take() else {
            inner.phase = Phase::Closed;
            return;
        };
        let work = Arc::new(Work {
            generation,
            deadline: Instant::now() + budget,
            cancel: watch::channel(false).0,
            done: watch::channel(false).0,
        });
        inner.phase = Phase::Checking;
        inner.work = Some(work.clone());
        (payload, work)
    };
    let owner = Owner {
        state: state.clone(),
        pending,
        work,
    };
    runtime.spawn(async move {
        let token = chimaera_core::generate_token();
        let request = wire::Request {
            version: 1,
            binding: payload.binding,
            capability: payload.capability,
            request_id: format!(
                "{}-{}-{}-{}-{}",
                &token[..8],
                &token[8..12],
                &token[12..16],
                &token[16..20],
                &token[20..32]
            ),
            command: wire::Command::Ready {},
        };
        let verified = ready(&owner, &request, &path).await.is_ok();
        let mut inner = lock(&owner.pending.ready.inner);
        if inner.phase == Phase::Checking
            && inner
                .work
                .as_ref()
                .is_some_and(|work| Arc::ptr_eq(work, &owner.work))
        {
            if verified && owner.current().is_ok() {
                inner.phase = Phase::Verified;
                inner.payload = Some(wire::StartupPayload {
                    version: 1,
                    binding: request.binding,
                    capability: request.capability,
                });
            } else {
                inner.phase = Phase::Closed;
            }
        }
        drop(inner);
        // The final publisher never takes the configuration mutex: replacement
        // may hold it while waiting for this actual continuation to finish.
    });
}
pub(super) fn current(state: &AppState, pending: &Pending, generation: u64) -> bool {
    !state.stopping.load(Ordering::Acquire)
        && lock(&state.pro.execution.provider_pending)
            .as_ref()
            .is_some_and(|row| std::ptr::eq(row.as_ref(), pending))
        && state.pro.generation.load(Ordering::Acquire) == generation
        && !super::super::drain::draining(state)
        && !super::super::delegation_lapsed(state)
        && super::supervisor::matches_provider_launch(state, &pending.launch)
        && lock(&state.pro.runtime)
            .as_ref()
            .is_some_and(|config| config.execution.is_some())
        && pending.protection.current().is_ok()
}
fn cancel(inner: &Inner) {
    for work in inner
        .work
        .iter()
        .chain(inner.consumers.iter())
        .chain(inner.children.iter())
    {
        work.cancel.send_replace(true);
    }
}
/// Admission is minted only from the retained exact Ready. The original Ready
/// deadline is not a credential lifetime; each consumer has its own bounded work.
pub(super) fn admit(
    state: &Arc<AppState>,
    command: wire::Command,
    deadline: Instant,
) -> Result<(Arc<Pending>, Arc<Work>, wire::Request), wire::Error> {
    let pending = pending(state).ok_or(wire::Error::Inactive)?;
    admit_at(state, pending, None, command, deadline)
}
pub(super) fn admit_child_stream(
    state: &Arc<AppState>,
    pending: &Arc<Pending>,
    child: &Arc<Work>,
    command: wire::Command,
    deadline: Instant,
) -> Result<(Arc<Pending>, Arc<Work>, wire::Request), wire::Error> {
    admit_at(state, pending.clone(), Some(child), command, deadline)
}
fn admit_at(
    state: &Arc<AppState>,
    pending: Arc<Pending>,
    child: Option<&Arc<Work>>,
    command: wire::Command,
    deadline: Instant,
) -> Result<(Arc<Pending>, Arc<Work>, wire::Request), wire::Error> {
    let mut inner = lock(&pending.ready.inner);
    let generation = inner.work.as_ref().ok_or(wire::Error::Inactive)?.generation;
    if child.is_some_and(|child| {
        child.generation != generation
            || Instant::now() >= child.deadline
            || deadline > child.deadline
            || *child.cancel.borrow()
            || *child.done.borrow()
            || !inner.children.iter().any(|row| Arc::ptr_eq(row, child))
    }) {
        return Err(wire::Error::StateChanged);
    }
    if inner.phase != Phase::Verified
        || Instant::now() >= deadline
        || !current(state, &pending, generation)
    {
        return Err(wire::Error::StateChanged);
    }
    inner.consumers.retain(|work| !*work.done.borrow());
    if inner.consumers.len() >= wire::STREAMS_PROJECT {
        return Err(wire::Error::LimitReached);
    }
    let payload = inner.payload.as_ref().ok_or(wire::Error::StateChanged)?;
    let token = chimaera_core::generate_token();
    let request = wire::Request {
        version: 1,
        binding: payload.binding.clone(),
        capability: wire::Capability::new(payload.capability.expose().to_owned())?,
        request_id: format!(
            "{}-{}-{}-{}-{}",
            &token[..8],
            &token[8..12],
            &token[12..16],
            &token[16..20],
            &token[20..32]
        ),
        command,
    };
    request.validate()?;
    let work = Arc::new(Work {
        generation,
        deadline,
        cancel: watch::channel(false).0,
        done: watch::channel(false).0,
    });
    inner.consumers.push(work.clone());
    drop(inner);
    Ok((pending, work, request))
}
/// Retained actual child lifetime, separate from individual stream permits.
/// No request or secret is minted merely by keeping a frontend alive.
pub(super) fn admit_child(
    state: &Arc<AppState>,
    deadline: Instant,
) -> Result<(Arc<Pending>, Arc<Work>), wire::Error> {
    let pending = pending(state).ok_or(wire::Error::Inactive)?;
    let mut inner = lock(&pending.ready.inner);
    let generation = inner.work.as_ref().ok_or(wire::Error::Inactive)?.generation;
    if inner.phase != Phase::Verified
        || Instant::now() >= deadline
        || !current(state, &pending, generation)
    {
        return Err(wire::Error::StateChanged);
    }
    inner.children.retain(|work| !*work.done.borrow());
    if inner.children.len() >= 4 {
        return Err(wire::Error::LimitReached);
    }
    let work = Arc::new(Work {
        generation,
        deadline,
        cancel: watch::channel(false).0,
        done: watch::channel(false).0,
    });
    inner.children.push(work.clone());
    drop(inner);
    Ok((pending, work))
}
pub(super) fn child_current(pending: &Pending, work: &Arc<Work>) -> bool {
    let inner = lock(&pending.ready.inner);
    inner.phase == Phase::Verified && inner.children.iter().any(|row| Arc::ptr_eq(row, work))
}
pub(super) fn consumer_current(pending: &Pending, work: &Arc<Work>) -> bool {
    let inner = lock(&pending.ready.inner);
    inner.phase == Phase::Verified && inner.consumers.iter().any(|row| Arc::ptr_eq(row, work))
}
#[cfg(any(test, feature = "daemon-extension-fixture"))]
pub(super) fn socket_fixture(pending: &Pending) -> Option<std::path::PathBuf> {
    lock(&pending.ready.transport)
        .as_ref()
        .map(|(path, _)| path.clone())
}
struct Owner {
    state: Arc<AppState>,
    pending: Arc<Pending>,
    work: Arc<Work>,
}
impl Owner {
    fn current(&self) -> Result<(), wire::Error> {
        if Instant::now() >= self.work.deadline
            || *self.work.cancel.borrow()
            || !current(&self.state, &self.pending, self.work.generation)
        {
            Err(wire::Error::StateChanged)
        } else {
            Ok(())
        }
    }
    async fn wait<T>(
        &self,
        effect: impl std::future::Future<Output = T>,
    ) -> Result<T, wire::Error> {
        self.current()?;
        let mut cancelled = self.work.cancel.subscribe();
        let value = tokio::select! {
            biased;
            _ = cancelled.wait_for(|cancelled| *cancelled) => return Err(wire::Error::StateChanged),
            _ = tokio::time::sleep_until(self.work.deadline.into()) => return Err(wire::Error::Unavailable),
            value = effect => value,
        };
        self.current()?;
        Ok(value)
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        {
            let mut inner = lock(&self.pending.ready.inner);
            if inner.phase == Phase::Checking {
                inner.phase = Phase::Closed;
                inner.payload = None;
            }
        }
        // All socket futures and secret temporaries were dropped before this
        // positive receipt. A lost observer never releases activity early.
        self.work.done.send_replace(true);
        self.state.changes.notify_waiters();
    }
}
async fn ready(owner: &Owner, request: &wire::Request, path: &Path) -> Result<(), wire::Error> {
    let bytes = wire::encode_control(request)?;
    loop {
        let inactive = owner.wait(exchange(request, &bytes, path)).await??;
        if !inactive {
            return Ok(());
        }
        // Only an authenticated, exactly correlated Inactive is retryable.
        // Every connection is closed before this bounded wait; no effects replay.
        owner.wait(tokio::time::sleep(RETRY)).await?;
    }
}
async fn exchange(request: &wire::Request, bytes: &[u8], path: &Path) -> Result<bool, wire::Error> {
    let mut socket = UnixStream::connect(path)
        .await
        .map_err(|_| wire::Error::Unavailable)?;
    let mut header = [0; 5];
    header[1..].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
    socket
        .write_all(&header)
        .await
        .map_err(|_| wire::Error::Unavailable)?;
    socket
        .write_all(bytes)
        .await
        .map_err(|_| wire::Error::Unavailable)?;
    socket
        .read_exact(&mut header)
        .await
        .map_err(|_| wire::Error::Unavailable)?;
    let header = wire::FrameHeader::decode(&header)?;
    if !matches!(
        header.kind,
        wire::FrameKind::ResponseBegin | wire::FrameKind::Error
    ) {
        return Err(wire::Error::InvalidRequest);
    }
    let mut response = Zeroizing::new(vec![0; wire::CONTROL_MAX]);
    socket
        .read_exact(&mut response[..header.length])
        .await
        .map_err(|_| wire::Error::Unavailable)?;
    let response = &response[..header.length];
    let inactive = if header.kind == wire::FrameKind::Error {
        let refusal: wire::Refusal =
            serde_json::from_slice(response).map_err(|_| wire::Error::InvalidRequest)?;
        refusal.validate(request)?;
        if refusal.error != wire::Error::Inactive {
            return Err(refusal.error);
        }
        true
    } else {
        wire::Response::decode(response, request)?;
        false
    };
    // One response only, including EOF. A stalled or appended frame cannot
    // publish Ready or turn a control exchange into an unbounded stream.
    if socket
        .read(&mut [0])
        .await
        .map_err(|_| wire::Error::Unavailable)?
        != 0
    {
        return Err(wire::Error::InvalidRequest);
    }
    Ok(inactive)
}

/// Read-only fixture evidence from the actual retained Ready publisher. It does
/// not mint a synthetic proof, alter phase or authorize ordinary execution.
#[cfg(all(unix, feature = "daemon-extension-fixture"))]
pub(super) fn fixture_verified(state: &AppState) -> Result<bool, wire::Error> {
    let pending = pending(state).ok_or(wire::Error::Inactive)?;
    pending
        .protection
        .current()
        .map_err(|_| wire::Error::StateChanged)?;
    let inner = lock(&pending.ready.inner);
    match inner.phase {
        Phase::Staged | Phase::Checking => Ok(false),
        Phase::Closed => Err(wire::Error::StateChanged),
        Phase::Verified => {
            let generation = inner
                .work
                .as_ref()
                .ok_or(wire::Error::StateChanged)?
                .generation;
            if current(state, &pending, generation) {
                Ok(true)
            } else {
                Err(wire::Error::StateChanged)
            }
        }
    }
}

#[cfg(test)]
#[path = "provider_ready_tests.rs"]
pub(super) mod tests;

#[cfg(any(test, feature = "daemon-extension-fixture"))]
#[path = "provider_test_fixture.rs"]
pub(super) mod test_fixture;
