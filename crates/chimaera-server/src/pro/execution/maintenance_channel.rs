//! Framing/provenance for the optional trusted fixed-launcher maintenance
//! socket. Only the opt-in actor sends Ready and exact parking receipts.
//! A valid frame is structure, not authenticated idle or process evidence.
use super::AppState;
use chimaera_core::project_secret_idle::{Binding, Ready, Reply, Request, REPLY_MAX, REQUEST_MAX};
use std::{
    os::fd::{AsRawFd, OwnedFd},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::{OwnedMutexGuard, OwnedSemaphorePermit},
};

const FRAME_DEADLINE: Duration = Duration::from_secs(3);
const REQUESTS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct InvalidChannel;
impl std::fmt::Display for InvalidChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("project secret idle channel unavailable")
    }
}
impl std::error::Error for InvalidChannel {}
type Result<T> = std::result::Result<T, InvalidChannel>;

/// Only the trusted fixed startup envelope may transfer this descriptor. These
/// checks exclude named/abstract sockets and other fd kinds, but do not replace
/// the launcher's provenance or the Linux proc/ptrace hardening gate.
pub(super) struct Channel {
    stream: UnixStream,
    binding: Binding,
    generation: u64,
    // The trusted startup envelope selects this handshake nonce. Merely
    // retaining/validating it never sends Ready or enables this extension.
    channel_nonce: String,
    last_request: u64,
    ready_sent: bool,
    closed: Arc<AtomicBool>,
    requests: Arc<tokio::sync::Semaphore>,
    effect: Arc<tokio::sync::Mutex<()>>,
}
impl Channel {
    pub(super) fn from_inherited(
        state: &AppState,
        descriptor: OwnedFd,
        binding: Binding,
        channel_nonce: String,
    ) -> Result<Self> {
        Reply::Ready(Ready {
            version: 1,
            binding: binding.clone(),
            channel_nonce: channel_nonce.clone(),
            project_secrets_idle: 1,
        })
        .validate()
        .map_err(|_| InvalidChannel)?;
        let generation = super::mutation::generation(state);
        if descriptor.as_raw_fd() <= 2 || !super::supervisor::matches_maintenance(state, &binding) {
            return Err(InvalidChannel);
        }
        let raw = descriptor.as_raw_fd();
        let mut kind = 0 as nix::libc::c_int;
        let mut kind_len = std::mem::size_of_val(&kind) as nix::libc::socklen_t;
        let mut address: nix::libc::sockaddr_storage = unsafe { std::mem::zeroed() };
        let mut address_len = std::mem::size_of_val(&address) as nix::libc::socklen_t;
        if unsafe {
            nix::libc::getsockopt(
                raw,
                nix::libc::SOL_SOCKET,
                nix::libc::SO_TYPE,
                (&mut kind as *mut nix::libc::c_int).cast(),
                &mut kind_len,
            )
        } != 0
            || kind_len as usize != std::mem::size_of_val(&kind)
            || kind != nix::libc::SOCK_STREAM
            || unsafe {
                nix::libc::getsockname(
                    raw,
                    (&mut address as *mut nix::libc::sockaddr_storage).cast(),
                    &mut address_len,
                )
            } != 0
            || address.ss_family as nix::libc::c_int != nix::libc::AF_UNIX
        {
            return Err(InvalidChannel);
        }
        let flags = unsafe { nix::libc::fcntl(raw, nix::libc::F_GETFD) };
        if flags < 0
            || unsafe { nix::libc::fcntl(raw, nix::libc::F_SETFD, flags | nix::libc::FD_CLOEXEC) }
                != 0
        {
            return Err(InvalidChannel);
        }
        let stream = std::os::unix::net::UnixStream::from(descriptor);
        for address in [stream.local_addr(), stream.peer_addr()] {
            let address = address.map_err(|_| InvalidChannel)?;
            if !address.is_unnamed() {
                return Err(InvalidChannel);
            }
        }
        stream.set_nonblocking(true).map_err(|_| InvalidChannel)?;
        let stream = UnixStream::from_std(stream).map_err(|_| InvalidChannel)?;
        if generation != super::mutation::generation(state)
            || !super::supervisor::matches_maintenance(state, &binding)
        {
            return Err(InvalidChannel);
        }
        Ok(Self {
            stream,
            binding,
            generation,
            channel_nonce,
            last_request: 0,
            ready_sent: false,
            closed: Arc::new(AtomicBool::new(false)),
            requests: Arc::new(tokio::sync::Semaphore::new(REQUESTS)),
            effect: Arc::new(tokio::sync::Mutex::new(())),
        })
    }
    fn current(&self, state: &AppState) -> Result<()> {
        if self.closed.load(Ordering::Acquire)
            || self.generation != super::mutation::generation(state)
            || !super::supervisor::matches_maintenance(state, &self.binding)
        {
            return Err(InvalidChannel);
        }
        Ok(())
    }
    /// Only the actual inherited actor, after restoration and launch checks,
    /// may negotiate this channel. A repeated or late Ready permanently fails.
    pub(super) async fn ready(&mut self, state: &AppState) -> Result<()> {
        let mut frame = FrameOwner::new(self.stream.as_raw_fd(), self.closed.clone());
        self.current(state)?;
        if self.ready_sent || self.last_request != 0 || !*state.restored.borrow() {
            return Err(InvalidChannel);
        }
        let reply = Reply::Ready(Ready {
            version: 1,
            binding: self.binding.clone(),
            channel_nonce: self.channel_nonce.clone(),
            project_secrets_idle: 1,
        });
        let bytes = reply.encode().map_err(|_| InvalidChannel)?;
        tokio::time::timeout(FRAME_DEADLINE, async {
            self.stream
                .write_all(&(bytes.len() as u32).to_be_bytes())
                .await
                .map_err(|_| InvalidChannel)?;
            self.stream
                .write_all(&bytes)
                .await
                .map_err(|_| InvalidChannel)?;
            self.stream.flush().await.map_err(|_| InvalidChannel)
        })
        .await
        .map_err(|_| InvalidChannel)??;
        self.current(state)?;
        self.ready_sent = true;
        frame.completed();
        Ok(())
    }
    pub(super) fn binding(&self) -> &Binding {
        &self.binding
    }
    /// One reader owns this stream. Capacity includes a partial frame and must
    /// follow any admitted work into its actual blocking/cleanup owner.
    pub(super) async fn read(&mut self, state: &AppState) -> Result<OwnedRequest> {
        let mut frame = FrameOwner::new(self.stream.as_raw_fd(), self.closed.clone());
        self.current(state)?;
        let capacity = self
            .requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| InvalidChannel)?;
        // Idle waiting has no effects and is canceled by the eventual launch
        // owner. Once one byte arrives, the entire remaining frame is bounded.
        let first = self.stream.read_u8().await.map_err(|_| InvalidChannel)?;
        let received_at = Instant::now();
        let bytes = tokio::time::timeout(FRAME_DEADLINE, async {
            let mut header = [0; 4];
            header[0] = first;
            self.stream
                .read_exact(&mut header[1..])
                .await
                .map_err(|_| InvalidChannel)?;
            let size = u32::from_be_bytes(header) as usize;
            if size == 0 || size > REQUEST_MAX {
                return Err(InvalidChannel);
            }
            let mut bytes = vec![0; size];
            self.stream
                .read_exact(&mut bytes)
                .await
                .map_err(|_| InvalidChannel)?;
            Ok(bytes)
        })
        .await
        .map_err(|_| InvalidChannel)??;
        let request = Request::decode(&bytes).map_err(|_| InvalidChannel)?;
        let identity = request.identity();
        let request_id = request.request_id();
        self.current(state)?;
        if identity.binding != self.binding
            || request_id <= self.last_request
            || request_id == u64::MAX
        {
            return Err(InvalidChannel);
        }
        self.last_request = request_id;
        let deadline = match &request {
            Request::Prepare(prepare) => received_at + Duration::from_millis(prepare.expires_in_ms),
            _ => received_at + Duration::from_secs(30),
        };
        frame.completed();
        Ok(OwnedRequest {
            request,
            deadline,
            generation: self.generation,
            closed: self.closed.clone(),
            capacity,
            effect: self.effect.clone(),
        })
    }
    /// Reply correlation is exact to the admitted request. Ready requires a
    /// separate reviewed handshake; this helper cannot advertise support.
    /// The actor must supply separately proved Prepared/rollback data.
    pub(super) async fn write(
        &mut self,
        state: &AppState,
        owner: &OwnedRequest,
        reply: &Reply,
    ) -> Result<()> {
        let request = &owner.request;
        let mut frame = FrameOwner::new(self.stream.as_raw_fd(), self.closed.clone());
        self.current(state)?;
        owner.current(state)?;
        request.validate().map_err(|_| InvalidChannel)?;
        if !Arc::ptr_eq(&owner.closed, &self.closed)
            || request.identity().binding != self.binding
            || reply.identity() != Some(request.identity())
            || reply.request_id() != Some(request.request_id())
        {
            return Err(InvalidChannel);
        }
        let bytes = reply.encode().map_err(|_| InvalidChannel)?;
        if bytes.is_empty() || bytes.len() > REPLY_MAX {
            return Err(InvalidChannel);
        }
        tokio::time::timeout(FRAME_DEADLINE, async {
            self.stream
                .write_all(&(bytes.len() as u32).to_be_bytes())
                .await
                .map_err(|_| InvalidChannel)?;
            self.stream
                .write_all(&bytes)
                .await
                .map_err(|_| InvalidChannel)?;
            self.stream.flush().await.map_err(|_| InvalidChannel)
        })
        .await
        .map_err(|_| InvalidChannel)??;
        self.current(state)?;
        owner.current(state)?;
        frame.completed();
        Ok(())
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        // Queued owners can outlive the reader. Losing the actual stream never
        // permits them to start effects, while existing cleanup stays owned.
        self.closed.store(true, Ordering::Release);
    }
}

pub(super) struct OwnedRequest {
    request: Request,
    deadline: Instant,
    generation: u64,
    // This exact stream's fence follows queued and actual work. Equal launch
    // bindings on another stream never authorize a reply or revive closed work.
    closed: Arc<AtomicBool>,
    // Not released when a detached task's observer disappears. The actual task
    // or blocking closure must own this entire value through settled cleanup.
    capacity: OwnedSemaphorePermit,
    effect: Arc<tokio::sync::Mutex<()>>,
}
impl OwnedRequest {
    pub(super) fn request(&self) -> &Request {
        &self.request
    }
    pub(super) fn deadline(&self) -> Instant {
        self.deadline
    }
    fn current(&self, state: &AppState) -> Result<()> {
        if self.closed.load(Ordering::Acquire)
            || self.generation != super::mutation::generation(state)
            || !super::supervisor::matches_maintenance(state, &self.request.identity().binding)
        {
            return Err(InvalidChannel);
        }
        Ok(())
    }
    pub(super) async fn into_effect(self, state: &AppState) -> Result<EffectOwner> {
        self.current(state)?;
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or(InvalidChannel)?;
        let guard = tokio::time::timeout(remaining, self.effect.clone().lock_owned())
            .await
            .map_err(|_| InvalidChannel)?;
        self.current(state)?;
        if Instant::now() >= self.deadline {
            return Err(InvalidChannel);
        }
        Ok(EffectOwner {
            owner: self,
            _effect: guard,
        })
    }
}
/// One indivisible owned value retains both budgets through actual effects or
/// blocking cleanup. Observer cancellation must never drop this actual owner.
pub(super) struct EffectOwner {
    owner: OwnedRequest,
    _effect: OwnedMutexGuard<()>,
}
impl EffectOwner {
    pub(super) fn owner(&self) -> &OwnedRequest {
        &self.owner
    }
}

/// This guard stays inside a future borrowing the owned Channel, so its raw fd
/// cannot outlive or be reused before that borrow ends. A partial frame cannot
/// be retried after timeout, parse failure, or caller cancellation.
struct FrameOwner {
    fd: std::os::fd::RawFd,
    closed: Arc<AtomicBool>,
    armed: bool,
}
impl FrameOwner {
    fn new(fd: std::os::fd::RawFd, closed: Arc<AtomicBool>) -> Self {
        Self {
            fd,
            closed,
            armed: true,
        }
    }
    fn completed(&mut self) {
        self.armed = false;
    }
}
impl Drop for FrameOwner {
    fn drop(&mut self) {
        if self.armed {
            self.closed.store(true, Ordering::Release);
            // Shutdown is an immediate kernel operation on our exact stream;
            // Drop of Channel eventually closes the descriptor itself.
            unsafe {
                nix::libc::shutdown(self.fd, nix::libc::SHUT_RDWR);
            }
        }
    }
}

#[cfg(all(test, unix))]
#[path = "maintenance_channel_tests.rs"]
mod tests;
