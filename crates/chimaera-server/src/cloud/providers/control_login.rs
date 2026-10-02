//! Default-off fixed official login runner. The private coordinator renews its
//! pending lease and separately obtains fresh authorization for canonical import.
use super::{
    authority::{
        Action, ControlBinding, ControlCommand, Credential, Error, LoginHome, Provider,
        Registration,
    },
    connect, process,
};
use serde::Serialize;
use serde_json::json;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot, watch},
};
use zeroize::Zeroizing;

const LEASE: Duration = Duration::from_secs(30);
const ATTEMPT: Duration = Duration::from_secs(900);
#[cfg(test)]
pub(super) static TEST_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static WRITERS: [AtomicBool; 3] = [
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
];
fn slot(p: Provider) -> usize {
    match p {
        Provider::Claude => 0,
        Provider::Codex => 1,
        Provider::Github => 2,
    }
}
struct Reservation {
    slot: usize,
    release: bool,
}
impl Reservation {
    fn take(provider: Provider) -> Result<Self, Error> {
        let slot = slot(provider);
        WRITERS[slot]
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::Changed)?;
        Ok(Self {
            slot,
            release: true,
        })
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if self.release {
            WRITERS[self.slot].store(false, Ordering::Release);
        }
    }
}
struct LeaseState {
    deadline: Instant,
    canceled: bool,
}
struct Pending {
    registration: Registration,
    device: String,
    operation: String,
    provider: Provider,
    generation: u64,
    started: AtomicBool,
    absolute: Instant,
    state: Mutex<LeaseState>,
    changes: watch::Sender<u64>,
}
/// Opaque pending-login authorization, never a runtime/project attachment.
/// Only the enrolled personal adapter obtains it; renewal follows its fresh
/// keeper lease exchange, not an observer's HTTP/socket lifetime.
#[derive(Clone)]
pub struct PendingLogin(Arc<Pending>);
impl ControlBinding {
    pub fn pending_login(&self, command: &ControlCommand) -> Result<PendingLogin, Error> {
        if !self.agrees(command) || !matches!(command.action(), Action::Connect) {
            return Err(Error::Unauthorized);
        }
        let now = Instant::now();
        let (changes, _) = watch::channel(0);
        Ok(PendingLogin(Arc::new(Pending {
            registration: command.registration().clone(),
            device: command.authenticated_device().into(),
            operation: command.operation_id().into(),
            provider: command.provider(),
            generation: command.expected_connection_generation(),
            started: AtomicBool::new(false),
            absolute: now + ATTEMPT,
            state: Mutex::new(LeaseState {
                deadline: now + LEASE,
                canceled: false,
            }),
            changes,
        })))
    }
}
impl PendingLogin {
    /// Same-host private supervisor transfer of an already validated keeper
    /// deadline. Reception/renewal never begins another authorization period.
    pub(super) fn limit_initial(&self, deadline: Instant) -> Result<(), Error> {
        let now = Instant::now();
        if deadline <= now || deadline > now + LEASE {
            return Err(Error::Unauthorized);
        }
        let mut state = crate::lock(&self.0.state);
        if self.0.started.load(Ordering::Acquire) || state.canceled || now >= state.deadline {
            return Err(Error::Changed);
        }
        state.deadline = state.deadline.min(deadline);
        Ok(())
    }
    pub(super) fn renew_until(&self, deadline: Instant) -> Result<(), Error> {
        self.check()?;
        let now = Instant::now();
        if deadline <= now || deadline > now + LEASE {
            return Err(Error::Unauthorized);
        }
        let mut state = crate::lock(&self.0.state);
        if state.canceled || now >= state.deadline || now >= self.0.absolute {
            return Err(Error::Changed);
        }
        state.deadline = deadline.min(self.0.absolute);
        self.0.changes.send_modify(|v| *v = v.wrapping_add(1));
        Ok(())
    }
    fn matches(&self, command: &ControlCommand) -> bool {
        self.0.registration == *command.registration()
            && self.0.device == command.authenticated_device()
            && self.0.provider == command.provider()
            && self.0.generation == command.expected_connection_generation()
    }
    pub fn renew(&self, binding: &ControlBinding, command: &ControlCommand) -> Result<(), Error> {
        if !binding.agrees(command)
            || !self.matches(command)
            || command.operation_id() != self.0.operation
            || !matches!(command.action(), Action::Connect)
        {
            return Err(Error::Unauthorized);
        }
        let mut state = crate::lock(&self.0.state);
        if state.canceled || Instant::now() >= state.deadline || Instant::now() >= self.0.absolute {
            return Err(Error::Changed);
        }
        state.deadline = (Instant::now() + LEASE).min(self.0.absolute);
        self.0
            .changes
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        Ok(())
    }
    /// The trusted coordinator cancels on device/account/worker/registration
    /// revocation; dropping a browser observation never invokes this.
    pub fn revoke(&self) {
        crate::lock(&self.0.state).canceled = true;
        self.0
            .changes
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
    pub fn check(&self) -> Result<(), Error> {
        let state = crate::lock(&self.0.state);
        if state.canceled || Instant::now() >= state.deadline || Instant::now() >= self.0.absolute {
            Err(Error::Changed)
        } else {
            Ok(())
        }
    }
    async fn canceled(&self) {
        let mut changed = self.0.changes.subscribe();
        loop {
            let deadline = {
                let state = crate::lock(&self.0.state);
                if state.canceled {
                    return;
                }
                state.deadline.min(self.0.absolute)
            };
            tokio::select! { _ = tokio::time::sleep_until(deadline.into()) => if self.check().is_err() { return; }, value = changed.changed() => if value.is_err() { return; } }
        }
    }
}
#[derive(Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoginPhase {
    Preparing,
    Waiting,
    Verifying,
    AwaitingPublication,
    Failed,
    Canceled,
    CleanupRequired,
}
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LoginAction {
    Browser {
        url: String,
        input: &'static str,
    },
    DeviceCode {
        verification_url: String,
        user_code: String,
    },
}
#[derive(Clone, Serialize)]
pub struct LoginStatus {
    pub attempt_id: String,
    pub provider: Provider,
    pub phase: LoginPhase,
    pub action: Option<LoginAction>,
    pub error_code: Option<&'static str>,
}
struct Input {
    nonce: Option<String>,
    sender: Option<mpsc::Sender<Zeroizing<String>>>,
}
pub struct LoginAttempt {
    pending: PendingLogin,
    status: watch::Receiver<LoginStatus>,
    input: Arc<Mutex<Input>>,
    completion: oneshot::Receiver<Result<FinishedLogin, &'static str>>,
}
/// Supervisor-only completion; no status/Debug/Serialize implementation. Retain
/// it through the private fresh-publication exchange and durable generation CAS.
struct Completion {
    credential: Mutex<Option<Credential>>,
    reservation: Mutex<Option<Reservation>>,
}
pub struct FinishedLogin {
    completion: Arc<Completion>,
    pending: PendingLogin,
}
impl FinishedLogin {
    /// Bounded zeroizing bytes for the supervisor-private importer only. Never
    /// retain a mutex guard while waiting for the fresh keeper exchange.
    pub fn credential(&self) -> Result<Zeroizing<Vec<u8>>, Error> {
        self.pending.check()?;
        let credential = crate::lock(&self.completion.credential);
        let value = credential.as_ref().ok_or(Error::Changed)?;
        struct Limited(Zeroizing<Vec<u8>>);
        impl std::io::Write for Limited {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.0.len() + bytes.len() > 128 * 1024 {
                    return Err(std::io::Error::other("Provider credential exceeds bound"));
                }
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut bytes = Limited(Zeroizing::new(Vec::with_capacity(128 * 1024)));
        serde_json::to_writer(&mut bytes, value).map_err(|_| Error::InvalidCommand)?;
        self.pending.check()?;
        Ok(bytes.0)
    }
    pub fn admission(&self) -> &PendingLogin {
        &self.pending
    }
}
impl Drop for FinishedLogin {
    fn drop(&mut self) {
        crate::lock(&self.completion.credential).take();
        crate::lock(&self.completion.reservation).take();
    }
}
impl LoginAttempt {
    pub fn snapshot(&self) -> LoginStatus {
        self.status.borrow().clone()
    }
    pub async fn changed(&mut self) -> Result<LoginStatus, Error> {
        self.status.changed().await.map_err(|_| Error::Changed)?;
        Ok(self.snapshot())
    }
    pub fn command(&self, command: ControlCommand) -> Result<(), Error> {
        if !self.pending.matches(&command) {
            return Err(Error::Unauthorized);
        }
        self.pending.check()?;
        match command.into_action() {
            Action::Cancel { attempt_id } if attempt_id == self.pending.0.operation => {
                self.pending.revoke();
                Ok(())
            }
            Action::Submit {
                attempt_id,
                submission_nonce,
                code,
            } if attempt_id == self.pending.0.operation
                && self.pending.0.provider == Provider::Claude =>
            {
                let mut input = crate::lock(&self.input);
                if let Some(nonce) = &input.nonce {
                    return if nonce == &submission_nonce {
                        Ok(())
                    } else {
                        Err(Error::Changed)
                    };
                }
                if self.snapshot().phase != LoginPhase::Waiting {
                    return Err(Error::Changed);
                }
                input
                    .sender
                    .as_ref()
                    .ok_or(Error::Changed)?
                    .try_send(code)
                    .map_err(|_| Error::Changed)?;
                input.nonce = Some(submission_nonce);
                Ok(())
            }
            _ => Err(Error::InvalidCommand),
        }
    }
    pub async fn finish(self) -> Result<FinishedLogin, &'static str> {
        self.completion.await.map_err(|_| "login_unavailable")?
    }
}
impl PendingLogin {
    pub fn start(&self) -> Result<LoginAttempt, Error> {
        self.start_with(None)
    }
    pub(super) fn start_with(
        &self,
        #[cfg_attr(not(test), allow(unused_variables))] test: Option<std::path::PathBuf>,
    ) -> Result<LoginAttempt, Error> {
        self.check()?;
        self.0
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::Changed)?;
        let reservation = Reservation::take(self.0.provider)?;
        let initial = LoginStatus {
            attempt_id: self.0.operation.clone(),
            provider: self.0.provider,
            phase: LoginPhase::Preparing,
            action: None,
            error_code: None,
        };
        let (status, observer) = watch::channel(initial);
        let (send, receiver) = mpsc::channel(1);
        let input = Arc::new(Mutex::new(Input {
            nonce: None,
            sender: Some(send),
        }));
        let (result, completion) = oneshot::channel();
        let pending = self.clone();
        let owned_input = input.clone();
        tokio::spawn(async move {
            let home_owner = pending.clone();
            let prepared = tokio::task::spawn_blocking(move || {
                #[cfg(test)]
                if let Some(path) = test {
                    return LoginHome::prepare_for_pending(
                        &path,
                        &home_owner.0.operation,
                        home_owner.0.provider,
                    );
                }
                LoginHome::prepare_for_pending(
                    std::path::Path::new("/state/provider-login"),
                    &home_owner.0.operation,
                    home_owner.0.provider,
                )
            })
            .await;
            let outcome = match prepared {
                Ok(Ok(home)) => run(home, pending.clone(), &status, receiver, reservation).await,
                _ => Err("login_home_unavailable"),
            };
            crate::lock(&owned_input).sender.take();
            if let Err(error) = &outcome {
                status.send_modify(|s| {
                    s.action = None;
                    s.error_code = Some(error);
                    s.phase = if *error == "cleanup_failed" {
                        LoginPhase::CleanupRequired
                    } else if pending.check().is_err() {
                        LoginPhase::Canceled
                    } else {
                        LoginPhase::Failed
                    };
                });
            }
            // Observer cancellation drops only its receiver. Owned cleanup and
            // any credential result finish here; never release an active writer.
            let _ = result.send(outcome);
        });
        Ok(LoginAttempt {
            pending: self.clone(),
            status: observer,
            input,
            completion,
        })
    }
}
fn update(status: &watch::Sender<LoginStatus>, phase: LoginPhase, action: Option<LoginAction>) {
    status.send_modify(|s| {
        s.phase = phase;
        s.action = action;
    });
}
async fn cleanup_home(home: Arc<LoginHome>) -> Result<(), &'static str> {
    let home = Arc::try_unwrap(home).map_err(|_| "cleanup_failed")?;
    match tokio::task::spawn_blocking(move || home.cleanup()).await {
        Ok(Ok(())) => Ok(()),
        _ => Err("cleanup_failed"),
    }
}
async fn run(
    home: LoginHome,
    pending: PendingLogin,
    status: &watch::Sender<LoginStatus>,
    input: mpsc::Receiver<Zeroizing<String>>,
    mut reservation: Reservation,
) -> Result<FinishedLogin, &'static str> {
    let home = Arc::new(home);
    let prepare = home.clone();
    let command = tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        if let Some(command) = prepare.fixture_command() {
            return Ok(command);
        }
        prepare.official_login_command()
    })
    .await;
    let flow = match command {
        Ok(Ok(command)) if pending.check().is_ok() => {
            process_login(command, &pending, status, input).await
        }
        _ => Ok(Err("login_home_unavailable")),
    };
    let flow = match flow {
        Ok(result) => result,
        // A failed process receipt must retain its home and writer fence. No
        // credential is read, and a new refresh owner cannot replace this one.
        Err(_) => {
            reservation.release = false;
            return Err("cleanup_failed");
        }
    };
    let extracted = if flow.is_ok() && pending.check().is_ok() {
        update(status, LoginPhase::Verifying, None);
        if pending.0.provider == Provider::Github {
            github(&home, &pending).await
        } else {
            let provider = pending.0.provider;
            let work = home.clone();
            tokio::task::spawn_blocking(move || match provider {
                Provider::Claude => work.claude_leaf(),
                Provider::Codex => work.codex_leaf(),
                _ => unreachable!(),
            })
            .await
            .unwrap_or(Err(Error::InvalidCommand))
            .map_err(|_| "credential_unavailable")
        }
    } else {
        Err(flow.err().unwrap_or("canceled"))
    };
    if extracted
        .as_ref()
        .is_err_and(|error| *error == "cleanup_failed")
    {
        reservation.release = false;
        return Err("cleanup_failed");
    }
    if cleanup_home(home).await.is_err() {
        reservation.release = false;
        return Err("cleanup_failed");
    }
    let credential = extracted?;
    pending.check().map_err(|_| "canceled")?;
    update(status, LoginPhase::AwaitingPublication, None);
    let completion = Arc::new(Completion {
        credential: Mutex::new(Some(credential)),
        reservation: Mutex::new(Some(reservation)),
    });
    let retained = completion.clone();
    let expiry = pending.clone();
    tokio::spawn(async move {
        expiry.canceled().await;
        crate::lock(&retained.credential).take();
        crate::lock(&retained.reservation).take();
    });
    Ok(FinishedLogin {
        completion,
        pending,
    })
}
async fn process_login(
    command: tokio::process::Command,
    pending: &PendingLogin,
    status: &watch::Sender<LoginStatus>,
    input: mpsc::Receiver<Zeroizing<String>>,
) -> Result<Result<(), &'static str>, &'static str> {
    if pending.0.provider == Provider::Codex {
        let mut rpc = match process::Rpc::spawn_command(command) {
            Ok(rpc) => rpc,
            Err(error) => return Ok(Err(error)),
        };
        let pid = rpc.process_id();
        let result = tokio::select! { biased; _ = pending.canceled() => Err("canceled"), result = codex(&mut rpc, status) => result };
        rpc.terminate(pid).await?;
        Ok(result)
    } else {
        let mut command = command;
        let mut child = match process::Child::spawn(&mut command) {
            Ok(child) => child,
            Err(error) => return Ok(Err(error)),
        };
        let pid = child.child.id();
        let result = tokio::select! { biased; _ = pending.canceled() => Err("canceled"), result = stream(&mut child, pending.0.provider, status, input) => result };
        child.terminate(pid).await?;
        Ok(result)
    }
}
async fn codex(
    rpc: &mut process::Rpc,
    status: &watch::Sender<LoginStatus>,
) -> Result<(), &'static str> {
    rpc.initialize().await?;
    let response = rpc
        .request("account/login/start", json!({"type":"chatgptDeviceCode"}))
        .await?;
    let (login, action) = connect::device_action(&response)?;
    let connect::Action::DeviceCode {
        verification_url,
        user_code,
    } = action
    else {
        return Err("unsupported_login");
    };
    update(
        status,
        LoginPhase::Waiting,
        Some(LoginAction::DeviceCode {
            verification_url,
            user_code,
        }),
    );
    for _ in 0..1024 {
        let msg = rpc.next().await?;
        if msg["method"] == "account/login/completed"
            && msg["params"]["loginId"].as_str() == Some(&login)
        {
            return if msg["params"]["success"] == true {
                Ok(())
            } else {
                Err("sign_in_failed")
            };
        }
    }
    Err("output_limit")
}
async fn stream(
    child: &mut process::Child,
    provider: Provider,
    status: &watch::Sender<LoginStatus>,
    mut input: mpsc::Receiver<Zeroizing<String>>,
) -> Result<(), &'static str> {
    let mut stdin = child.child.stdin.take().ok_or("start_failed")?;
    let mut stdout = child.child.stdout.take().ok_or("start_failed")?;
    let mut stderr = child.child.stderr.take().ok_or("start_failed")?;
    let mut out = Zeroizing::new([0u8; 2048]);
    let mut err = Zeroizing::new([0u8; 2048]);
    let mut captured = Zeroizing::new(Vec::with_capacity(process::LIMIT));
    let mut total = 0usize;
    let mut action = false;
    let mut answered = false;
    let mut out_open = true;
    let mut err_open = true;
    loop {
        tokio::select! {
            exit = child.wait() => return if exit.map_err(|_| "sign_in_failed")?.success() && status.borrow().phase != LoginPhase::Preparing { Ok(()) } else { Err("sign_in_failed") },
            code = input.recv(), if provider == Provider::Claude && action && status.borrow().phase == LoginPhase::Waiting => {
                let code = code.ok_or("canceled")?;
                let mut bytes = Zeroizing::new(Vec::with_capacity(4097)); bytes.extend_from_slice(code.as_bytes()); bytes.push(b'\n');
                update(status, LoginPhase::Verifying, None);
                tokio::time::timeout(Duration::from_secs(3), async { stdin.write_all(&bytes).await?; stdin.flush().await }).await.map_err(|_| "sign_in_failed")?.map_err(|_| "sign_in_failed")?;
                // A successful one-use input never reopens an input channel.
                input.close(); action = false;
            },
            result = stdout.read(&mut out[..]), if out_open => { let n = result.map_err(|_| "connection_closed")?; out_open = n > 0; total = total.saturating_add(n); if total > process::LIMIT { return Err("output_limit"); }
                if !action { captured.extend_from_slice(&out[..n]); } },
            result = stderr.read(&mut err[..]), if err_open => { let n = result.map_err(|_| "connection_closed")?; err_open = n > 0; total = total.saturating_add(n); if total > process::LIMIT { return Err("output_limit"); }
                if !action { captured.extend_from_slice(&err[..n]); } },
        }
        if !action && status.borrow().phase == LoginPhase::Preparing {
            if provider == Provider::Claude
                && captured
                    .windows(b"Paste code here".len())
                    .any(|s| s == b"Paste code here")
            {
                let url = connect::claude::authorization_url(&captured)
                    .ok_or("browser_login_unavailable")?;
                update(
                    status,
                    LoginPhase::Waiting,
                    Some(LoginAction::Browser {
                        url,
                        input: "authorization_code",
                    }),
                );
                action = true;
                captured.clear();
            } else if provider == Provider::Github {
                let prompt = connect::github::prompt(&captured);
                if prompt.enter && !answered {
                    answered = true;
                    stdin.write_all(b"\n").await.map_err(|_| "sign_in_failed")?;
                }
                if let Some(code) = prompt.code {
                    let url = prompt
                        .page
                        .unwrap_or_else(|| "https://github.com/login/device".into());
                    update(
                        status,
                        LoginPhase::Waiting,
                        Some(LoginAction::DeviceCode {
                            verification_url: url,
                            user_code: code,
                        }),
                    );
                    action = true;
                    captured.clear();
                }
            }
        }
    }
}
async fn github(home: &Arc<LoginHome>, pending: &PendingLogin) -> Result<Credential, &'static str> {
    let mut outputs = Vec::with_capacity(2);
    for token in [true, false] {
        pending.check().map_err(|_| "canceled")?;
        let work = home.clone();
        let mut cmd = tokio::task::spawn_blocking(move || work.github_probe(token))
            .await
            .unwrap_or(Err(Error::InvalidCommand))
            .map_err(|_| "credential_unavailable")?;
        pending.check().map_err(|_| "canceled")?;
        let mut child = process::Child::spawn(&mut cmd)?;
        let pid = child.child.id();
        let out = child.child.stdout.take().ok_or("start_failed")?;
        let err = child.child.stderr.take().ok_or("start_failed")?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(process::LIMIT + 1));
        let mut discarded = Zeroizing::new(Vec::with_capacity(process::LIMIT + 1));
        let result = tokio::select! { biased; _ = pending.canceled() => Err("canceled"),
            result = tokio::time::timeout(process::TIMEOUT, async { let (_, _, status) = tokio::try_join!(async { out.take((process::LIMIT+1) as u64).read_to_end(&mut bytes).await }, async { err.take((process::LIMIT+1) as u64).read_to_end(&mut discarded).await }, child.wait())?;
                Ok::<_, std::io::Error>(status)
            }) => match result { Ok(Ok(status)) => Ok(status), _ => Err("credential_unavailable") },
        };
        child.terminate(pid).await?;
        if result.is_err()
            || !result.is_ok_and(|s| s.success())
            || bytes.len() > process::LIMIT
            || discarded.len() > process::LIMIT
        {
            return Err("credential_unavailable");
        }
        outputs.push(bytes);
    }
    LoginHome::github_leaf(outputs.remove(0), outputs.remove(0))
        .map_err(|_| "credential_unavailable")
}
#[cfg(test)]
#[path = "control_login_tests.rs"]
mod tests;
