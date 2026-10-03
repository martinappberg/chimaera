//! A native-selected agent socket and responding peer, never from the wire.
//! Dev/inode and kernel PID checks detect replacement; they do not prove a
//! process birth identity or atomically exclude arbitrary local ABA mutation.
use super::{AgentConnection, Failure, LocalAgent, SSH_AUTH_PACKET_MAX};
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

#[derive(Clone)]
pub(crate) struct UnixAgent {
    path: PathBuf,
    pin: Pin,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
    owner: u32,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Peer {
    uid: u32,
    pid: u32,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Pin {
    socket: SocketIdentity,
    peer: Peer,
}
impl SocketIdentity {
    async fn read(path: &std::path::Path) -> Result<Self, Failure> {
        // Native IdentityAgent paths may be symlinks. Compare their followed
        // socket identity without changing the original configuration selector.
        // A canceled probe cannot release admission while a slow metadata
        // syscall still runs. Full admission refuses without another queued job.
        static CHECKS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
        let permit = CHECKS
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(16)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure::Unavailable)?;
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let metadata = std::fs::metadata(path).map_err(|_| Failure::KeyUnavailable)?;
            if !metadata.file_type().is_socket() {
                return Err(Failure::KeyUnavailable);
            }
            Ok(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                owner: metadata.uid(),
            })
        })
        .await
        .map_err(|_| Failure::Unavailable)?
    }
}
impl Peer {
    fn read(socket: &UnixStream) -> Result<Self, Failure> {
        #[cfg(target_os = "macos")]
        {
            use std::os::fd::AsRawFd;
            let mut uid = 0;
            let mut gid = 0;
            let mut pid = 0i32;
            let mut length = std::mem::size_of::<i32>() as nix::libc::socklen_t;
            if unsafe { nix::libc::getpeereid(socket.as_raw_fd(), &mut uid, &mut gid) } != 0
                || unsafe {
                    nix::libc::getsockopt(
                        socket.as_raw_fd(),
                        0,
                        nix::libc::LOCAL_PEERPID,
                        (&mut pid as *mut i32).cast(),
                        &mut length,
                    )
                } != 0
                || length as usize != std::mem::size_of::<i32>()
                || pid <= 0
            {
                return Err(Failure::KeyUnavailable);
            }
            Ok(Self {
                uid,
                pid: pid as u32,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let peer = socket.peer_cred().map_err(|_| Failure::KeyUnavailable)?;
            let pid = peer
                .pid()
                .filter(|pid| *pid > 0)
                .ok_or(Failure::KeyUnavailable)?;
            Ok(Self {
                uid: peer.uid(),
                pid: pid as u32,
            })
        }
    }
}
impl UnixAgent {
    pub(crate) async fn capture(native_socket: PathBuf) -> Result<(Self, Vec<u8>), Failure> {
        if !native_socket.is_absolute() {
            return Err(Failure::KeyUnavailable);
        }
        let (_, pin, identities) = Self::probe(&native_socket, None).await?;
        Ok((
            Self {
                path: native_socket,
                pin,
            },
            identities,
        ))
    }
    pub(super) fn path(&self) -> &std::path::Path {
        &self.path
    }
    async fn probe(
        path: &std::path::Path,
        expected: Option<Pin>,
    ) -> Result<(UnixStream, Pin, Vec<u8>), Failure> {
        // The caller's original Connect/bind deadline also bounds this future.
        // A new read-only identity response establishes the actual responder
        // before inspecting Darwin's last-peer PID on an inherited listener.
        tokio::time::timeout(Duration::from_secs(5), async {
            let before = SocketIdentity::read(path).await?;
            if expected.is_some_and(|pin| pin.socket != before) {
                return Err(Failure::KeyUnavailable);
            }
            let mut socket = UnixStream::connect(path)
                .await
                .map_err(|_| Failure::KeyUnavailable)?;
            if SocketIdentity::read(path).await? != before {
                return Err(Failure::KeyUnavailable);
            }
            let identities = socket.exchange(&[11]).await?;
            let pin = Pin {
                socket: SocketIdentity::read(path).await?,
                peer: Peer::read(&socket)?,
            };
            if pin.socket != before || expected.is_some_and(|expected| pin != expected) {
                return Err(Failure::KeyUnavailable);
            }
            // Do not reselect or merge advertised keys during later probes.
            // The selected key allow-list remains the original parsed reply.
            if identities.first() != Some(&12) {
                return Err(Failure::AgentRefused);
            }
            if expected.is_some() && super::selection::identity_reply(&identities).is_err() {
                return Err(Failure::AgentRefused);
            }
            Ok((socket, pin, identities))
        })
        .await
        .map_err(|_| Failure::Expired)?
    }
}
impl LocalAgent for UnixAgent {
    type Connection = UnixStream;
    async fn connect(&self) -> Result<Self::Connection, Failure> {
        let (socket, _, _) = Self::probe(&self.path, Some(self.pin)).await?;
        Ok(socket)
    }
}

impl AgentConnection for UnixStream {
    async fn exchange(&mut self, packet: &[u8]) -> Result<Vec<u8>, Failure> {
        if packet.is_empty() || packet.len() > SSH_AUTH_PACKET_MAX {
            return Err(Failure::InvalidRequest);
        }
        self.write_u32(packet.len() as u32)
            .await
            .map_err(|_| Failure::Unavailable)?;
        self.write_all(packet)
            .await
            .map_err(|_| Failure::Unavailable)?;
        self.flush().await.map_err(|_| Failure::Unavailable)?;
        let length = self.read_u32().await.map_err(|_| Failure::Unavailable)? as usize;
        if length == 0 || length > SSH_AUTH_PACKET_MAX {
            return Err(Failure::AgentRefused);
        }
        let mut reply = vec![0; length];
        self.read_exact(&mut reply)
            .await
            .map_err(|_| Failure::Unavailable)?;
        Ok(reply)
    }
}

#[cfg(test)]
#[path = "unix_tests.rs"]
mod tests;
