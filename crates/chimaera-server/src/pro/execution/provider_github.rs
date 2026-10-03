//! Fixed GitHub consumer, deliberately not a project-shell credential overlay.
//! Named tokens enter only a final owned gh child or an exact HTTPS helper pipe.
use super::provider_client::Owner;
use crate::{cloud::providers::process::Child, AppState};
use chimaera_core::provider_runtime as wire;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::oneshot,
};
use zeroize::Zeroizing;

const BUDGET: Duration = Duration::from_secs(10);
const OUTPUT: usize = 128 * 1024;
const HOME: &str = "/home/chimaera";
const GH: &str = "/usr/bin/gh";
const ARGS: &[&str] = &["api", "--hostname", "github.com", "--method", "GET", "user"];

/// Exact `get` payload only. No URL, username, path, configuration or executable
/// selector is accepted, including duplicate fields and trailing records.
fn https_get(input: &[u8]) -> Result<(), wire::Error> {
    if input.len() > 4096 || !input.ends_with(b"\n\n") {
        return Err(wire::Error::InvalidRequest);
    }
    let input = std::str::from_utf8(input).map_err(|_| wire::Error::InvalidRequest)?;
    let mut protocol = false;
    let mut host = false;
    for line in input[..input.len() - 2].split('\n') {
        match line {
            "protocol=https" if !protocol => protocol = true,
            "host=github.com" if !host => host = true,
            _ => return Err(wire::Error::InvalidRequest),
        }
    }
    if protocol && host {
        Ok(())
    } else {
        Err(wire::Error::InvalidRequest)
    }
}
/// Caller supplies an already-owned helper output pipe, never an upstream URL.
/// Its observer may disappear; actual delivery/EOF stays owned and counted.
pub(super) async fn credentials<W>(
    state: &Arc<AppState>,
    input: &[u8],
    mut output: W,
) -> Result<(), wire::Error>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    https_get(input)?;
    let owner = Owner::new(
        state,
        wire::Command::GithubHttpsCredentials {
            protocol: "https".into(),
            host: "github.com".into(),
        },
        Instant::now() + BUDGET,
    )?;
    let _observer = owner.observer();
    let (sent, received) = oneshot::channel();
    tokio::spawn(async move {
        let result = async {
            let access = owner.github().await?;
            owner.current()?;
            // Fixed maximum preallocation, including the ASCII access-token cap.
            let mut bytes = Zeroizing::new(Vec::with_capacity(wire::ACCESS_MAX + 64));
            bytes.extend_from_slice(b"username=x-access-token\npassword=");
            bytes.extend_from_slice(access.access_token.expose().as_bytes());
            bytes.extend_from_slice(b"\n\n");
            owner
                .wait(output.write_all(&bytes))
                .await?
                .map_err(|_| wire::Error::Unavailable)?;
            owner
                .wait(output.shutdown())
                .await?
                .map_err(|_| wire::Error::Unavailable)?;
            owner.current()
        }
        .await;
        // Close the actual writer and secret buffers before the activity receipt.
        drop(output);
        drop(owner);
        let _ = sent.send(result);
    });
    received.await.map_err(|_| wire::Error::Unavailable)?
}

/// Closed viewer command proves final-spawn auth without accepting arbitrary gh
/// commands. General shell/gh routing and the Claude/Codex overlay remain open.
pub(super) async fn viewer(state: &Arc<AppState>) -> Result<Zeroizing<Vec<u8>>, wire::Error> {
    #[cfg(target_os = "linux")]
    {
        viewer_at(state, PathBuf::from(GH), PathBuf::from(HOME), BUDGET).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = state;
        Err(wire::Error::Unsupported)
    }
}
async fn viewer_at(
    state: &Arc<AppState>,
    executable: PathBuf,
    home: PathBuf,
    budget: Duration,
) -> Result<Zeroizing<Vec<u8>>, wire::Error> {
    let owner = Owner::new(
        state,
        wire::Command::GithubGhAccess {},
        Instant::now() + budget,
    )?;
    let cwd = owner.project_root()?;
    let _observer = owner.observer();
    let (sent, received) = oneshot::channel();
    tokio::spawn(async move {
        let result = run(&owner, &executable, &home, &cwd).await;
        drop(owner);
        let _ = sent.send(result);
    });
    received.await.map_err(|_| wire::Error::Unavailable)?
}
async fn bounded<R: AsyncRead + Unpin>(mut reader: R) -> Result<Zeroizing<Vec<u8>>, wire::Error> {
    let mut bytes = Zeroizing::new(vec![0; OUTPUT + 1]);
    let mut length = 0;
    loop {
        let count = reader
            .read(&mut bytes[length..])
            .await
            .map_err(|_| wire::Error::Unavailable)?;
        if count == 0 {
            bytes.truncate(length);
            return Ok(bytes);
        }
        length += count;
        if length > OUTPUT {
            return Err(wire::Error::LimitReached);
        }
    }
}
async fn run(
    owner: &Owner,
    executable: &Path,
    home: &Path,
    cwd: &Path,
) -> Result<Zeroizing<Vec<u8>>, wire::Error> {
    owner.current()?;
    // Retain the off-reactor operation through completion even when its observer
    // retires. A detached blocking worker must not escape the activity budget.
    let config = tokio::task::spawn_blocking(EmptyConfig::new)
        .await
        .map_err(|_| wire::Error::Unavailable)??;
    owner.current()?;
    let access = owner.github().await?;
    owner.current()?;
    // No login shell, mutable PATH, ambient token/proxy/TLS/host override,
    // hooks, extra argv or personal provider HOME enters this fixed child.
    let mut command = tokio::process::Command::new(executable);
    command
        .args(ARGS)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("GH_CONFIG_DIR", config.path())
        .env("GH_HOST", "github.com")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1")
        .env("GH_TELEMETRY", "false")
        .env("GH_TOKEN", access.access_token.expose())
        .env("LANG", "C.UTF-8")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    config.inherit(&mut command);
    owner.current()?;
    owner.child_pending(true);
    let mut child = match Child::spawn(&mut command) {
        Ok(child) => child,
        Err(_) => {
            owner.child_pending(false);
            return Err(wire::Error::Unavailable);
        }
    };
    let original = child.child.id();
    // The command is never returned, cloned or formatted. Clear its token copy
    // immediately after final spawn; the access value remains zeroizing.
    command.env_remove("GH_TOKEN");
    drop(command);
    let result = async {
        owner.current()?;
        let stdout = child.child.stdout.take().ok_or(wire::Error::Unavailable)?;
        let stderr = child.child.stderr.take().ok_or(wire::Error::Unavailable)?;
        let ((stdout, _), status) = owner
            .wait(async {
                tokio::try_join!(
                    async { tokio::try_join!(bounded(stdout), bounded(stderr)) },
                    async { child.wait().await.map_err(|_| wire::Error::Unavailable) }
                )
            })
            .await??;
        owner.current()?;
        // Raw stderr and vendor failures never become errors/status or logs.
        if !status.success() {
            return Err(wire::Error::Unavailable);
        }
        Ok(stdout)
    }
    .await;
    // Always require positive group disappearance, including success, timeout
    // and post-spawn revocation. Deadline expiry closes authority, not cleanup.
    while child.terminate(original).await.is_err() {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    drop(child);
    drop(config);
    owner.child_pending(false);
    drop(access);
    owner.current()?;
    result
}

/// No named configuration is read after a path check. The captured empty
/// directory is already unlinked before credentials; Linux nlink=0 proves no
/// pathname can mutate it. Only this fd becomes inheritable in the final child.
struct EmptyConfig(std::fs::File);
impl EmptyConfig {
    fn new() -> Result<Self, wire::Error> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
        let path = PathBuf::from("/tmp").join(format!(
            "chimaera-gh-config-{}",
            chimaera_core::generate_token()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| wire::Error::Unavailable)?;
        let result = (|| {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC)
                .open(&path)
                .map_err(|_| wire::Error::Unavailable)?;
            let opened = file.metadata().map_err(|_| wire::Error::Unavailable)?;
            let named = std::fs::symlink_metadata(&path).map_err(|_| wire::Error::Unavailable)?;
            if !opened.is_dir()
                || opened.uid() != unsafe { nix::libc::geteuid() }
                || opened.mode() & 0o777 != 0o700
                || opened.dev() != named.dev()
                || opened.ino() != named.ino()
            {
                return Err(wire::Error::StateChanged);
            }
            std::fs::remove_dir(&path).map_err(|_| wire::Error::Unavailable)?;
            #[cfg(target_os = "linux")]
            if file
                .metadata()
                .map_err(|_| wire::Error::Unavailable)?
                .nlink()
                != 0
            {
                return Err(wire::Error::StateChanged);
            }
            Ok(Self(file))
        })();
        // Only our still-empty leaf may be removed; never recursive cleanup.
        if result.is_err() {
            let _ = std::fs::remove_dir(&path);
        }
        result
    }
    fn path(&self) -> String {
        use std::os::fd::AsRawFd;
        #[cfg(target_os = "linux")]
        let prefix = "/proc/self/fd";
        #[cfg(not(target_os = "linux"))]
        let prefix = "/dev/fd";
        format!("{prefix}/{}", self.0.as_raw_fd())
    }
    fn inherit(&self, command: &mut tokio::process::Command) {
        use std::os::fd::AsRawFd;
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
#[path = "provider_github_tests.rs"]
mod tests;
