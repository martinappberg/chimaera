//! Per-leg native probe relays. Their private sockets never borrow the app's
//! generic askpass authority or expose an unrestricted local agent.
use super::super::{
    selection::{self, NativeIdentity, SelectionFailure},
    GrantVerifier, SshAuthReply, SshAuthRequest,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::Mutex,
    task::JoinSet,
    time::Instant,
};

pub(super) struct Bridge {
    tasks: JoinSet<()>,
    pub(super) bound: Arc<AtomicBool>,
    pub(super) receipt: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
}
struct Agent {
    identity: NativeIdentity,
    verifier: Option<GrantVerifier<super::super::unix::UnixAgent>>,
    text: String,
    home: PathBuf,
    candidate: PathBuf,
    empty: bool,
    sequence: u64,
    bound: Arc<AtomicBool>,
    receipt: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    signed: Arc<AtomicBool>,
}
async fn read_packet(stream: &mut UnixStream) -> Result<Vec<u8>, SelectionFailure> {
    let length = stream
        .read_u32()
        .await
        .map_err(|_| SelectionFailure::Unavailable)? as usize;
    if !(1..=chimaera_link::SSH_AUTH_PACKET_MAX).contains(&length) {
        return Err(SelectionFailure::Unavailable);
    }
    let mut bytes = vec![0; length];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|_| SelectionFailure::Unavailable)?;
    Ok(bytes)
}
async fn write_packet(stream: &mut UnixStream, packet: &[u8]) -> Result<(), SelectionFailure> {
    stream
        .write_u32(packet.len() as u32)
        .await
        .map_err(|_| SelectionFailure::Unavailable)?;
    stream
        .write_all(packet)
        .await
        .map_err(|_| SelectionFailure::Unavailable)
}
impl Agent {
    async fn reply(
        &mut self,
        connection: &str,
        packet: &[u8],
    ) -> Result<Vec<u8>, SelectionFailure> {
        match packet.first() {
            Some(11) if packet.len() == 1 => {
                let mut reply = vec![12];
                let keys = if self.empty {
                    &[][..]
                } else {
                    &self.identity.user_keys[..]
                };
                reply.extend_from_slice(&(keys.len() as u32).to_be_bytes());
                for key in keys {
                    let blob = chimaera_link::decode_packet(key, chimaera_link::SSH_AUTH_KEY_MAX)
                        .map_err(|_| SelectionFailure::Unavailable)?;
                    reply.extend_from_slice(&(blob.len() as u32).to_be_bytes());
                    reply.extend_from_slice(&blob);
                    reply.extend_from_slice(&0u32.to_be_bytes());
                }
                Ok(reply)
            }
            Some(27) if self.empty => {
                if self.bound.load(Ordering::Acquire) {
                    return Err(SelectionFailure::Unavailable);
                }
                let before = super::storage::candidate_bytes(&self.candidate)?;
                let (destination, hosts) = selection::candidate_trust(
                    &self.text,
                    &self.home,
                    &self.identity,
                    &self.candidate,
                )
                .await?;
                let policy = selection::native_policy(&self.identity, destination, hosts)?;
                policy
                    .bind(packet)
                    .map_err(|_| SelectionFailure::Unavailable)?;
                if before != super::storage::candidate_bytes(&self.candidate)? {
                    return Err(SelectionFailure::Unavailable);
                }
                *self
                    .receipt
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(before);
                self.bound.store(true, Ordering::Release);
                Ok(vec![6])
            }
            Some(kind @ (27 | 13)) if !self.empty => {
                self.sequence = self
                    .sequence
                    .checked_add(1)
                    .ok_or(SelectionFailure::Unavailable)?;
                let request = if *kind == 27 {
                    SshAuthRequest::SessionBind {
                        connection_id: connection.into(),
                        request_id: self.sequence,
                        packet: STANDARD.encode(packet),
                    }
                } else {
                    SshAuthRequest::Sign {
                        connection_id: connection.into(),
                        request_id: self.sequence,
                        packet: STANDARD.encode(packet),
                    }
                };
                match self
                    .verifier
                    .as_mut()
                    .ok_or(SelectionFailure::Unavailable)?
                    .handle(request)
                    .await
                    .map_err(|_| SelectionFailure::Unavailable)?
                {
                    Some(SshAuthReply::Bound { .. }) => Ok(vec![6]),
                    Some(SshAuthReply::Signature { packet, .. }) => {
                        self.signed.store(true, Ordering::Release);
                        chimaera_link::decode_packet(&packet, chimaera_link::SSH_AUTH_PACKET_MAX)
                            .map_err(|_| SelectionFailure::Unavailable)
                    }
                    _ => Err(SelectionFailure::Unavailable),
                }
            }
            _ => Err(SelectionFailure::Unavailable),
        }
    }
}
pub(super) struct Admission<'a> {
    pub(super) owner: super::Owner,
    pub(super) directory: &'a Path,
    pub(super) identity: NativeIdentity,
    pub(super) text: String,
    pub(super) home: PathBuf,
    pub(super) candidate: PathBuf,
    pub(super) leg: &'a chimaera_link::SshRouteAuthLeg,
    pub(super) empty: bool,
    pub(super) deadline: Instant,
}
impl Bridge {
    pub(super) fn start(admission: Admission<'_>) -> Result<Self, SelectionFailure> {
        let Admission {
            owner,
            directory,
            identity,
            text,
            home,
            candidate,
            leg,
            empty,
            deadline,
        } = admission;
        let guard = owner.guard.clone();
        let alias = owner.alias.clone();
        let agent_socket = UnixListener::bind(directory.join("agent"))
            .map_err(|_| SelectionFailure::Unavailable)?;
        let askpass = UnixListener::bind(directory.join("askpass"))
            .map_err(|_| SelectionFailure::Unavailable)?;
        let verifier = if empty || identity.agent.is_none() {
            None
        } else {
            Some(
                selection::selected_identity(identity.clone(), leg, "native-probe".into())
                    .ok_or(SelectionFailure::Unavailable)?
                    .verifier(deadline)
                    .map_err(|_| SelectionFailure::Unavailable)?
                    .1,
            )
        };
        let bound = Arc::new(AtomicBool::new(false));
        let signed = Arc::new(AtomicBool::new(false));
        let receipt = Arc::new(std::sync::Mutex::new(None));
        let state = Arc::new(Mutex::new(Agent {
            identity,
            verifier,
            text,
            home,
            candidate,
            empty,
            sequence: 0,
            bound: bound.clone(),
            receipt: receipt.clone(),
            signed: signed.clone(),
        }));
        let mut tasks = JoinSet::new();
        let agent_guard = guard.clone();
        tasks.spawn(async move {
            let mut connections = JoinSet::new();
            for index in 0..32 {
                let (mut stream, _) = tokio::select! {
                    biased;
                    _ = agent_guard.stopped() => break,
                    value = agent_socket.accept() => match value { Ok(value) => value, Err(_) => break },
                };
                let state = state.clone();
                let guard = agent_guard.clone();
                connections.spawn(async move {
                    let operation = async {
                        for _ in 0..128 {
                            let packet = read_packet(&mut stream).await?;
                            let mut state = state.lock().await;
                            if !guard.active() { return Err(SelectionFailure::Unavailable); }
                            let reply = state.reply(&format!("native-{index}"), &packet).await?;
                            if !guard.active() { return Err(SelectionFailure::Unavailable); }
                            write_packet(&mut stream, &reply).await?;
                        }
                        Err::<(), _>(SelectionFailure::Unavailable)
                    };
                    tokio::select! { biased; _ = guard.stopped() => {}, _ = operation => {} }
                });
            }
            // Drop aborts each owner on control loss, including a withheld agent.
            tokio::select! { biased; _ = agent_guard.stopped() => {}, _ = async { while connections.join_next().await.is_some() {} } => {} }
        });
        let signed_prompt = signed.clone();
        let mode = leg.mode;
        let unknown = leg.host_keys.is_empty();
        let policy = leg.policy.clone().ok_or(SelectionFailure::Unavailable)?;
        tasks.spawn(async move {
            for _ in 0..32 {
                let (mut stream, _) = tokio::select! {
                    biased;
                    _ = guard.stopped() => break,
                    value = askpass.accept() => match value { Ok(value) => value, Err(_) => break },
                };
                let operation = async {
                    let mut request = String::new();
                    (&mut stream)
                        .take(16 * 1024 + 1)
                        .read_to_string(&mut request)
                        .await
                        .ok()?;
                    if request.len() > 16 * 1024 {
                        return None;
                    }
                    let scoped = request
                        .strip_prefix(super::SCOPE_FRAME)?
                        .strip_prefix('\n')?;
                    let (claimed_alias, prompt) = scoped.split_once('\n')?;
                    if claimed_alias != alias {
                        return None;
                    }
                    let trust = prompt.starts_with("The authenticity of host ");
                    let interactive = !empty
                        && (mode == chimaera_link::SshRouteMode::Interactive
                            || signed_prompt.load(Ordering::Acquire));
                    if trust && !unknown {
                        return None;
                    }
                    if !trust
                        && (!interactive
                            || (!policy
                                .methods
                                .contains(&chimaera_link::SshRouteMethod::Password)
                                && !policy
                                    .methods
                                    .contains(&chimaera_link::SshRouteMethod::KeyboardInteractive)))
                    {
                        return None;
                    }
                    let answer = (owner.prompt)(prompt.into(), guard.clone()).await?;
                    if !guard.active() {
                        return None;
                    }
                    stream.write_all(answer.as_bytes()).await.ok()?;
                    stream.write_all(b"\n").await.ok()?;
                    stream.shutdown().await.ok()?;
                    Some(())
                };
                tokio::select! { biased; _ = guard.stopped() => break, _ = operation => {} }
            }
        });
        Ok(Self {
            tasks,
            bound,
            receipt,
        })
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.tasks.abort_all();
    }
}
