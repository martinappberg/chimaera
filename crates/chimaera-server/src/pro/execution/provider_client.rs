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
const SOCKET: &str = "/run/chimaera/providers.sock";

/// No Clone/Debug: only exact retained Ready can mint this owner. It has no
/// caller-supplied binding, registration, capability, transport or generation.
pub(super) struct Owner {
    state: Arc<AppState>,
    pending: Arc<Pending>,
    work: Arc<provider_ready::Work>,
    request: Option<wire::Request>,
    permit: Option<SemaphorePermit<'static>>,
    child_pending: AtomicBool,
    request_used: AtomicBool,
}
impl Owner {
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
        };
        owner.current()?;
        Ok(owner)
    }
    pub(super) fn current(&self) -> Result<(), wire::Error> {
        if Instant::now() >= self.work.deadline
            || *self.work.cancel.borrow()
            || !provider_ready::consumer_current(&self.pending, &self.work)
            || !provider_ready::current(&self.state, &self.pending, self.work.generation)
        {
            Err(wire::Error::StateChanged)
        } else {
            Ok(())
        }
    }
    pub(super) async fn wait<T>(&self, effect: impl Future<Output = T>) -> Result<T, wire::Error> {
        self.current()?;
        let mut cancelled = self.work.cancel.subscribe();
        let result = tokio::select! {
            biased;
            _ = cancelled.wait_for(|cancelled| *cancelled) => return Err(wire::Error::StateChanged),
            _ = tokio::time::sleep_until(self.work.deadline.into()) => return Err(wire::Error::Unavailable),
            result = effect => result,
        };
        self.current()?;
        Ok(result)
    }
    pub(super) fn project_root(&self) -> Result<std::path::PathBuf, wire::Error> {
        self.current()?;
        crate::lock(&self.state.pro.runtime)
            .as_ref()
            .map(|config| config.workspace_root.clone())
            .ok_or(wire::Error::StateChanged)
    }
    pub(super) fn observer(&self) -> Observer {
        Observer {
            cancel: self.work.cancel.clone(),
        }
    }
    pub(super) fn child_pending(&self, pending: bool) {
        self.child_pending.store(pending, Ordering::Release);
    }
    pub(super) async fn github(&self) -> Result<wire::GithubAccess, wire::Error> {
        self.current()?;
        if self.request_used.swap(true, Ordering::AcqRel) {
            return Err(wire::Error::InvalidRequest);
        }
        let request = self.request.as_ref().ok_or(wire::Error::StateChanged)?;
        let bytes = wire::encode_control(request)?;
        let path = std::path::PathBuf::from(SOCKET);
        #[cfg(test)]
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
/// Dropping the awaiting caller synchronously retires authority; the producer
/// still owns its actual socket/group cleanup and quota until positive completion.
pub(super) struct Observer {
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
