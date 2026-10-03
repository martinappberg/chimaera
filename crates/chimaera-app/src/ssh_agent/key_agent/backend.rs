//! Per-leg immutable source mapping. A refused key stays refused; no sign is
//! retried against another agent, including a duplicate software key.
use super::{
    load::{CertificateFile, SelectedFile},
    Admission, Session,
};
use crate::ssh_agent::{
    packet::Reader, trust::Owner, unix::UnixAgent, AgentConnection, Failure, LocalAgent,
};
use std::{collections::HashMap, sync::Arc};
use tokio::{net::UnixStream, time::Instant};
#[derive(Clone)]
pub(crate) struct Agent {
    external: Option<UnixAgent>,
    private: Option<Session>,
    keys: Arc<HashMap<Vec<u8>, usize>>,
    files: Arc<[SelectedFile]>,
    owner: Option<Owner>,
    deadline: Option<Instant>,
    certificates: Option<(Admission, Arc<[CertificateFile]>)>,
}
impl Agent {
    pub(crate) fn external(agent: UnixAgent) -> Self {
        Self {
            external: Some(agent),
            private: None,
            keys: Arc::default(),
            files: Arc::from([]),
            owner: None,
            deadline: None,
            certificates: None,
        }
    }
    pub(crate) fn selected(
        external: Option<UnixAgent>,
        external_keys: &[String],
        private: Option<Session>,
        loaded: &[(Vec<u8>, SelectedFile)],
        owner: Owner,
        deadline: Instant,
    ) -> Result<Self, Failure> {
        let mut keys = HashMap::new();
        for key in external_keys {
            let key = chimaera_link::decode_packet(key, chimaera_link::SSH_AUTH_KEY_MAX)
                .map_err(|_| Failure::InvalidRequest)?;
            keys.insert(key, 0);
        }
        for (key, _) in loaded {
            keys.entry(key.clone()).or_insert(1);
        }
        if keys.len() > chimaera_link::SSH_AUTH_KEYS_MAX
            || keys.is_empty()
            || keys.values().any(|source| match source {
                0 => external.is_none(),
                1 => private.is_none(),
                _ => true,
            })
        {
            return Err(Failure::KeyUnavailable);
        }
        Ok(Self {
            external,
            private,
            keys: Arc::new(keys),
            files: loaded
                .iter()
                .map(|(_, f)| f.clone())
                .collect::<Vec<_>>()
                .into(),
            owner: Some(owner),
            deadline: Some(deadline),
            certificates: None,
        })
    }
    pub(crate) fn with_certificates(
        mut self,
        admission: Admission,
        files: Vec<CertificateFile>,
    ) -> Self {
        self.certificates = Some((admission, files.into()));
        self
    }
    async fn check(&self) -> Result<(), Failure> {
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
            || self
                .owner
                .as_ref()
                .is_some_and(|owner| !owner.guard.active() || !(owner.current)())
        {
            return Err(Failure::Revoked);
        }
        if let Some((admission, certificates)) = &self.certificates {
            for certificate in certificates.iter() {
                certificate
                    .check(admission)
                    .await
                    .map_err(|_| Failure::Revoked)?;
            }
        }
        if let Some(session) = &self.private {
            session.0.check().map_err(|_| Failure::Revoked)?;
            for file in self.files.iter() {
                file.check(&session.0).await.map_err(|_| Failure::Revoked)?;
            }
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
            || self
                .owner
                .as_ref()
                .is_some_and(|owner| !owner.guard.active() || !(owner.current)())
        {
            return Err(Failure::Revoked);
        }
        Ok(())
    }
}
pub(crate) struct Connection {
    agent: Agent,
    external: Option<UnixStream>,
    private: Option<UnixStream>,
}
impl LocalAgent for Agent {
    type Connection = Connection;
    async fn connect(&self) -> Result<Connection, Failure> {
        self.check().await?;
        let external = if let Some(agent) = &self.external {
            Some(agent.connect().await?)
        } else {
            None
        };
        let private = if let Some(session) = &self.private {
            Some(
                session
                    .0
                    .connect()
                    .await
                    .map_err(|_| Failure::KeyUnavailable)?,
            )
        } else {
            None
        };
        self.check().await?;
        Ok(Connection {
            agent: self.clone(),
            external,
            private,
        })
    }
}
impl AgentConnection for Connection {
    async fn exchange(&mut self, packet: &[u8]) -> Result<Vec<u8>, Failure> {
        self.agent.check().await?;
        let reply = if self.agent.owner.is_none() {
            self.external
                .as_mut()
                .ok_or(Failure::KeyUnavailable)?
                .exchange(packet)
                .await?
        } else {
            let mut reader = Reader::new(packet)?;
            match packet.first() {
                Some(27) => {
                    for socket in [&mut self.external, &mut self.private]
                        .into_iter()
                        .flatten()
                    {
                        if socket.exchange(packet).await? != [6] {
                            return Err(Failure::AgentRefused);
                        }
                    }
                    vec![6]
                }
                Some(13) => {
                    reader.byte_is(13)?;
                    let key = reader.string()?;
                    reader.string()?;
                    reader.u32()?;
                    reader.end()?;
                    let source = self.agent.keys.get(key).ok_or(Failure::KeyUnavailable)?;
                    let socket = match source {
                        0 => &mut self.external,
                        1 => &mut self.private,
                        _ => return Err(Failure::KeyUnavailable),
                    };
                    socket
                        .as_mut()
                        .ok_or(Failure::KeyUnavailable)?
                        .exchange(packet)
                        .await?
                }
                _ => return Err(Failure::InvalidRequest),
            }
        };
        self.agent.check().await?;
        Ok(reply)
    }
}
