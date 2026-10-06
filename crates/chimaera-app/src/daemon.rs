//! Local daemon lifecycle: reuse the machine's running daemon when its
//! manifest checks out, else spawn our own executable detached in `--daemon`
//! mode. The daemon deliberately outlives the app — sessions are tmux-grade
//! and quitting the shell must never kill an agent mid-task.
//!
//! Build parity (the local half of daemon self-update on connect): a running
//! daemon whose manifest build differs from this app's is replaced at
//! startup when it is provably idle — graceful stop, respawn — and attached
//! as `outdated` otherwise, so the home screen can offer the explicit
//! update. Same rules as the remote flow, no ssh: the manifest is on disk
//! and the session count comes straight off 127.0.0.1.

#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::time::Duration;

#[cfg(unix)]
use anyhow::{bail, Context};
use chimaera_core::Manifest;
#[cfg(unix)]
use chimaera_remote::Decision;

/// A reachable local daemon.
#[derive(Clone, Debug)]
pub struct LocalDaemon {
    pub port: u16,
    pub token: String,
    /// Build id from the daemon's manifest; `None` = a pre-build-id daemon.
    pub build: Option<String>,
    /// The daemon is an older build than this app, left running because
    /// live sessions (or an unknown count) made replacing it unsafe.
    pub outdated: bool,
    /// Live sessions counted when `outdated` was decided.
    pub live_sessions: Option<usize>,
    /// Authenticated assembly observation; absent on legacy or WSL adoption.
    pub daemon_extension: Option<bool>,
}

/// Headless entry point for `chimaera-app --daemon`. Unix-only: on Windows
/// the daemon is the Linux musl binary inside WSL2, never this executable.
#[cfg(unix)]
pub fn run_headless() {
    run_headless_with_runtime(None);
}

/// Build-selected, stateless daemon runtime; the free executable supplies none.
#[cfg(unix)]
pub type RuntimeFactory = fn() -> std::sync::Arc<dyn chimaera_server::daemon_extension::Runtime>;

/// Private assemblies reuse the original headless runtime and fixed server configuration.
#[cfg(unix)]
pub fn run_headless_with_extension(factory: RuntimeFactory) {
    run_headless_with_runtime(Some(factory));
}

#[cfg(unix)]
fn run_headless_with_runtime(factory: Option<RuntimeFactory>) {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    // Same shape as `chimaera serve` (crates/chimaera/src/main.rs): the
    // daemon measures <1 core steady-state, so four workers carry the async
    // side and the blocking pool takes file walks, renders and NFS stats.
    // Two daemons on two runtime shapes would make starvation bugs reproduce
    // on one and not the other.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .max_blocking_threads(128)
        .enable_all()
        .build()
        .expect("failed to start tokio runtime");
    if let Err(e) = runtime.block_on(async {
        let config = chimaera_server::ServerConfig {
            port: None,
            // The app's local daemon is never a compute-node daemon.
            routable_bind: false,
        };
        match factory {
            Some(factory) => chimaera_server::run_with_extension(config, factory()).await,
            None => chimaera_server::run(config).await,
        }
    }) {
        eprintln!("daemon exited with error: {e:#}");
        std::process::exit(1);
    }
}

/// The free shell may reuse an extension daemon, but must never replace it
/// with its own free executable. Selected account startup requires a positive
/// authenticated extension observation, independently of Core build parity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeRequirement {
    FreeCompatible,
    Extension,
    ExtensionWithIdentity(&'static str),
}

pub(crate) fn valid_assembly_identity(identity: &str) -> bool {
    !identity.is_empty()
        && identity.len() <= 128
        && identity.bytes().all(|byte| byte.is_ascii_graphic())
}

impl RuntimeRequirement {
    #[cfg(unix)]
    fn identity_selected(self) -> bool {
        matches!(self, Self::ExtensionWithIdentity(_))
    }

    #[cfg(unix)]
    fn matches(self, extension: Option<bool>, identity: Option<&str>) -> bool {
        match self {
            Self::FreeCompatible => true,
            Self::Extension => extension == Some(true),
            Self::ExtensionWithIdentity(expected) => {
                extension == Some(true) && identity == Some(expected)
            }
        }
    }

    pub(crate) fn ready(self, daemon: &LocalDaemon) -> bool {
        match self {
            Self::FreeCompatible => true,
            Self::Extension => daemon.daemon_extension == Some(true),
            Self::ExtensionWithIdentity(_) => {
                daemon.daemon_extension == Some(true) && !daemon.outdated
            }
        }
    }
}

#[cfg(all(test, unix))]
fn runtime_decision(
    requirement: RuntimeRequirement,
    local_build: &str,
    remote_build: Option<&str>,
    extension: Option<bool>,
    sessions: Option<usize>,
    force: bool,
) -> anyhow::Result<Decision> {
    runtime_decision_with_identity(
        requirement,
        local_build,
        remote_build,
        extension,
        None,
        sessions,
        force,
    )
}

#[cfg(unix)]
fn runtime_decision_with_identity(
    requirement: RuntimeRequirement,
    local_build: &str,
    remote_build: Option<&str>,
    extension: Option<bool>,
    identity: Option<&str>,
    sessions: Option<usize>,
    force: bool,
) -> anyhow::Result<Decision> {
    if requirement == RuntimeRequirement::FreeCompatible && extension == Some(true) {
        if chimaera_core::builds_match(local_build, remote_build) {
            return Ok(Decision::Reuse);
        }
        if force {
            bail!("This app cannot replace this daemon. Use the application that started it to update it.");
        }
        return Ok(Decision::ConnectOutdated);
    }
    let compatible_build = if !requirement.matches(extension, identity) {
        None
    } else {
        remote_build
    };
    Ok(chimaera_remote::update_decision(
        local_build,
        compatible_build,
        sessions,
        force,
    ))
}

/// Preserve the original free API for callers that do not select an owner.
#[cfg(unix)]
pub async fn ensure_local_daemon() -> anyhow::Result<LocalDaemon> {
    ensure_local_daemon_for(RuntimeRequirement::FreeCompatible).await
}

#[cfg(unix)]
pub(crate) async fn ensure_local_daemon_for(
    requirement: RuntimeRequirement,
) -> anyhow::Result<LocalDaemon> {
    if let Some(probed) = first_look(Manifest::load(), requirement).await {
        let m = &probed.manifest;
        let compatible = chimaera_core::builds_match(chimaera_core::BUILD_ID, m.build.as_deref())
            && requirement.matches(probed.extension, probed.identity.as_deref());
        let sessions = if compatible {
            None
        } else {
            live_session_count(m.port, &m.token).await
        };
        match runtime_decision_with_identity(
            requirement,
            chimaera_core::BUILD_ID,
            m.build.as_deref(),
            probed.extension,
            probed.identity.as_deref(),
            sessions,
            false,
        )? {
            Decision::Reuse => return Ok(probed.attached(false, sessions)),
            Decision::Update => stop_local(m).await?,
            Decision::ConnectOutdated => {
                tracing::warn!("local daemon replacement deferred: build or selected assembly differs and safe replacement is unavailable");
                return Ok(probed.attached(true, sessions));
            }
        }
    }
    spawn_detached().context("failed to spawn the local daemon")?;
    // Original readiness rounds and per-probe deadline; no timer is reseeded.
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let Ok(Some(probed)) = probe(requirement).await else {
            continue;
        };
        if runtime_decision_with_identity(
            requirement,
            chimaera_core::BUILD_ID,
            probed.manifest.build.as_deref(),
            probed.extension,
            probed.identity.as_deref(),
            None,
            false,
        )? == Decision::Reuse
        {
            return Ok(probed.attached(false, None));
        }
    }
    bail!(
        "The local daemon did not become ready — check {}",
        log_path().display()
    )
}

/// Explicit replacement retains the original user-consented session/stop rules.
#[cfg(unix)]
pub async fn update_local_daemon() -> anyhow::Result<LocalDaemon> {
    update_local_daemon_for(RuntimeRequirement::FreeCompatible).await
}

#[cfg(unix)]
pub(crate) async fn update_local_daemon_for(
    requirement: RuntimeRequirement,
) -> anyhow::Result<LocalDaemon> {
    if let Some(probed) = first_look(Manifest::load(), requirement).await {
        let m = &probed.manifest;
        let decision = runtime_decision_with_identity(
            requirement,
            chimaera_core::BUILD_ID,
            m.build.as_deref(),
            probed.extension,
            probed.identity.as_deref(),
            None,
            false,
        )?;
        if decision == Decision::Reuse {
            return Ok(probed.attached(false, None));
        }
        // Force only after the free/selected-assembly downgrade check.
        runtime_decision_with_identity(
            requirement,
            chimaera_core::BUILD_ID,
            m.build.as_deref(),
            probed.extension,
            probed.identity.as_deref(),
            None,
            true,
        )?;
        stop_local(m).await?;
    }
    ensure_local_daemon_for(requirement).await
}

// On Windows the local daemon is the Linux musl binary inside WSL2; the wsl
// module owns detect/provision/spawn/adopt. Startup only ADOPTS here —
// anything that would provision, replace, or install runs in the wizard
// window instead, so minutes of download/setup are never invisible. Errors
// carrying WslNotReady make startup open that wizard rather than fail.
#[cfg(windows)]
pub async fn ensure_local_daemon() -> anyhow::Result<LocalDaemon> {
    crate::wsl::adopt_daemon().await
}

#[cfg(windows)]
pub async fn update_local_daemon() -> anyhow::Result<LocalDaemon> {
    crate::wsl::update_daemon(&|_| {}).await
}

#[cfg(windows)]
pub(crate) async fn ensure_local_daemon_for(_: RuntimeRequirement) -> anyhow::Result<LocalDaemon> {
    ensure_local_daemon().await
}

#[cfg(windows)]
pub(crate) async fn update_local_daemon_for(_: RuntimeRequirement) -> anyhow::Result<LocalDaemon> {
    update_local_daemon().await
}

/// Manifest → LocalDaemon, shared by the unix adopt path and the WSL engine.
pub(crate) fn attached(m: Manifest, outdated: bool, live_sessions: Option<usize>) -> LocalDaemon {
    LocalDaemon {
        port: m.port,
        token: m.token,
        build: m.build,
        outdated,
        live_sessions,
        daemon_extension: None,
    }
}

#[cfg(unix)]
struct Probed {
    manifest: Manifest,
    extension: Option<bool>,
    identity: Option<String>,
}

#[cfg(unix)]
impl Probed {
    fn attached(self, outdated: bool, sessions: Option<usize>) -> LocalDaemon {
        let mut local = attached(self.manifest, outdated, sessions);
        local.daemon_extension = self.extension;
        local
    }
}

/// The same original manifest PID/token and one bounded authenticated request.
#[cfg(unix)]
async fn probe(requirement: RuntimeRequirement) -> anyhow::Result<Option<Probed>> {
    probe_loaded_for(Manifest::load(), requirement).await
}

/// The first look before attaching or starting. A daemon that does not
/// answer its health check plainly (busy past the 2 s budget, hung, a token
/// that no longer authenticates) does not abort the launch, as it never did:
/// fall through to spawning. A second daemon refuses to start beside a live
/// one, and the readiness wait attaches only to a daemon that answers and
/// matches.
#[cfg(unix)]
async fn first_look(
    loaded: anyhow::Result<Option<Manifest>>,
    requirement: RuntimeRequirement,
) -> Option<Probed> {
    match probe_loaded_for(loaded, requirement).await {
        Ok(probed) => probed,
        Err(error) => {
            tracing::warn!(
                "the local daemon did not answer its health check ({error:#}); starting one"
            );
            None
        }
    }
}

#[cfg(unix)]
async fn probe_loaded_for(
    loaded: anyhow::Result<Option<Manifest>>,
    requirement: RuntimeRequirement,
) -> anyhow::Result<Option<Probed>> {
    // Corrupt/unreadable records never supplied a usable original daemon.
    // The server independently checks its own live-record/startup lock.
    let Some(m) = loaded.ok().flatten() else {
        return Ok(None);
    };
    if !m.is_alive() {
        return Ok(None);
    }
    let (port, token, pid, build) = (m.port, m.token.clone(), m.pid, m.build.clone());
    let health = tokio::task::spawn_blocking(move || {
        health_observation(
            port,
            &token,
            pid,
            build.as_deref(),
            requirement.identity_selected(),
        )
    })
    .await
    .context("local daemon health worker failed")??;
    let (extension, identity) = match health {
        Health::Reachable(extension, identity) => (extension, identity),
        Health::Closed => return Ok(None),
    };
    Ok(Some(Probed {
        manifest: m,
        extension,
        identity,
    }))
}

#[cfg(unix)]
fn refused_hint(error: &anyhow::Error) -> bool {
    matches!(error.downcast_ref::<ureq::Error>(), Some(ureq::Error::Io(error))
        if error.kind() == std::io::ErrorKind::ConnectionRefused)
}

#[cfg(unix)]
fn decode_health(
    body: &[u8],
    pid: u32,
    build: Option<&str>,
    identity_selected: bool,
) -> anyhow::Result<(Option<bool>, Option<String>)> {
    if body.len() > 16 * 1024 {
        bail!("local daemon health response exceeds its bound")
    }
    let value: serde_json::Value =
        serde_json::from_slice(body).context("invalid local daemon health response")?;
    if value.get("name").and_then(|v| v.as_str()) != Some("chimaera")
        || value.get("pid").and_then(|v| v.as_u64()) != Some(u64::from(pid))
        || build.is_some_and(|build| value.get("build").and_then(|v| v.as_str()) != Some(build))
    {
        bail!("local daemon health does not match its original manifest");
    }
    let extension = match value.get("daemon_extension") {
        None => None,
        Some(serde_json::Value::Bool(enabled)) => Some(*enabled),
        Some(_) => bail!("invalid local daemon assembly observation"),
    };
    // Free/legacy assemblies retain their original tolerance of unknown fields.
    let identity = if identity_selected {
        match value.get("daemon_assembly") {
            None => None,
            Some(value) => {
                let Some(identity) = value
                    .as_str()
                    .filter(|identity| valid_assembly_identity(identity))
                else {
                    bail!("invalid local daemon assembly identity");
                };
                Some(identity.to_owned())
            }
        }
    } else {
        None
    };
    Ok((extension, identity))
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum Health {
    Reachable(Option<bool>, Option<String>),
    Closed,
}

#[cfg(unix)]
fn confirmed_closed(port: u16, deadline: std::time::Instant) -> bool {
    let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
        return false;
    };
    if remaining.is_zero() {
        return false;
    }
    let address = (std::net::Ipv4Addr::LOCALHOST, port).into();
    matches!(std::net::TcpStream::connect_timeout(&address, remaining.min(Duration::from_millis(100))),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused)
}

#[cfg(unix)]
fn health_observation(
    port: u16,
    token: &str,
    pid: u32,
    build: Option<&str>,
    identity_selected: bool,
) -> anyhow::Result<Health> {
    use std::io::Read;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let agent = crate::http::agent();
    let Some(remaining) = deadline
        .checked_duration_since(std::time::Instant::now())
        .filter(|remaining| !remaining.is_zero())
    else {
        bail!("local daemon health deadline expired");
    };
    let response = agent
        .get(&format!("http://127.0.0.1:{port}/api/v1/health"))
        .header("Authorization", &format!("Bearer {token}"))
        .config()
        .timeout_global(Some(remaining))
        .build()
        .call()
        .context("could not authenticate the running local daemon");
    let mut response = match response {
        Ok(response) => response,
        // ureq can synthesize its refusal after exhausted address attempts.
        // Confirm only this hint with raw IO, inside the same original budget.
        Err(error) if refused_hint(&error) && confirmed_closed(port, deadline) => {
            return Ok(Health::Closed)
        }
        Err(error) => return Err(error),
    };
    let mut body = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(16 * 1024 + 1)
        .read_to_end(&mut body)
        .context("could not read local daemon health")?;
    decode_health(&body, pid, build, identity_selected)
        .map(|(extension, identity)| Health::Reachable(extension, identity))
}

/// GET /api/v1/health with the manifest token; any 200 counts. Shared with
/// the WSL probe, where passing it also proves the NAT localhost forward.
#[cfg(windows)]
pub(crate) fn health_ok(port: u16, token: &str) -> bool {
    crate::http::agent()
        .get(&format!("http://127.0.0.1:{port}/api/v1/health"))
        .header("Authorization", &format!("Bearer {token}"))
        .config()
        .timeout_global(Some(Duration::from_secs(2)))
        .build()
        .call()
        .is_ok()
}

/// Live session count straight off the local daemon (loopback + manifest
/// token, no ssh). `None` = could not determine; callers treat that as
/// busy, never as zero.
pub(crate) async fn live_session_count(port: u16, token: &str) -> Option<usize> {
    let token = token.to_string();
    tokio::task::spawn_blocking(move || {
        let mut response = crate::http::agent()
            .get(&format!("http://127.0.0.1:{port}/api/v1/sessions"))
            .header("Authorization", &format!("Bearer {token}"))
            .config()
            .timeout_global(Some(Duration::from_secs(5)))
            .build()
            .call()
            .ok()?;
        let body = response.body_mut().read_to_string().ok()?;
        chimaera_remote::count_alive_sessions(&body)
    })
    .await
    .unwrap_or(None)
}

/// Gracefully stop the local daemon: SIGTERM, then poll for exit for up to
/// ~10s. Never escalates to SIGKILL — a daemon that will not die may be
/// holding sessions that must not be torn out from under their owner.
#[cfg(unix)]
async fn stop_local(m: &Manifest) -> anyhow::Result<()> {
    tracing::info!("stopping local daemon (pid {})", m.pid);
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(m.pid as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .with_context(|| format!("failed to signal pid {}", m.pid))?;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !m.is_alive() {
            return Ok(());
        }
    }
    bail!(
        "local daemon (pid {}) is still running 10s after SIGTERM — refusing to kill -9 it; \
         something is keeping it busy (open browser tabs hold its sockets). Close them and retry.",
        m.pid
    )
}

#[cfg(unix)]
fn log_path() -> std::path::PathBuf {
    chimaera_core::data_dir().join("logs").join("serve.log")
}

/// Spawn our own executable as `--daemon`, in a new session with stdio on
/// the serve log, so it survives the shell quitting.
#[cfg(unix)]
fn spawn_detached() -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let log = log_path();
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .with_context(|| format!("failed to open {}", log.display()))?;
    let err = out.try_clone()?;
    tracing::info!("starting local daemon: {} --daemon", exe.display());
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--daemon")
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0);
    cmd.spawn()?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn assembly_matrix_preserves_idle_admission_and_prevents_free_downgrade() {
        use RuntimeRequirement::{Extension, FreeCompatible};
        let build = chimaera_core::BUILD_ID;
        for sessions in [None, Some(0), Some(3)] {
            assert_eq!(
                runtime_decision(
                    FreeCompatible,
                    build,
                    Some(build),
                    Some(true),
                    sessions,
                    false
                )
                .unwrap(),
                Decision::Reuse
            );
            assert_eq!(
                runtime_decision(Extension, build, Some(build), Some(true), sessions, false)
                    .unwrap(),
                Decision::Reuse
            );
            assert_eq!(
                runtime_decision(
                    FreeCompatible,
                    build,
                    Some("different"),
                    Some(true),
                    sessions,
                    false
                )
                .unwrap(),
                Decision::ConnectOutdated
            );
            assert!(runtime_decision(
                FreeCompatible,
                build,
                Some("different"),
                Some(true),
                sessions,
                true
            )
            .is_err());
            for observed in [None, Some(false)] {
                let expected = if sessions == Some(0) {
                    Decision::Update
                } else {
                    Decision::ConnectOutdated
                };
                assert_eq!(
                    runtime_decision(Extension, build, Some(build), observed, sessions, false)
                        .unwrap(),
                    expected
                );
                assert_eq!(
                    runtime_decision(Extension, build, Some(build), observed, sessions, true)
                        .unwrap(),
                    Decision::Update
                );
                assert_eq!(
                    runtime_decision(
                        FreeCompatible,
                        build,
                        Some(build),
                        observed,
                        sessions,
                        false
                    )
                    .unwrap(),
                    Decision::Reuse
                );
                assert_eq!(
                    runtime_decision(
                        FreeCompatible,
                        build,
                        Some("different"),
                        observed,
                        sessions,
                        false
                    )
                    .unwrap(),
                    expected
                );
            }
        }
        let mut daemon = LocalDaemon {
            port: 1,
            token: String::new(),
            build: Some(build.into()),
            outdated: false,
            live_sessions: None,
            daemon_extension: None,
        };
        assert!(FreeCompatible.ready(&daemon));
        assert!(!Extension.ready(&daemon));
        daemon.daemon_extension = Some(false);
        assert!(!Extension.ready(&daemon));
        daemon.daemon_extension = Some(true);
        assert!(Extension.ready(&daemon));
    }

    async fn probe_loaded(
        loaded: anyhow::Result<Option<Manifest>>,
    ) -> anyhow::Result<Option<Probed>> {
        probe_loaded_for(loaded, RuntimeRequirement::FreeCompatible).await
    }

    fn health_extension(
        port: u16,
        token: &str,
        pid: u32,
        build: Option<&str>,
    ) -> anyhow::Result<Health> {
        health_observation(port, token, pid, build, false)
    }

    #[test]
    fn selected_identity_preserves_original_replacement_and_readiness_rules() {
        let selected = RuntimeRequirement::ExtensionWithIdentity("2.0.0@fixture-new");
        for sessions in [None, Some(0), Some(3)] {
            for identity in [None, Some("1.0.0@fixture-old")] {
                let expected = if sessions == Some(0) {
                    Decision::Update
                } else {
                    Decision::ConnectOutdated
                };
                assert_eq!(
                    runtime_decision_with_identity(
                        selected,
                        "sdk.2",
                        Some("sdk.1"),
                        Some(true),
                        identity,
                        sessions,
                        false
                    )
                    .unwrap(),
                    expected
                );
                assert_eq!(
                    runtime_decision_with_identity(
                        selected,
                        "sdk.2",
                        Some("sdk.1"),
                        Some(true),
                        identity,
                        sessions,
                        true
                    )
                    .unwrap(),
                    Decision::Update
                );
            }
            assert_eq!(
                runtime_decision_with_identity(
                    selected,
                    "sdk.2",
                    Some("sdk.1"),
                    Some(true),
                    Some("2.0.0@fixture-new"),
                    sessions,
                    false
                )
                .unwrap(),
                Decision::Reuse
            );
            // Unknown extra metadata must not change the free or legacy path.
            for requirement in [
                RuntimeRequirement::FreeCompatible,
                RuntimeRequirement::Extension,
            ] {
                assert_eq!(
                    runtime_decision_with_identity(
                        requirement,
                        "sdk.2",
                        Some("sdk.1"),
                        Some(true),
                        Some("foreign"),
                        sessions,
                        false
                    )
                    .unwrap(),
                    Decision::Reuse
                );
            }
        }
        assert!(runtime_decision_with_identity(
            RuntimeRequirement::FreeCompatible,
            "sdk.2",
            Some("other.1"),
            Some(true),
            Some("foreign"),
            Some(0),
            true
        )
        .is_err());
        let mut local = LocalDaemon {
            port: 1,
            token: String::new(),
            build: Some("sdk.1".into()),
            outdated: true,
            live_sessions: Some(3),
            daemon_extension: Some(true),
        };
        assert!(
            !selected.ready(&local),
            "an old busy assembly must not initialize its successor owner"
        );
        local.outdated = false;
        assert!(selected.ready(&local));
        local.daemon_extension = None;
        assert!(!selected.ready(&local));
    }

    #[test]
    fn authenticated_health_identity_is_bounded_and_selected_only() {
        let baseline = serde_json::json!({"name":"chimaera", "pid":42, "build":"fixture-build", "daemon_extension":true});
        for identity in [
            None,
            Some(serde_json::json!("2.0.0@fixture-new")),
            Some(serde_json::json!("1.0.0@fixture-old")),
        ] {
            let mut value = baseline.clone();
            if let Some(identity) = identity {
                value["daemon_assembly"] = identity;
            }
            let (port, task) = serve_health(serde_json::to_vec(&value).unwrap());
            let result = health_observation(port, "fixture-token", 42, Some("fixture-build"), true);
            task.join().unwrap();
            assert_eq!(
                result.unwrap(),
                Health::Reachable(
                    Some(true),
                    value
                        .get("daemon_assembly")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned)
                )
            );
        }
        for identity in [
            serde_json::json!(null),
            serde_json::json!(42),
            serde_json::json!(""),
            serde_json::json!("bad identity"),
            serde_json::json!("x".repeat(129)),
        ] {
            let mut value = baseline.clone();
            value["daemon_assembly"] = identity;
            let raw = serde_json::to_vec(&value).unwrap();
            let (port, task) = serve_health(raw.clone());
            let selected =
                health_observation(port, "fixture-token", 42, Some("fixture-build"), true);
            task.join().unwrap();
            assert!(
                selected.is_err(),
                "malformed selected identity must refuse before any replacement"
            );
            let (port, task) = serve_health(raw);
            let free = health_observation(port, "fixture-token", 42, Some("fixture-build"), false);
            task.join().unwrap();
            assert_eq!(free.unwrap(), Health::Reachable(Some(true), None));
        }
    }

    // Exercise the actual bounded authenticated request, not only the JSON helper.
    fn serve_health(body: Vec<u8>) -> (u16, std::thread::JoinHandle<()>) {
        serve_response(200, body)
    }

    fn serve_response(status: u16, body: Vec<u8>) -> (u16, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let task = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            let mut peer = loop {
                match listener.accept() {
                    Ok((peer, _)) => break peer,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("fixture health accept failed: {e}"),
                }
            };
            peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            peer.set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                peer.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() <= 4096);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("GET /api/v1/health HTTP/1.1\r\n"));
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer fixture-token\r\n"));
            write!(
                peer,
                "HTTP/1.1 {status} fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            // The capped client may close before an over-bound body is fully written.
            let _ = peer.write_all(&body);
        });
        (port, task)
    }

    #[test]
    fn original_authenticated_health_checks_assembly_identity_and_bound() {
        for marker in [None, Some(false), Some(true)] {
            let mut value =
                serde_json::json!({"name":"chimaera", "pid":42, "build":"fixture-build"});
            if let Some(marker) = marker {
                value["daemon_extension"] = marker.into();
            }
            let (port, task) = serve_health(serde_json::to_vec(&value).unwrap());
            let result = health_extension(port, "fixture-token", 42, Some("fixture-build"));
            task.join().unwrap();
            assert_eq!(result.unwrap(), Health::Reachable(marker, None));
        }
        for body in [
            br#"{"name":"chimaera","pid":43,"build":"fixture-build","daemon_extension":true}"#
                .to_vec(),
            br#"{"name":"chimaera","pid":42,"build":"other","daemon_extension":true}"#.to_vec(),
            br#"{"name":"chimaera","pid":42,"build":"fixture-build","daemon_extension":"true"}"#
                .to_vec(),
            br#"{"name":"chimaera","pid":42,"build":"fixture-build","daemon_extension":null}"#
                .to_vec(),
            b"not JSON".to_vec(),
            vec![b' '; 16 * 1024 + 1],
        ] {
            let (port, task) = serve_health(body);
            let result = health_extension(port, "fixture-token", 42, Some("fixture-build"));
            task.join().unwrap();
            assert!(result.is_err());
        }
    }

    #[tokio::test]
    async fn actual_probe_recovers_stale_records_but_refuses_reachable_failures() {
        use std::io::Write;
        struct OwnedRecord(std::path::PathBuf);
        impl Drop for OwnedRecord {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let record = OwnedRecord(std::env::temp_dir().join(format!(
            "chimaera-native-manifest-{}-{}.json", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        )));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&record.0)
            .unwrap();
        file.write_all(b"not a manifest").unwrap();
        drop(file);
        let loaded = std::fs::read(&record.0)
            .map_err(anyhow::Error::from)
            .and_then(|body| Ok(Some(serde_json::from_slice::<Manifest>(&body)?)));
        assert!(probe_loaded(loaded).await.unwrap().is_none());
        let unreadable = std::fs::read(record.0.join("unreadable-child"))
            .map_err(anyhow::Error::from)
            .and_then(|body| Ok(Some(serde_json::from_slice::<Manifest>(&body)?)));
        assert!(probe_loaded(unreadable).await.unwrap().is_none());

        let manifest = |port| Manifest {
            hostname: "fixture".into(),
            port,
            token: "fixture-token".into(),
            pid: std::process::id(),
            version: "fixture".into(),
            started_at: 0,
            build: Some("fixture-build".into()),
            slurm_job_id: None,
            runtime_leases: false,
            daemon_extension: false,
        };
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        assert!(probe_loaded(Ok(Some(manifest(port))))
            .await
            .unwrap()
            .is_none());
        for (status, body) in [
            (200, b"not JSON".to_vec()),
            (401, b"unauthorized".to_vec()),
            (
                200,
                br#"{"name":"chimaera","pid":0,"daemon_extension":true}"#.to_vec(),
            ),
        ] {
            let (port, task) = serve_response(status, body.clone());
            let result = probe_loaded(Ok(Some(manifest(port)))).await;
            task.join().unwrap();
            assert!(result.is_err());
            // The launch itself never aborts on it: it starts a daemon.
            let (port, task) = serve_response(status, body);
            let first = first_look(Ok(Some(manifest(port))), RuntimeRequirement::Extension).await;
            task.join().unwrap();
            assert!(first.is_none());
        }
        // Ambiguous failures remain protective; only positive connection refusal recovers.
        assert!(!refused_hint(&ureq::Error::ConnectionFailed.into()));
        assert!(!refused_hint(
            &ureq::Error::Timeout(ureq::Timeout::Global).into()
        ));
    }

    #[test]
    fn raw_closed_confirmation_preserves_original_deadline_and_listening_refusal() {
        let listening = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listening.local_addr().unwrap().port();
        assert!(!confirmed_closed(
            port,
            std::time::Instant::now() + Duration::from_secs(2)
        ));
        drop(listening);
        assert!(confirmed_closed(
            port,
            std::time::Instant::now() + Duration::from_secs(2)
        ));
        assert!(!confirmed_closed(
            port,
            std::time::Instant::now() - Duration::from_millis(1)
        ));
    }
}
