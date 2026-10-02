//! An exact local-agent socket captured by native Connect, never from the wire.
use super::{AgentConnection, Failure, LocalAgent, SSH_AUTH_PACKET_MAX};
use std::path::PathBuf;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

pub(crate) struct UnixAgent(PathBuf);
impl UnixAgent {
    pub(crate) fn new(native_socket: PathBuf) -> Result<Self, Failure> {
        if !native_socket.is_absolute() {
            return Err(Failure::KeyUnavailable);
        }
        Ok(Self(native_socket))
    }
}
impl LocalAgent for UnixAgent {
    type Connection = UnixStream;
    async fn connect(&self) -> Result<Self::Connection, Failure> {
        UnixStream::connect(&self.0)
            .await
            .map_err(|_| Failure::KeyUnavailable)
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
