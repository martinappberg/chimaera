//! Closed Claude exercise, not normal PTY/chat launch routing. The final child
//! receives only a project frontend credential, never canonical provider auth.
use super::{
    provider_claude::Frontend,
    provider_claude_diagnostics::{self as diagnostics, Stage, Trace},
    provider_client::ChildLifetime,
};
use crate::{cloud::providers::process::Child, AppState};
use chimaera_core::provider_runtime as wire;
use std::{
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    sync::oneshot,
};
use zeroize::Zeroizing;

const CLI: &str = "/usr/local/bin/claude";
const HOME: &str = "/home/chimaera";
const CHILD_TIME: Duration = Duration::from_secs(30);
const INPUT: usize = 16 * 1024;
const OUTPUT: usize = 128 * 1024;
const ARGS: &[&str] = &[
    "-p",
    "--output-format",
    "json",
    "--no-session-persistence",
    "--setting-sources",
    "",
    "--max-turns",
    "1",
    "--tools",
    "",
    "--strict-mcp-config",
    "--mcp-config",
    "{\"mcpServers\":{}}",
];

pub(super) async fn exercise(
    state: &Arc<AppState>,
    input: Zeroizing<Vec<u8>>,
) -> Result<Zeroizing<Vec<u8>>, wire::Error> {
    #[cfg(target_os = "linux")]
    {
        exercise_at(
            state,
            input,
            PathBuf::from(CLI),
            PathBuf::from(HOME),
            CHILD_TIME,
        )
        .await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (state, input);
        Err(wire::Error::Unsupported)
    }
}
async fn exercise_at(
    state: &Arc<AppState>,
    input: Zeroizing<Vec<u8>>,
    executable: PathBuf,
    home: PathBuf,
    budget: Duration,
) -> Result<Zeroizing<Vec<u8>>, wire::Error> {
    exercise_at_diagnostic(state, input, executable, home, budget, None).await
}
#[cfg(feature = "provider-claude-fixture")]
pub(super) async fn exercise_fixture(
    state: &Arc<AppState>,
    input: Zeroizing<Vec<u8>>,
    trace: Arc<Trace>,
) -> Result<Zeroizing<Vec<u8>>, wire::Error> {
    exercise_at_diagnostic(
        state,
        input,
        PathBuf::from(CLI),
        PathBuf::from(HOME),
        CHILD_TIME,
        Some(trace),
    )
    .await
}
async fn exercise_at_diagnostic(
    state: &Arc<AppState>,
    input: Zeroizing<Vec<u8>>,
    executable: PathBuf,
    home: PathBuf,
    budget: Duration,
    trace: Option<Arc<Trace>>,
) -> Result<Zeroizing<Vec<u8>>, wire::Error> {
    if input.is_empty() || input.len() > INPUT {
        return Err(wire::Error::InvalidRequest);
    }
    let lifetime = ChildLifetime::new(state, Instant::now() + budget)?;
    let _observer = lifetime.observer();
    let (sent, received) = oneshot::channel();
    tokio::spawn(async move {
        let result = run(&lifetime, input, &executable, &home, trace).await;
        drop(lifetime);
        let _ = sent.send(result);
    });
    received.await.map_err(|_| wire::Error::Unavailable)?
}
async fn bounded<R: AsyncRead + Unpin>(mut reader: R) -> Result<Zeroizing<Vec<u8>>, wire::Error> {
    let mut bytes = Zeroizing::new(vec![0; OUTPUT + 1]);
    let mut length = 0;
    loop {
        let n = reader
            .read(&mut bytes[length..])
            .await
            .map_err(|_| wire::Error::Unavailable)?;
        if n == 0 {
            bytes.truncate(length);
            return Ok(bytes);
        }
        length += n;
        if length > OUTPUT {
            return Err(wire::Error::LimitReached);
        }
    }
}
async fn run(
    lifetime: &Arc<ChildLifetime>,
    input: Zeroizing<Vec<u8>>,
    executable: &Path,
    home: &Path,
    trace: Option<Arc<Trace>>,
) -> Result<Zeroizing<Vec<u8>>, wire::Error> {
    let cwd = lifetime.project_root()?;
    let home_copy = home.to_path_buf();
    // This blocking continuation remains owned even after its observer retires.
    let config = tokio::task::spawn_blocking(move || Config::capture(&home_copy))
        .await
        .map_err(|_| wire::Error::Unavailable)
        .inspect_err(|_| diagnostics::emit(&trace, Stage::ConfigRefused))?
        .inspect_err(|_| diagnostics::emit(&trace, Stage::ConfigRefused))?;
    lifetime.current()?;
    diagnostics::emit(&trace, Stage::ConfigCaptured);
    let frontend = Frontend::start_diagnostic(lifetime.clone(), trace.clone())
        .await
        .inspect_err(|_| diagnostics::emit(&trace, Stage::FrontendStartRefused))?;
    diagnostics::emit(&trace, Stage::FrontendStarted);
    let result = async {
        lifetime.current()?;
        let mut command = tokio::process::Command::new(executable);
        command
            .args(ARGS)
            .env_clear()
            .env("HOME", home)
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("CLAUDE_CONFIG_DIR", config.path())
            .env("ANTHROPIC_BASE_URL", frontend.url())
            .env("CLAUDE_CODE_OAUTH_TOKEN", frontend.token())
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .env("DISABLE_TELEMETRY", "1")
            .env("DISABLE_ERROR_REPORTING", "1")
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        config.inherit(&mut command);
        lifetime.current()?;
        lifetime.process_pending(true);
        let mut child = match Child::spawn(&mut command) {
            Ok(child) => child,
            Err(_) => {
                diagnostics::emit(&trace, Stage::ChildSpawnRefused);
                lifetime.process_pending(false);
                return Err(wire::Error::Unavailable);
            }
        };
        diagnostics::emit(&trace, Stage::ChildSpawned);
        let original = child.child.id();
        command.env_remove("CLAUDE_CODE_OAUTH_TOKEN");
        drop(command);
        let result = async {
            lifetime.current()?;
            let mut stdin = child.child.stdin.take().ok_or(wire::Error::Unavailable)?;
            let stdout = child.child.stdout.take().ok_or(wire::Error::Unavailable)?;
            let stderr = child.child.stderr.take().ok_or(wire::Error::Unavailable)?;
            let input_trace = trace.clone();
            let (_, out, _, status) = lifetime
                .wait(async {
                    tokio::try_join!(
                        async move {
                            let result = stdin
                                .write_all(&input)
                                .await
                                .map_err(|_| wire::Error::Unavailable);
                            // Unix pipe shutdown is a no-op. Close the owned
                            // writer before waiting for the CLI to consume EOF.
                            drop(stdin);
                            if result.is_ok() {
                                diagnostics::emit(&input_trace, Stage::StdinEof);
                            }
                            result
                        },
                        bounded(stdout),
                        bounded(stderr),
                        async { child.wait().await.map_err(|_| wire::Error::Unavailable) }
                    )
                })
                .await
                .inspect_err(|_| diagnostics::emit(&trace, Stage::ChildWaitRefused))?
                .inspect_err(|_| diagnostics::emit(&trace, Stage::ChildWaitRefused))?;
            lifetime.current()?;
            if !status.success() {
                use std::os::unix::process::ExitStatusExt;
                diagnostics::emit(
                    &trace,
                    if status.signal().is_some() {
                        Stage::ChildSignaled
                    } else {
                        Stage::ChildNonzero
                    },
                );
                return Err(wire::Error::Unavailable);
            }
            diagnostics::emit(&trace, Stage::ChildSuccess);
            Ok(out)
        }
        .await;
        // Authority deadlines never manufacture positive process cleanup.
        while child.terminate(original).await.is_err() {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        drop(child);
        lifetime.process_pending(false);
        diagnostics::emit(&trace, Stage::ChildCleaned);
        result
    }
    .await;
    // Our successful shutdown intentionally cancels this lifetime. Validate
    // before that cancellation; it must not turn every positive result stale.
    let current = lifetime.current();
    frontend.stop().await;
    diagnostics::emit(&trace, Stage::FrontendCleaned);
    drop(config);
    current?;
    if let Err(error) = result.as_ref() {
        diagnostics::error(&trace, *error);
    }
    result
}

/// Capture the project-private config directory, not shared personal provider
/// HOME. Linux descriptor paths keep settings/history in the original directory
/// if its pathname is replaced. Real pinned CLI compatibility is a later gate.
struct Config(std::fs::File);
impl Config {
    fn capture(home: &Path) -> Result<Self, wire::Error> {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let home = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(
                nix::libc::O_NOFOLLOW
                    | nix::libc::O_DIRECTORY
                    | nix::libc::O_CLOEXEC
                    | nix::libc::O_NONBLOCK,
            )
            .open(home)
            .map_err(|_| wire::Error::Unavailable)?;
        let valid = |file: &std::fs::File| -> Result<(), wire::Error> {
            let m = file.metadata().map_err(|_| wire::Error::Unavailable)?;
            if !m.is_dir() || m.uid() != unsafe { nix::libc::geteuid() } || m.mode() & 0o022 != 0 {
                Err(wire::Error::StateChanged)
            } else {
                Ok(())
            }
        };
        valid(&home)?;
        let name = c".claude";
        if unsafe { nix::libc::mkdirat(home.as_raw_fd(), name.as_ptr(), 0o700) } != 0
            && std::io::Error::last_os_error().raw_os_error() != Some(nix::libc::EEXIST)
        {
            return Err(wire::Error::Unavailable);
        }
        let fd = unsafe {
            nix::libc::openat(
                home.as_raw_fd(),
                name.as_ptr(),
                nix::libc::O_RDONLY
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_DIRECTORY
                    | nix::libc::O_CLOEXEC
                    | nix::libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(wire::Error::StateChanged);
        }
        use std::os::fd::FromRawFd;
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        if fd < 3 {
            return Err(wire::Error::StateChanged);
        }
        valid(&file)?;
        Ok(Self(file))
    }
    fn path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.0.as_raw_fd()))
    }
    fn inherit(&self, command: &mut tokio::process::Command) {
        let fd = self.0.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                let flags = nix::libc::fcntl(fd, nix::libc::F_GETFD);
                if flags < 0
                    || nix::libc::fcntl(fd, nix::libc::F_SETFD, flags & !nix::libc::FD_CLOEXEC) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

#[cfg(test)]
#[path = "provider_claude_child_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "provider_claude_official_tests.rs"]
mod official_tests;
