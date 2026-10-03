//! Disposable positive canonical pairing only. No ordinary route or execution
//! policy selects this task, and no token/result bytes enter logs or HTTP.
use super::{provider_client::Owner, provider_github, provider_ready};
use crate::{lock, AppState};
use anyhow::{ensure, Result};
use chimaera_core::provider_runtime as wire;
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};
use tokio::io::AsyncReadExt;
use zeroize::Zeroizing;

const ACCOUNT: &str = "controller-fixture";
const PROJECT: &str = "fixture-project-a";
const TOKEN: &str = "synthetic-github-runtime-access";
const VIEWER: &[u8] = b"{\"login\":\"synthetic-fixture-user\"}";
const NAME: &std::ffi::CStr = c"provider-github-fixture.json";

/// Validate the real inherited protected launch before any selected fixture
/// work. A command-line flag alone cannot create a project capability.
pub(crate) fn start(state: &Arc<AppState>) -> Result<()> {
    ensure!(
        std::env::var("CHIMAERA_WORKER").as_deref() == Ok("1"),
        "Fixture worker required"
    );
    let uid = unsafe { nix::libc::geteuid() };
    ensure!(
        (20000..30000).contains(&uid) && unsafe { nix::libc::getegid() } == uid,
        "Fixture project identity refused"
    );
    let pending = lock(&state.pro.execution.provider_pending)
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Fixture inherited provider startup required"))?;
    ensure!(
        pending.launch.account_id == ACCOUNT && pending.launch.workspace_id == PROJECT,
        "Fixture launch binding refused"
    );
    pending
        .protection
        .current()
        .map_err(|_| anyhow::anyhow!("Fixture protection refused"))?;
    let state = state.clone();
    tokio::spawn(async move {
        // One original finite task bound; consumer admission keeps its existing
        // per-request limits/deadlines, and retained child cleanup is unchanged.
        if run(state).await.is_err() {
            eprintln!("provider github fixture failure: 1");
        }
    });
    Ok(())
}
async fn run(state: Arc<AppState>) -> Result<(), wire::Error> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if Instant::now() >= deadline || state.stopping.load(Ordering::Acquire) {
            return Err(wire::Error::StateChanged);
        }
        if provider_ready::fixture_verified(&state)? {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let proof = Owner::new(&state, wire::Command::GithubGhAccess {}, deadline)?;
    let root = proof.project_root()?;
    if root != Path::new("/workspace") {
        return Err(wire::Error::StateChanged);
    }
    let viewer = proof.wait(provider_github::viewer(&state)).await??;
    if viewer.as_slice() != VIEWER {
        return Err(wire::Error::Unavailable);
    }
    drop(viewer);
    proof.current()?;
    let (write, mut read) = tokio::io::duplex(wire::ACCESS_MAX + 64);
    // Read concurrently; even a maximum legal response cannot deadlock its
    // owned output pipe. Both sides are retained through actual completion.
    let (delivered, received) = tokio::join!(
        proof.wait(provider_github::credentials(
            &state,
            b"protocol=https\nhost=github.com\n\n",
            write
        )),
        async {
            let mut bytes = Zeroizing::new(vec![0; wire::ACCESS_MAX + 65]);
            let mut used = 0;
            loop {
                let count = proof
                    .wait(read.read(&mut bytes[used..]))
                    .await?
                    .map_err(|_| wire::Error::Unavailable)?;
                if count == 0 {
                    bytes.truncate(used);
                    return Ok::<_, wire::Error>(bytes);
                }
                used += count;
                if used > wire::ACCESS_MAX + 64 {
                    return Err(wire::Error::LimitReached);
                }
            }
        }
    );
    delivered??;
    let bytes = received?;
    let expected = Zeroizing::new(format!("username=x-access-token\npassword={TOKEN}\n\n"));
    if bytes.as_slice() != expected.as_bytes() {
        return Err(wire::Error::Unavailable);
    }
    drop(bytes);
    drop(expected);
    proof.current()?;
    let pending = lock(&state.pro.execution.provider_pending)
        .clone()
        .ok_or(wire::Error::Inactive)?;
    let revision = pending.launch.registration_revision;
    let generation = pending.launch.launch_generation;
    let root_identity = (
        pending.launch.root_identity.device,
        pending.launch.root_identity.inode,
    );
    // The actual blocking writer owns the captured proof until it settles;
    // dropping this task/observer cannot release its reserved activity early.
    tokio::task::spawn_blocking(move || {
        proof.current()?;
        receipt(&root, root_identity, revision, generation)?;
        proof.current()
    })
    .await
    .map_err(|_| wire::Error::Unavailable)??;
    Ok(())
}
fn receipt(
    root: &Path,
    root_identity: (u64, u64),
    revision: u64,
    generation: u64,
) -> Result<(), wire::Error> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(root)
        .map_err(|_| wire::Error::Unavailable)?;
    let meta = directory.metadata().map_err(|_| wire::Error::Unavailable)?;
    let uid = unsafe { nix::libc::geteuid() };
    if !meta.is_dir()
        || meta.uid() != uid
        || meta.mode() & 0o7777 != 0o700
        || (meta.dev(), meta.ino()) != root_identity
    {
        return Err(wire::Error::StateChanged);
    }
    let fd = unsafe {
        nix::libc::openat(
            directory.as_raw_fd(),
            NAME.as_ptr(),
            nix::libc::O_WRONLY
                | nix::libc::O_CREAT
                | nix::libc::O_EXCL
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(wire::Error::Unavailable);
    }
    use std::os::fd::FromRawFd;
    let mut file = unsafe { File::from_raw_fd(fd) };
    let bytes = serde_json::to_vec(&serde_json::json!({
        "provider_github_fixture":1, "workspace_id":PROJECT,
        "registration_revision":revision,"launch_generation":generation,
        "gh_viewer":true,"https_credentials":true
    }))
    .map_err(|_| wire::Error::Unavailable)?;
    if bytes.len() > 512 {
        return Err(wire::Error::LimitReached);
    }
    file.write_all(&bytes)
        .map_err(|_| wire::Error::Unavailable)?;
    file.sync_all().map_err(|_| wire::Error::Unavailable)?;
    directory.sync_all().map_err(|_| wire::Error::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("ghp-{}", chimaera_core::generate_token()));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
        fn identity(&self) -> (u64, u64) {
            let m = fs::metadata(&self.0).unwrap();
            (m.dev(), m.ino())
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn receipt_requires_exact_root_and_never_overwrites_existing_evidence() {
        let temp = Temp::new();
        let (dev, ino) = temp.identity();
        assert_eq!(
            receipt(&temp.0, (dev, ino.wrapping_add(1)), 2, 1),
            Err(wire::Error::StateChanged)
        );
        assert!(fs::read_dir(&temp.0).unwrap().next().is_none());
        receipt(&temp.0, (dev, ino), 2, 1).unwrap();
        let bytes = fs::read(temp.0.join("provider-github-fixture.json")).unwrap();
        assert!(bytes.len() <= 512);
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["registration_revision"], 2);
        assert_eq!(value["launch_generation"], 1);
        assert_eq!(value["gh_viewer"], true);
        assert_eq!(value["https_credentials"], true);
        assert!(!bytes
            .windows(TOKEN.len())
            .any(|bytes| bytes == TOKEN.as_bytes()));
        assert_eq!(
            receipt(&temp.0, (dev, ino), 3, 2),
            Err(wire::Error::Unavailable)
        );
        assert_eq!(
            fs::read(temp.0.join("provider-github-fixture.json")).unwrap(),
            bytes
        );
    }
}
