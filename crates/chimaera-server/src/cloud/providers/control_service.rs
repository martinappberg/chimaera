//! Inherited supervisor-only socket service. No daemon/project route or selected
//! executable exists here; the CLI dispatch only supplies owned descriptors.
use super::authority::{
    self, Action, ControlBinding, Error, FinishedLogin, LoginAttempt, LoginPhase, LoginStatus,
    PendingLogin,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    os::fd::OwnedFd,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};
use zeroize::Zeroizing;
const REQUEST: usize = 20 * 1024;
const REPLY: usize = 128 * 1024;
const IO: Duration = Duration::from_secs(5);
const RECORDS: usize = 24;
struct CommandJson(Zeroizing<String>);
impl<'de> Deserialize<'de> for CommandJson {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let raw = Box::<serde_json::value::RawValue>::deserialize(de)?;
        let text: Box<str> = raw.into();
        let value = Zeroizing::new(text.into_string());
        if value.len() > 8192 {
            return Err(serde::de::Error::custom("command exceeds bound"));
        }
        Ok(Self(value))
    }
}
enum Request {
    Command {
        capability: Zeroizing<String>,
        device_id: String,
        command: CommandJson,
        lease_deadline_ns: Option<u64>,
    },
    Renew {
        capability: Zeroizing<String>,
        operation_id: String,
        lease_deadline_ns: u64,
    },
    Status {
        capability: Zeroizing<String>,
        operation_id: String,
    },
    Credential {
        capability: Zeroizing<String>,
        operation_id: String,
    },
    Release {
        capability: Zeroizing<String>,
        operation_id: String,
    },
    Shutdown {
        capability: Zeroizing<String>,
    },
}
// Decode the fixed envelope directly. Internally tagged serde enums buffer the
// command through `Content`, which cannot retain RawValue's exact zeroizing
// allocation; a direct struct also rejects fields outside the selected variant.
impl<'de> Deserialize<'de> for Request {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        fn present<'de, T: Deserialize<'de>, D: serde::Deserializer<'de>>(
            de: D,
        ) -> Result<Option<T>, D::Error> {
            T::deserialize(de).map(Some)
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Envelope {
            #[serde(rename = "type")]
            kind: String,
            capability: Zeroizing<String>,
            #[serde(default, deserialize_with = "present")]
            device_id: Option<String>,
            #[serde(default, deserialize_with = "present")]
            command: Option<CommandJson>,
            #[serde(default, deserialize_with = "present")]
            lease_deadline_ns: Option<Option<u64>>,
            #[serde(default, deserialize_with = "present")]
            operation_id: Option<String>,
        }
        let e = Envelope::deserialize(de)?;
        let invalid = || serde::de::Error::custom("invalid control envelope");
        match e.kind.as_str() {
            "command" if e.operation_id.is_none() && e.lease_deadline_ns.is_some() => {
                Ok(Self::Command {
                    capability: e.capability,
                    device_id: e.device_id.ok_or_else(invalid)?,
                    command: e.command.ok_or_else(invalid)?,
                    lease_deadline_ns: e.lease_deadline_ns.flatten(),
                })
            }
            "renew" if e.device_id.is_none() && e.command.is_none() => Ok(Self::Renew {
                capability: e.capability,
                operation_id: e.operation_id.ok_or_else(invalid)?,
                lease_deadline_ns: e.lease_deadline_ns.flatten().ok_or_else(invalid)?,
            }),
            "status" | "credential" | "release"
                if e.device_id.is_none()
                    && e.command.is_none()
                    && e.lease_deadline_ns.is_none() =>
            {
                let operation_id = e.operation_id.ok_or_else(invalid)?;
                Ok(match e.kind.as_str() {
                    "status" => Self::Status {
                        capability: e.capability,
                        operation_id,
                    },
                    "credential" => Self::Credential {
                        capability: e.capability,
                        operation_id,
                    },
                    _ => Self::Release {
                        capability: e.capability,
                        operation_id,
                    },
                })
            }
            "shutdown"
                if e.device_id.is_none()
                    && e.command.is_none()
                    && e.lease_deadline_ns.is_none()
                    && e.operation_id.is_none() =>
            {
                Ok(Self::Shutdown {
                    capability: e.capability,
                })
            }
            _ => Err(invalid()),
        }
    }
}
impl Request {
    fn capability(&self) -> &str {
        match self {
            Self::Command { capability, .. }
            | Self::Renew { capability, .. }
            | Self::Status { capability, .. }
            | Self::Credential { capability, .. }
            | Self::Release { capability, .. }
            | Self::Shutdown { capability } => capability,
        }
    }
}
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Reply<'a> {
    Ready {
        registration: authority::Registration,
    },
    Status {
        status: LoginStatus,
    },
    Credential {
        operation_id: &'a str,
        credential: &'a serde_json::value::RawValue,
    },
    Released {
        operation_id: &'a str,
    },
    Stopped {
        cleanup_confirmed: bool,
    },
    Refused {
        error_code: &'static str,
    },
}
struct Record {
    digest: [u8; 32],
    parent: String,
}
struct Login {
    pending: PendingLogin,
    attempt: Option<LoginAttempt>,
    finished: Option<FinishedLogin>,
    status: LoginStatus,
    released: bool,
}
impl Login {
    fn snapshot(&mut self) -> LoginStatus {
        if let Some(attempt) = &self.attempt {
            self.status = attempt.snapshot();
        }
        if self.finished.is_some() && self.pending.check().is_err() {
            self.finished.take();
            self.released = true;
            self.status.phase = LoginPhase::Canceled;
            self.status.action = None;
            self.status.error_code = Some("expired");
        }
        self.status.clone()
    }
    fn terminal(&mut self) -> bool {
        self.snapshot();
        self.released || matches!(self.status.phase, LoginPhase::Failed | LoginPhase::Canceled)
    }
}
struct Service {
    binding: ControlBinding,
    records: BTreeMap<String, Record>,
    logins: BTreeMap<String, Login>,
    #[cfg(test)]
    fixture: Option<std::path::PathBuf>,
}
fn deadline(ticks: u64) -> Result<Instant, Error> {
    let start = Instant::now();
    let mut now = nix::libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // CLOCK_MONOTONIC is host-wide, unlike process-local Instant representation.
    if unsafe { nix::libc::clock_gettime(nix::libc::CLOCK_MONOTONIC, &mut now) } != 0 {
        return Err(Error::Changed);
    }
    let actual = u64::try_from(now.tv_sec)
        .ok()
        .and_then(|v| v.checked_mul(1_000_000_000))
        .and_then(|v| v.checked_add(u64::try_from(now.tv_nsec).ok()?))
        .ok_or(Error::Changed)?;
    let remaining = ticks
        .checked_sub(actual)
        .filter(|v| *v > 0 && *v <= 30_000_000_000)
        .ok_or(Error::Unauthorized)?;
    start
        .checked_add(Duration::from_nanos(remaining))
        .ok_or(Error::Changed)
}
impl Service {
    fn parent(&self, id: &str) -> Result<String, Error> {
        self.records
            .get(id)
            .map(|r| r.parent.clone())
            .ok_or(Error::Changed)
    }
    fn original(&self, id: &str) -> Result<String, Error> {
        let parent = self.parent(id)?;
        if parent != id {
            return Err(Error::Changed);
        }
        Ok(parent)
    }
    fn room(&mut self) -> Result<(), Error> {
        if self.records.len() < RECORDS {
            return Ok(());
        }
        let parent = self
            .records
            .values()
            .find_map(|record| {
                self.logins
                    .get_mut(&record.parent)
                    .is_some_and(Login::terminal)
                    .then(|| record.parent.clone())
            })
            .ok_or(Error::Changed)?;
        // A UUID group is one immutable attempt. Retaining an alias after its
        // parent ID became reusable would bind that old alias to new work.
        self.records.retain(|_, record| record.parent != parent);
        self.logins.remove(&parent);
        Ok(())
    }
    async fn command(
        &mut self,
        device: &str,
        raw: CommandJson,
        until: Option<u64>,
    ) -> Result<Reply<'static>, Error> {
        let registration = self.binding.acknowledgment();
        let command = self.binding.consume(
            self.binding_capability(),
            &registration,
            device,
            raw.0.as_bytes().to_vec(),
        )?;
        let id = command.operation_id().to_owned();
        let digest = command.nonsensitive_digest();
        if let Some(record) = self.records.get(&id) {
            if record.digest != digest {
                return Err(Error::Changed);
            }
            return Ok(Reply::Status {
                status: self
                    .logins
                    .get_mut(&record.parent)
                    .ok_or(Error::Changed)?
                    .snapshot(),
            });
        }
        self.room()?;
        let parent = match command.action() {
            Action::Connect => {
                let until = deadline(until.ok_or(Error::Unauthorized)?)?;
                let pending = self.binding.pending_login(&command)?;
                pending.limit_initial(until)?;
                #[cfg(test)]
                let attempt = pending.start_with(self.fixture.clone())?;
                #[cfg(not(test))]
                let attempt = pending.start()?;
                let status = attempt.snapshot();
                self.logins.insert(
                    id.clone(),
                    Login {
                        pending,
                        attempt: Some(attempt),
                        finished: None,
                        status,
                        released: false,
                    },
                );
                id.clone()
            }
            Action::Cancel { attempt_id } | Action::Submit { attempt_id, .. } => {
                if until.is_some() {
                    return Err(Error::InvalidCommand);
                }
                let parent = self.parent(attempt_id)?;
                let login = self.logins.get_mut(&parent).ok_or(Error::Changed)?;
                login
                    .attempt
                    .as_ref()
                    .ok_or(Error::Changed)?
                    .command(command)?;
                parent
            }
            Action::Disconnect => return Err(Error::InvalidCommand),
        };
        self.records.insert(
            id,
            Record {
                digest,
                parent: parent.clone(),
            },
        );
        Ok(Reply::Status {
            status: self.logins.get_mut(&parent).unwrap().snapshot(),
        })
    }
    // Command has already been authenticated from its socket envelope. Reusing
    // the enrolled capability internally does not expose or serialize it.
    fn binding_capability(&self) -> &str {
        self.binding.capability_for_service()
    }
    async fn stop(&mut self) -> bool {
        for login in self.logins.values_mut() {
            login.pending.revoke();
            if login.finished.take().is_some() {
                login.released = true;
            }
        }
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                let mut complete = true;
                for login in self.logins.values_mut() {
                    let s = login.snapshot();
                    if s.phase == LoginPhase::AwaitingPublication {
                        if let Some(attempt) = login.attempt.take() {
                            if attempt.finish().await.is_ok() {
                                login.released = true;
                            }
                        }
                    }
                    if matches!(
                        s.phase,
                        LoginPhase::Preparing | LoginPhase::Waiting | LoginPhase::Verifying
                    ) {
                        complete = false;
                    }
                }
                if complete {
                    return self.logins.values_mut().all(|l| l.terminal());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or(false)
    }
}
struct Limited(Zeroizing<Vec<u8>>);
impl std::io::Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len() + bytes.len() > REPLY {
            return Err(std::io::Error::other("reply exceeds bound"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
async fn send(socket: &mut UnixStream, reply: &Reply<'_>) -> Result<(), Error> {
    let mut bytes = Limited(Zeroizing::new(Vec::with_capacity(REPLY)));
    serde_json::to_writer(&mut bytes, reply).map_err(|_| Error::Changed)?;
    tokio::time::timeout(IO, async {
        socket
            .write_all(&(bytes.0.len() as u32).to_be_bytes())
            .await?;
        socket.write_all(&bytes.0).await?;
        socket.flush().await
    })
    .await
    .map_err(|_| Error::Changed)?
    .map_err(|_| Error::Changed)
}
async fn read(socket: &mut UnixStream) -> Result<Option<Request>, Error> {
    let mut length = [0; 4];
    let first = socket
        .read(&mut length[..1])
        .await
        .map_err(|_| Error::Changed)?;
    if first == 0 {
        return Ok(None);
    }
    tokio::time::timeout(IO, async {
        socket
            .read_exact(&mut length[1..])
            .await
            .map_err(|_| Error::Changed)?;
        let count = u32::from_be_bytes(length) as usize;
        if count == 0 || count > REQUEST {
            return Err(Error::InvalidCommand);
        }
        let mut bytes = Zeroizing::new(vec![0; count]);
        socket
            .read_exact(&mut bytes)
            .await
            .map_err(|_| Error::Changed)?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| Error::InvalidCommand)
    })
    .await
    .map_err(|_| Error::Changed)?
}
/// Only the hidden feature-gated binary entrypoint calls this. Both descriptors
/// are inherited; the startup pipe is consumed before any provider child exists.
pub async fn run(startup: OwnedFd, channel: OwnedFd) -> Result<(), Error> {
    run_inner(startup, channel, None).await
}
async fn run_inner(
    startup: OwnedFd,
    channel: OwnedFd,
    #[cfg_attr(not(test), allow(unused_variables))] fixture: Option<std::path::PathBuf>,
) -> Result<(), Error> {
    let binding = authority::read_control_startup(startup).await?;
    let stream = std::os::unix::net::UnixStream::from(channel);
    if !stream
        .peer_addr()
        .map_err(|_| Error::InvalidStartup)?
        .is_unnamed()
    {
        return Err(Error::InvalidStartup);
    }
    let flags = rustix::io::fcntl_getfd(&stream).map_err(|_| Error::InvalidStartup)?;
    rustix::io::fcntl_setfd(&stream, flags | rustix::io::FdFlags::CLOEXEC)
        .map_err(|_| Error::InvalidStartup)?;
    stream
        .set_nonblocking(true)
        .map_err(|_| Error::InvalidStartup)?;
    let mut socket = UnixStream::from_std(stream).map_err(|_| Error::InvalidStartup)?;
    #[cfg(test)]
    let test_root = fixture.is_none().then(tests::Root::new);
    #[cfg(test)]
    let fixture = fixture.or_else(|| test_root.as_ref().map(|root| root.0.clone()));
    #[cfg(test)]
    let root_path = fixture.clone();
    tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        if let Some(path) = root_path {
            return super::login_home::LoginHome::prepare_root_at(&path);
        }
        super::login_home::LoginHome::prepare_root()
    })
    .await
    .map_err(|_| Error::InvalidStartup)??;
    send(
        &mut socket,
        &Reply::Ready {
            registration: binding.acknowledgment(),
        },
    )
    .await?;
    let service = Service {
        binding,
        records: BTreeMap::new(),
        logins: BTreeMap::new(),
        #[cfg(test)]
        fixture,
    };
    serve(socket, service).await
}
async fn serve(mut socket: UnixStream, mut service: Service) -> Result<(), Error> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| Error::InvalidStartup)?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(|_| Error::InvalidStartup)?;
    let outcome = async {
        loop {
            let request = tokio::select! {
                result = read(&mut socket) => result?,
                _ = terminate.recv() => None,
                _ = interrupt.recv() => None,
            };
            let Some(request) = request else {
                return Ok(());
            };
            if !service.binding.authenticates(request.capability()) {
                send(
                    &mut socket,
                    &Reply::Refused {
                        error_code: "unauthorized",
                    },
                )
                .await?;
                continue;
            }
            match request {
                Request::Command {
                    device_id,
                    command,
                    lease_deadline_ns,
                    ..
                } => {
                    let response = service
                        .command(&device_id, command, lease_deadline_ns)
                        .await
                        .unwrap_or(Reply::Refused {
                            error_code: "command_refused",
                        });
                    send(&mut socket, &response).await?;
                }
                Request::Renew {
                    operation_id,
                    lease_deadline_ns,
                    ..
                } => {
                    let result: Result<Reply<'_>, Error> = (|| {
                        let parent = service.original(&operation_id)?;
                        let login = service.logins.get_mut(&parent).ok_or(Error::Changed)?;
                        login.pending.renew_until(deadline(lease_deadline_ns)?)?;
                        Ok(Reply::Status {
                            status: login.snapshot(),
                        })
                    })();
                    send(
                        &mut socket,
                        &result.unwrap_or(Reply::Refused {
                            error_code: "lease_refused",
                        }),
                    )
                    .await?;
                }
                Request::Status { operation_id, .. } => {
                    let result = service.parent(&operation_id).and_then(|parent| {
                        service
                            .logins
                            .get_mut(&parent)
                            .map(|l| Reply::Status {
                                status: l.snapshot(),
                            })
                            .ok_or(Error::Changed)
                    });
                    send(
                        &mut socket,
                        &result.unwrap_or(Reply::Refused {
                            error_code: "unavailable",
                        }),
                    )
                    .await?;
                }
                Request::Credential { operation_id, .. } => {
                    let extracted = async {
                        let parent = service.original(&operation_id)?;
                        let login = service.logins.get_mut(&parent).ok_or(Error::Changed)?;
                        if login.snapshot().phase != LoginPhase::AwaitingPublication {
                            return Err(Error::Changed);
                        }
                        if login.finished.is_none() {
                            login.finished = Some(
                                login
                                    .attempt
                                    .take()
                                    .ok_or(Error::Changed)?
                                    .finish()
                                    .await
                                    .map_err(|_| Error::Changed)?,
                            );
                        }
                        login
                            .finished
                            .as_ref()
                            .ok_or(Error::Changed)?
                            .credential()
                            .map_err(|_| Error::Changed)
                    }
                    .await;
                    match extracted {
                        Ok(bytes) => {
                            match serde_json::from_slice::<&serde_json::value::RawValue>(&bytes) {
                                Ok(credential) => {
                                    // Once frame publication starts, any transport failure
                                    // closes the socket; a second frame cannot repair it.
                                    send(
                                        &mut socket,
                                        &Reply::Credential {
                                            operation_id: &operation_id,
                                            credential,
                                        },
                                    )
                                    .await?;
                                }
                                Err(_) => {
                                    send(
                                        &mut socket,
                                        &Reply::Refused {
                                            error_code: "credential_unavailable",
                                        },
                                    )
                                    .await?
                                }
                            }
                        }
                        Err(_) => {
                            send(
                                &mut socket,
                                &Reply::Refused {
                                    error_code: "credential_unavailable",
                                },
                            )
                            .await?
                        }
                    }
                }
                Request::Release { operation_id, .. } => {
                    let result = service.original(&operation_id).and_then(|parent| {
                        let l = service.logins.get_mut(&parent).ok_or(Error::Changed)?;
                        if l.finished.is_none() {
                            return Err(Error::Changed);
                        }
                        l.finished.take();
                        l.released = true;
                        Ok(Reply::Released {
                            operation_id: &operation_id,
                        })
                    });
                    send(
                        &mut socket,
                        &result.unwrap_or(Reply::Refused {
                            error_code: "release_refused",
                        }),
                    )
                    .await?;
                }
                Request::Shutdown { .. } => {
                    let cleanup_confirmed = service.stop().await;
                    send(&mut socket, &Reply::Stopped { cleanup_confirmed }).await?;
                    return if cleanup_confirmed {
                        Ok(())
                    } else {
                        Err(Error::Changed)
                    };
                }
            }
        }
    }
    .await;
    let cleanup_confirmed = service.stop().await;
    outcome?;
    if cleanup_confirmed {
        Ok(())
    } else {
        Err(Error::Changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{io::Write, os::fd::AsRawFd};
    pub(super) struct Root(pub(super) std::path::PathBuf);
    impl Root {
        pub(super) fn new() -> Self {
            let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "chimaera-provider-helper-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            Self(path)
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    pub(super) const CAP: &str = "synthetic-private-capability-no-real-credential-000000";
    pub(super) fn registration() -> serde_json::Value {
        json!({"version":1,"account_id":"fixture","holder_id":"worker-fixture","process_boot":"00000000-0000-4000-8000-000000000001","registration_generation":7,"worker_credential_digest":"0".repeat(64)})
    }
    async fn fixture() -> (UnixStream, tokio::task::JoinHandle<Result<(), Error>>) {
        fixture_at(None).await
    }
    async fn fixture_at(
        root: Option<std::path::PathBuf>,
    ) -> (UnixStream, tokio::task::JoinHandle<Result<(), Error>>) {
        let (read, write) = nix::unistd::pipe().unwrap();
        let mut startup = registration();
        startup["capability"] = json!(CAP);
        std::fs::File::from(write)
            .write_all(&serde_json::to_vec(&startup).unwrap())
            .unwrap();
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        client.set_nonblocking(true).unwrap();
        let client = UnixStream::from_std(client).unwrap();
        (client, tokio::spawn(run_inner(read, server.into(), root)))
    }
    pub(super) async fn reply(socket: &mut UnixStream) -> serde_json::Value {
        let size = socket.read_u32().await.unwrap() as usize;
        assert!(size <= REPLY);
        let mut bytes = vec![0; size];
        socket.read_exact(&mut bytes).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
    pub(super) async fn request(
        socket: &mut UnixStream,
        value: serde_json::Value,
    ) -> serde_json::Value {
        let bytes = serde_json::to_vec(&value).unwrap();
        socket.write_u32(bytes.len() as u32).await.unwrap();
        socket.write_all(&bytes).await.unwrap();
        reply(socket).await
    }
    #[tokio::test]
    async fn unsafe_login_root_refuses_before_ready() {
        use std::os::unix::fs::PermissionsExt;
        let root = Root::new();
        std::fs::create_dir(&root.0).unwrap();
        std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (mut socket, task) = fixture_at(Some(root.0.clone())).await;
        assert!(matches!(task.await.unwrap(), Err(Error::InvalidStartup)));
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
        assert_eq!(
            std::fs::metadata(&root.0).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
    #[tokio::test]
    async fn exact_startup_ack_capability_closed_frames_and_shutdown() {
        let (mut socket, task) = fixture().await;
        assert_eq!(
            reply(&mut socket).await,
            json!({"type":"ready","registration":registration()})
        );
        let response=request(&mut socket,json!({"type":"status","capability":"wrong","operation_id":"00000000-0000-4000-8000-000000000003"})).await;
        assert_eq!(
            response,
            json!({"type":"refused","error_code":"unauthorized"})
        );
        let response = request(&mut socket, json!({"type":"shutdown","capability":CAP})).await;
        assert_eq!(response, json!({"type":"stopped","cleanup_confirmed":true}));
        assert!(task.await.unwrap().is_ok());
        let raw = json!({"type":"status","capability":CAP,"operation_id":"x","selected_home":"synthetic-secret-marker"});
        assert!(serde_json::from_value::<Request>(raw).is_err());
        assert!(serde_json::from_str::<Request>(&format!(
            "{{\"type\":\"shutdown\",\"capability\":\"{CAP}\",\"lease_deadline_ns\":null}}"
        ))
        .is_err());
    }
    #[tokio::test]
    async fn socket_is_close_on_exec_and_eof_stops_empty_helper() {
        let (read, write) = nix::unistd::pipe().unwrap();
        let mut startup = registration();
        startup["capability"] = json!(CAP);
        std::fs::File::from(write)
            .write_all(&serde_json::to_vec(&startup).unwrap())
            .unwrap();
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let duplicate = server.try_clone().unwrap();
        let fd = server.as_raw_fd();
        client.set_nonblocking(true).unwrap();
        let mut client = UnixStream::from_std(client).unwrap();
        let task = tokio::spawn(run(read, server.into()));
        reply(&mut client).await;
        // Duplicate socket remains a separate owner; verify flags on the exact
        // consumed descriptor while the helper is known alive at its read.
        let flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFD) };
        assert!(flags >= 0 && flags & nix::libc::FD_CLOEXEC != 0);
        drop(duplicate);
        drop(client);
        assert!(tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_ok());
    }
    fn service() -> Service {
        let mut startup = registration();
        startup["capability"] = json!(CAP);
        Service {
            binding: ControlBinding::parse(&serde_json::to_vec(&startup).unwrap()).unwrap(),
            records: BTreeMap::new(),
            logins: BTreeMap::new(),
            fixture: None,
        }
    }
    fn synthetic_login(service: &Service, id: &str, phase: LoginPhase, released: bool) -> Login {
        let registration = service.binding.acknowledgment();
        let command = service
            .binding
            .consume(
                CAP,
                &registration,
                "device-one",
                serde_json::to_vec(&json!({
            "version":1,"operation_id":id,"provider":"claude","expected_connection_generation":0,
            "command":{"type":"connect"}}))
                .unwrap(),
            )
            .unwrap();
        Login {
            pending: service.binding.pending_login(&command).unwrap(),
            attempt: None,
            finished: None,
            status: LoginStatus {
                attempt_id: id.into(),
                provider: super::super::authority::Provider::Claude,
                phase,
                action: None,
                error_code: None,
            },
            released,
        }
    }
    #[test]
    fn pressure_retires_whole_terminal_group_before_original_uuid_reuse() {
        let mut service = service();
        let parent = "00000000-0000-4000-8000-000000000001";
        service.logins.insert(
            parent.into(),
            synthetic_login(&service, parent, LoginPhase::Canceled, true),
        );
        let live = "00000000-0000-4000-8000-000000000024";
        service.logins.insert(
            live.into(),
            synthetic_login(&service, live, LoginPhase::Waiting, false),
        );
        for index in 1..=24 {
            let id = format!("00000000-0000-4000-8000-{index:012}");
            service.records.insert(
                id,
                Record {
                    digest: [0; 32],
                    parent: if index == 24 { live } else { parent }.into(),
                },
            );
        }
        service.room().unwrap();
        assert_eq!(service.records.len(), 1);
        assert!(!service.logins.contains_key(parent));
        service.logins.insert(
            parent.into(),
            synthetic_login(&service, parent, LoginPhase::Waiting, false),
        );
        service.records.insert(
            parent.into(),
            Record {
                digest: [1; 32],
                parent: parent.into(),
            },
        );
        assert!(service
            .parent("00000000-0000-4000-8000-000000000002")
            .is_err());
        assert_eq!(service.parent(live).unwrap(), live);
        // Unknown cleanup is never an eviction/UUID-reuse receipt.
        service.records.clear();
        service.logins.clear();
        service.logins.insert(
            parent.into(),
            synthetic_login(&service, parent, LoginPhase::CleanupRequired, false),
        );
        for index in 1..=24 {
            service.records.insert(
                format!("00000000-0000-4000-8000-{index:012}"),
                Record {
                    digest: [0; 32],
                    parent: parent.into(),
                },
            );
        }
        assert!(service.room().is_err());
        assert_eq!(service.records.len(), 24);
    }
    #[test]
    fn command_requires_explicit_nullable_lease_field() {
        let body=format!("{{\"type\":\"command\",\"capability\":\"{CAP}\",\"device_id\":\"device-one\",\"command\":{{}}}}");
        assert!(serde_json::from_str::<Request>(&body).is_err());
        let body = body.strip_suffix('}').unwrap().to_owned() + ",\"lease_deadline_ns\":null}";
        assert!(serde_json::from_str::<Request>(&body).is_ok());
    }
    #[tokio::test]
    async fn unconfirmed_cleanup_is_a_failed_exit_on_shutdown_and_eof() {
        for explicit in [true, false] {
            let mut service = service();
            let id = "00000000-0000-4000-8000-000000000001";
            service.logins.insert(
                id.into(),
                synthetic_login(&service, id, LoginPhase::CleanupRequired, false),
            );
            // Synthetic cleanup-required state has no process or credential.
            let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
            client.set_nonblocking(true).unwrap();
            server.set_nonblocking(true).unwrap();
            let mut client = UnixStream::from_std(client).unwrap();
            let task = tokio::spawn(serve(UnixStream::from_std(server).unwrap(), service));
            if explicit {
                assert_eq!(
                    request(&mut client, json!({"type":"shutdown","capability":CAP})).await
                        ["cleanup_confirmed"],
                    false
                );
            }
            drop(client);
            assert_eq!(task.await.unwrap(), Err(Error::Changed));
        }
    }
    #[test]
    fn monotonic_transfer_refuses_expired_overlong_and_overflow_deadlines() {
        assert!(deadline(0).is_err());
        assert!(deadline(u64::MAX).is_err());
    }
}

#[cfg(test)]
mod login_socket_tests {
    use super::{
        tests::{registration, reply, request, CAP},
        *,
    };
    use serde_json::json;
    use std::{
        io::Write,
        os::{fd::AsRawFd, unix::fs::PermissionsExt},
    };
    #[tokio::test]
    async fn expired_transferred_lease_stops_cli_and_refuses_late_renewal_or_leaf() {
        let _serial = super::super::control_login::TEST_SERIAL.lock().await;
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "chimaera-helper-expiry-{}",
            crate::agents::fresh_session_id()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(root.join("fixture-script"),
            "printf 'https://claude.com/cai/oauth/authorize?state=fixture\\nPaste code here\\n'; read code").unwrap();
        let (read, write) = nix::unistd::pipe().unwrap();
        let mut startup = registration();
        startup["capability"] = json!(CAP);
        std::fs::File::from(write)
            .write_all(&serde_json::to_vec(&startup).unwrap())
            .unwrap();
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        client.set_nonblocking(true).unwrap();
        let mut client = UnixStream::from_std(client).unwrap();
        let task = tokio::spawn(run_inner(read, server.into(), Some(root.clone())));
        reply(&mut client).await;
        let mut now = nix::libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        assert_eq!(
            unsafe { nix::libc::clock_gettime(nix::libc::CLOCK_MONOTONIC, &mut now) },
            0
        );
        let ticks = now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64;
        let op = "00000000-0000-4000-8000-000000000007";
        let command = json!({"type":"command","capability":CAP,"device_id":"device-one",
            "command":{"version":1,"operation_id":op,"provider":"claude",
                "expected_connection_generation":0,"command":{"type":"connect"}},
            "lease_deadline_ns":ticks+500_000_000});
        assert_eq!(
            request(&mut client, command.clone()).await["type"],
            "status"
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let status = request(
                    &mut client,
                    json!({"type":"status","capability":CAP,"operation_id":op}),
                )
                .await;
                if status["status"]["phase"] == "canceled" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(!root.join("claude").join(op).exists());
        assert_eq!(
            request(
                &mut client,
                json!({"type":"renew","capability":CAP,
            "operation_id":op,"lease_deadline_ns":ticks+20_000_000_000})
            )
            .await["error_code"],
            "lease_refused"
        );
        assert_eq!(
            request(
                &mut client,
                json!({"type":"credential","capability":CAP,
            "operation_id":op})
            )
            .await["error_code"],
            "credential_unavailable"
        );
        assert_eq!(
            request(&mut client, command).await["status"]["phase"],
            "canceled"
        );
        assert_eq!(
            request(&mut client, json!({"type":"shutdown","capability":CAP})).await
                ["cleanup_confirmed"],
            true
        );
        assert!(task.await.unwrap().is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn synthetic_cli_socket_flow_transfers_deadline_cleans_before_leaf_and_consumes_code_once(
    ) {
        let _serial = super::super::control_login::TEST_SERIAL.lock().await;
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "chimaera-helper-login-{}",
            crate::agents::fresh_session_id()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (read, write) = nix::unistd::pipe().unwrap();
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let fd = server.as_raw_fd();
        // This is a fixture-owned shell, never a vendor executable or a global
        // HOME/PATH override. The real public runner owns its group and home.
        let script=format!("test ! -e /dev/fd/{fd} || exit 17; printf 'https://claude.com/cai/oauth/authorize?state=fixture\\nPaste code here\\n'; read code; mkdir -p .claude; printf '%s' '{{\"claudeAiOauth\":{{\"accessToken\":\"synthetic-access\",\"refreshToken\":\"synthetic-refresh\",\"expiresAt\":2000000000000,\"scopes\":[\"user:inference\"],\"subscriptionType\":\"max\",\"rateLimitTier\":\"default\"}}}}' > .claude/.credentials.json; printf '%s' '{{\"oauthAccount\":{{\"accountUuid\":\"user-one\",\"organizationUuid\":\"org-one\"}}}}' > .claude.json; exit 0");
        std::fs::write(root.join("fixture-script"), script).unwrap();
        let mut startup = registration();
        startup["capability"] = json!(CAP);
        std::fs::File::from(write)
            .write_all(&serde_json::to_vec(&startup).unwrap())
            .unwrap();
        client.set_nonblocking(true).unwrap();
        let mut client = UnixStream::from_std(client).unwrap();
        let task = tokio::spawn(run_inner(read, server.into(), Some(root.clone())));
        reply(&mut client).await;
        let mut now = nix::libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        assert_eq!(
            unsafe { nix::libc::clock_gettime(nix::libc::CLOCK_MONOTONIC, &mut now) },
            0
        );
        let ticks = now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64 + 20_000_000_000;
        let op = "00000000-0000-4000-8000-000000000004";
        let command = json!({"version":1,"operation_id":op,"provider":"claude","expected_connection_generation":0,"command":{"type":"connect"}});
        let original = json!({"type":"command","capability":CAP,"device_id":"device-one","command":command,"lease_deadline_ns":ticks});
        assert_eq!(
            request(&mut client, original.clone()).await["status"]["attempt_id"],
            op
        );
        let status_request = json!({"type":"status","capability":CAP,"operation_id":op});
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let s = request(&mut client, status_request.clone()).await;
                if s["status"]["phase"] == "waiting" {
                    assert!(!s.to_string().contains("synthetic-access"));
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let submit = json!({"type":"command","capability":CAP,"device_id":"device-one","command":{"version":1,"operation_id":"00000000-0000-4000-8000-000000000005","provider":"claude","expected_connection_generation":0,"command":{"type":"submit","attempt_id":op,"submission_nonce":"00000000-0000-4000-8000-000000000006","code":"synthetic-code#state"}},"lease_deadline_ns":null});
        let response = request(&mut client, submit.clone()).await;
        assert_eq!(response["type"], "status");
        let mut retry = submit;
        retry["command"]["command"]["code"] = json!("changed-code#state");
        assert_eq!(request(&mut client, retry).await["type"], "status");
        assert_eq!(
            request(
                &mut client,
                json!({"type":"renew","capability":CAP,
            "operation_id":"00000000-0000-4000-8000-000000000005",
            "lease_deadline_ns":ticks})
            )
            .await["error_code"],
            "lease_refused"
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let s = request(&mut client, status_request.clone()).await;
                if s["status"]["phase"] == "awaiting_publication" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            !root.join("claude").join(op).exists(),
            "credential extraction follows positive home cleanup"
        );
        let result = request(
            &mut client,
            json!({"type":"credential","capability":CAP,"operation_id":op}),
        )
        .await;
        assert_eq!(result["credential"]["access"], "synthetic-access");
        assert_eq!(result["credential"]["identity"]["workspace"], "org-one");
        assert_eq!(
            request(
                &mut client,
                json!({"type":"release","capability":CAP,"operation_id":op})
            )
            .await["type"],
            "released"
        );
        assert_eq!(
            request(&mut client, original).await["type"],
            "status",
            "same UUID never starts another CLI"
        );
        assert_eq!(
            request(&mut client, json!({"type":"shutdown","capability":CAP})).await
                ["cleanup_confirmed"],
            true
        );
        assert!(task.await.unwrap().is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
}
