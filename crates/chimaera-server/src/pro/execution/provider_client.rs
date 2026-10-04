//! One fixed authenticated control exchange. Owners retain activity and quotas
//! through socket/child cleanup; a lost observer never admits a replacement.
use super::{provider_ready, provider_startup::Pending};
use crate::AppState;
use chimaera_core::provider_runtime as wire;
use std::{
    future::Future,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::{Semaphore, SemaphorePermit},
};
use zeroize::Zeroizing;

static WORK: Semaphore = Semaphore::const_new(wire::STREAMS_GLOBAL);
static CHILDREN: Semaphore = Semaphore::const_new(16);
const SOCKET: &str = "/run/chimaera/providers.sock";

/// No Clone/Debug: only exact retained Ready can mint this owner. It has no
/// caller-supplied binding, registration, capability, transport or generation.
pub struct Owner {
    state: Arc<AppState>,
    pending: Arc<Pending>,
    work: Arc<provider_ready::Work>,
    request: Option<wire::Request>,
    permit: Option<SemaphorePermit<'static>>,
    child_pending: AtomicBool,
    request_used: AtomicBool,
    parent: Option<Arc<ChildLifetime>>,
}
impl Owner {
    #[cfg(feature = "daemon-extension-fixture")]
    pub fn admit(
        context: &super::provider_fixture_host::Context,
        command: wire::Command,
        deadline: Instant,
    ) -> Result<Self, wire::Error> {
        context.current()?;
        let owner = Self::new(&context.state, command, deadline)?;
        context.current()?;
        Ok(owner)
    }
    pub(super) fn new(
        state: &Arc<AppState>,
        command: wire::Command,
        deadline: Instant,
    ) -> Result<Self, wire::Error> {
        if !matches!(
            &command,
            wire::Command::GithubGhAccess {} | wire::Command::GithubHttpsCredentials { .. }
        ) {
            return Err(wire::Error::Unsupported);
        }
        let permit = WORK.try_acquire().map_err(|_| wire::Error::LimitReached)?;
        let (pending, work, request) = provider_ready::admit(state, command, deadline)?;
        let owner = Self {
            state: state.clone(),
            pending,
            work,
            request: Some(request),
            permit: Some(permit),
            child_pending: AtomicBool::new(false),
            request_used: AtomicBool::new(false),
            parent: None,
        };
        owner.current()?;
        Ok(owner)
    }
    pub fn for_child(
        child: &Arc<ChildLifetime>,
        command: wire::Command,
        deadline: Instant,
    ) -> Result<Self, wire::Error> {
        child.current()?;
        if !matches!(command, wire::Command::ClaudeStreamPinned { .. }) {
            return Err(wire::Error::Unsupported);
        }
        let permit = WORK.try_acquire().map_err(|_| wire::Error::LimitReached)?;
        let (pending, work, request) = provider_ready::admit_child_stream(
            &child.state,
            &child.pending,
            &child.work,
            command,
            deadline.min(child.work.deadline),
        )?;
        let owner = Self {
            state: child.state.clone(),
            pending,
            work,
            request: Some(request),
            permit: Some(permit),
            child_pending: AtomicBool::new(false),
            request_used: AtomicBool::new(false),
            parent: Some(child.clone()),
        };
        owner.current()?;
        Ok(owner)
    }
    pub fn claim(&self) -> Result<&wire::Request, wire::Error> {
        self.current()?;
        if self.request_used.swap(true, Ordering::AcqRel) {
            return Err(wire::Error::InvalidRequest);
        }
        self.request.as_ref().ok_or(wire::Error::StateChanged)
    }
    pub async fn connect(&self) -> Result<UnixStream, wire::Error> {
        let path = std::path::PathBuf::from(SOCKET);
        #[cfg(any(test, feature = "daemon-extension-fixture"))]
        let path = provider_ready::socket_fixture(&self.pending).unwrap_or(path);
        self.wait(UnixStream::connect(path))
            .await?
            .map_err(|_| wire::Error::Unavailable)
    }
    pub fn current(&self) -> Result<(), wire::Error> {
        if self
            .parent
            .as_ref()
            .is_some_and(|parent| parent.current().is_err())
            || Instant::now() >= self.work.deadline
            || *self.work.cancel.borrow()
            || !provider_ready::consumer_current(&self.pending, &self.work)
            || !provider_ready::current(&self.state, &self.pending, self.work.generation)
        {
            Err(wire::Error::StateChanged)
        } else {
            Ok(())
        }
    }
    pub async fn wait<T>(&self, effect: impl Future<Output = T>) -> Result<T, wire::Error> {
        self.current()?;
        let mut cancelled = self.work.cancel.subscribe();
        let mut parent = self
            .parent
            .as_ref()
            .map(|parent| parent.work.cancel.subscribe());
        let result = tokio::select! {
            biased;
            _ = cancelled.wait_for(|cancelled| *cancelled) => return Err(wire::Error::StateChanged),
            _ = async {
                if let Some(parent) = &mut parent { let _ = parent.wait_for(|cancelled| *cancelled).await; }
                else { std::future::pending::<()>().await; }
            } => return Err(wire::Error::StateChanged),
            _ = tokio::time::sleep_until(self.work.deadline.into()) => return Err(wire::Error::Unavailable),
            result = effect => result,
        };
        self.current()?;
        Ok(result)
    }
    pub fn project_root(&self) -> Result<std::path::PathBuf, wire::Error> {
        self.current()?;
        let root = accepted_root(&self.state, &self.pending)?;
        self.current()?;
        Ok(root)
    }
    pub fn observer(&self) -> Observer {
        Observer {
            cancel: self.work.cancel.clone(),
        }
    }
    pub fn child_pending(&self, pending: bool) {
        self.child_pending.store(pending, Ordering::Release);
    }
    pub async fn github(&self) -> Result<wire::GithubAccess, wire::Error> {
        self.current()?;
        if self.request_used.swap(true, Ordering::AcqRel) {
            return Err(wire::Error::InvalidRequest);
        }
        let request = self.request.as_ref().ok_or(wire::Error::StateChanged)?;
        let bytes = wire::encode_control(request)?;
        let path = std::path::PathBuf::from(SOCKET);
        #[cfg(any(test, feature = "daemon-extension-fixture"))]
        let path = provider_ready::socket_fixture(&self.pending).unwrap_or(path);
        let response = exchange(self, request, &bytes, &path).await?;
        match response.result {
            wire::Reply::GithubAccess { access } => {
                self.current()?;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| wire::Error::Unavailable)?
                    .as_secs();
                if access.expires_at.is_some_and(|expiry| expiry <= now) {
                    return Err(wire::Error::NeedsSignIn);
                }
                Ok(access)
            }
            _ => Err(wire::Error::InvalidRequest),
        }
    }
}
/// One child/frontend pinned to the original attachment; not a stream request.
/// The observer retires it immediately, while actual child cleanup stays owned.
pub struct ChildLifetime {
    state: Arc<AppState>,
    pending: Arc<Pending>,
    work: Arc<provider_ready::Work>,
    permit: Option<SemaphorePermit<'static>>,
    process_pending: AtomicBool,
}
impl ChildLifetime {
    #[cfg(feature = "daemon-extension-fixture")]
    pub fn admit(
        context: &super::provider_fixture_host::Context,
        deadline: Instant,
    ) -> Result<Arc<Self>, wire::Error> {
        context.current()?;
        let child = Self::new(&context.state, deadline)?;
        context.current()?;
        Ok(child)
    }
    pub(super) fn new(state: &Arc<AppState>, deadline: Instant) -> Result<Arc<Self>, wire::Error> {
        let permit = CHILDREN
            .try_acquire()
            .map_err(|_| wire::Error::LimitReached)?;
        let (pending, work) = provider_ready::admit_child(state, deadline)?;
        let child = Arc::new(Self {
            state: state.clone(),
            pending,
            work,
            permit: Some(permit),
            process_pending: AtomicBool::new(false),
        });
        child.current()?;
        Ok(child)
    }
    pub fn current(&self) -> Result<(), wire::Error> {
        if Instant::now() >= self.work.deadline
            || *self.work.cancel.borrow()
            || !provider_ready::child_current(&self.pending, &self.work)
            || !provider_ready::current(&self.state, &self.pending, self.work.generation)
        {
            Err(wire::Error::StateChanged)
        } else {
            Ok(())
        }
    }
    pub fn cancel(&self) {
        self.work.cancel.send_replace(true);
    }
    pub fn observer(&self) -> Observer {
        Observer {
            cancel: self.work.cancel.clone(),
        }
    }
    pub fn cancellation(&self) -> tokio::sync::watch::Receiver<bool> {
        self.work.cancel.subscribe()
    }
    pub fn deadline(&self) -> Instant {
        self.work.deadline
    }
    pub fn process_pending(&self, pending: bool) {
        self.process_pending.store(pending, Ordering::Release);
    }
    pub fn project_root(&self) -> Result<std::path::PathBuf, wire::Error> {
        self.current()?;
        let root = accepted_root(&self.state, &self.pending)?;
        self.current()?;
        Ok(root)
    }
    pub async fn wait<T>(&self, effect: impl Future<Output = T>) -> Result<T, wire::Error> {
        self.current()?;
        let mut cancelled = self.cancellation();
        let value = tokio::select! {
            biased;
            _ = cancelled.wait_for(|value| *value) => return Err(wire::Error::StateChanged),
            _ = tokio::time::sleep_until(self.work.deadline.into()) => return Err(wire::Error::Unavailable),
            value = effect => value,
        };
        self.current()?;
        Ok(value)
    }
}
impl Drop for ChildLifetime {
    fn drop(&mut self) {
        self.cancel();
        if self.process_pending.load(Ordering::Acquire) {
            if let Some(permit) = self.permit.take() {
                std::mem::forget(permit);
            }
        } else {
            self.work.done.send_replace(true);
        }
        self.state.changes.notify_waiters();
    }
}
fn accepted_root(state: &AppState, pending: &Pending) -> Result<std::path::PathBuf, wire::Error> {
    // Configure keeps the accepted directory in workspace authority, not
    // its credential configuration. Require the exact captured launch;
    // never use a default workspace or an unbound caller-selected cwd.
    let root = {
        let authority = crate::lock(&state.pro.authority);
        match &*authority {
            crate::pro::authority::Authority::Bound(accepted)
                if accepted.cleanup_binding(
                    &pending.launch.account_id,
                    &pending.launch.workspace_id,
                    pending.launch.registration_revision,
                    (
                        pending.launch.root_identity.device,
                        pending.launch.root_identity.inode,
                    ),
                ) =>
            {
                accepted.root.clone()
            }
            _ => return Err(wire::Error::StateChanged),
        }
    };
    Ok(root)
}
/// Dropping the awaiting caller synchronously retires authority; the producer
/// still owns its actual socket/group cleanup and quota until positive completion.
pub struct Observer {
    cancel: tokio::sync::watch::Sender<bool>,
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.cancel.send_replace(true);
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        // Callers keep this owner outside all socket and child temporaries. A
        // failed cleanup must retain it, rather than manufacturing quiet/idle.
        drop(self.request.take());
        if self.child_pending.load(Ordering::Acquire) {
            // A panic/task abort is not a positive process receipt. Retain this
            // bounded slot and activity fence until supervisor recovery; never
            // admit new effects while group cleanup is unknown.
            if let Some(permit) = self.permit.take() {
                std::mem::forget(permit);
            }
            self.state.changes.notify_waiters();
            return;
        }
        self.work.done.send_replace(true);
        self.state.changes.notify_waiters();
    }
}
async fn exchange(
    owner: &Owner,
    request: &wire::Request,
    bytes: &[u8],
    path: &Path,
) -> Result<wire::Response, wire::Error> {
    let mut socket = owner
        .wait(UnixStream::connect(path))
        .await?
        .map_err(|_| wire::Error::Unavailable)?;
    let mut header = [0; 5];
    header[1..].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
    owner
        .wait(socket.write_all(&header))
        .await?
        .map_err(|_| wire::Error::Unavailable)?;
    owner
        .wait(socket.write_all(bytes))
        .await?
        .map_err(|_| wire::Error::Unavailable)?;
    owner
        .wait(socket.read_exact(&mut header))
        .await?
        .map_err(|_| wire::Error::Unavailable)?;
    let header = wire::FrameHeader::decode(&header)?;
    if !matches!(
        header.kind,
        wire::FrameKind::ResponseBegin | wire::FrameKind::Error
    ) {
        return Err(wire::Error::InvalidRequest);
    }
    // Allocate once before any access token arrives. No secret-bearing growth.
    let mut bytes = Zeroizing::new(vec![0; wire::CONTROL_MAX]);
    owner
        .wait(socket.read_exact(&mut bytes[..header.length]))
        .await?
        .map_err(|_| wire::Error::Unavailable)?;
    let result = if header.kind == wire::FrameKind::Error {
        let refusal: wire::Refusal = serde_json::from_slice(&bytes[..header.length])
            .map_err(|_| wire::Error::InvalidRequest)?;
        refusal.validate(request)?;
        Err(refusal.error)
    } else {
        wire::Response::decode(&bytes[..header.length], request)
    };
    // Correlation does not excuse a second/truncated/stalled frame. No retry,
    // refresh, TCP or legacy credential fallback occurs, including Inactive.
    if owner
        .wait(socket.read(&mut [0]))
        .await?
        .map_err(|_| wire::Error::Unavailable)?
        != 0
    {
        return Err(wire::Error::InvalidRequest);
    }
    owner.current()?;
    result
}

#[cfg(test)]
#[path = "provider_client_tests.rs"]
pub(super) mod tests;
