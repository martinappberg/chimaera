//! Disposable protected Claude print pairing; no ordinary route selects it.
//! Diagnostic receipt only: private independently proves actual Ready, request
//! EOF, canonical synthetic Messages and owned drains. No TUI/vendor acceptance.
use super::{provider_claude_child, provider_client::ChildLifetime, provider_ready};
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
use zeroize::Zeroizing;

const ACCOUNT: &str = "controller-fixture";
const PROJECT: &str = "fixture-project-a";
const MARKER: &[u8] = b"SYNTHETIC_OK";
const NAME: &std::ffi::CStr = c"provider-claude-fixture.json";

/// Validate the real inherited protected launch before any selected fixture
/// work. A command-line flag alone cannot create a project capability.
pub(crate) fn start(state: &Arc<AppState>) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
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
        if run(state, deadline).await.is_err() {
            eprintln!("provider claude fixture failure: 1");
        }
    });
    Ok(())
}
async fn run(state: Arc<AppState>, deadline: Instant) -> Result<(), wire::Error> {
    loop {
        if Instant::now() >= deadline || state.stopping.load(Ordering::Acquire) {
            return Err(wire::Error::StateChanged);
        }
        if provider_ready::fixture_verified(&state)? {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // Two finite fixture reservations: this outer current/receipt guard and
    // exercise's actual child/frontend owner. Neither changes normal launch.
    let proof = ChildLifetime::new(&state, deadline)?;
    let root = proof.project_root()?;
    if root != Path::new("/workspace") {
        return Err(wire::Error::StateChanged);
    }
    let output = proof
        .wait(provider_claude_child::exercise(
            &state,
            Zeroizing::new(b"Respond with SYNTHETIC_OK".to_vec()),
        ))
        .await??;
    if output.len() > 128 * 1024 || !output.windows(MARKER.len()).any(|bytes| bytes == MARKER) {
        return Err(wire::Error::Unavailable);
    }
    drop(output);
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
        receipt(&root, root_identity, revision, generation, || {
            proof.current()
        })
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
    current: impl Fn() -> Result<(), wire::Error>,
) -> Result<(), wire::Error> {
    current()?;
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
    let bytes = serde_json::to_vec(&serde_json::json!({
        "provider_claude_fixture":1, "workspace_id":PROJECT,
        "registration_revision":revision,"launch_generation":generation,
        "print_messages":true
    }))
    .map_err(|_| wire::Error::Unavailable)?;
    if bytes.len() > 512 {
        return Err(wire::Error::LimitReached);
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
    let result = (|| {
        current()?;
        file.write_all(&bytes)
            .map_err(|_| wire::Error::Unavailable)?;
        file.sync_all().map_err(|_| wire::Error::Unavailable)?;
        directory.sync_all().map_err(|_| wire::Error::Unavailable)?;
        current()
    })();
    if result.is_err() {
        // Failed/stale fixture writes cannot leave positive reusable evidence.
        // Truncate the original captured file even if its name was replaced.
        file.set_len(0).map_err(|_| wire::Error::Unavailable)?;
        file.sync_all().map_err(|_| wire::Error::Unavailable)?;
        // Keep an invalid empty diagnostic at its original inode. Unlinking
        // by a project-mutable name could delete an unrelated replacement.
        directory.sync_all().map_err(|_| wire::Error::Unavailable)?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("clp-{}", chimaera_core::generate_token()));
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
            receipt(&temp.0, (dev, ino.wrapping_add(1)), 2, 1, || Ok(())),
            Err(wire::Error::StateChanged)
        );
        assert!(fs::read_dir(&temp.0).unwrap().next().is_none());
        receipt(&temp.0, (dev, ino), 2, 1, || Ok(())).unwrap();
        let bytes = fs::read(temp.0.join("provider-claude-fixture.json")).unwrap();
        assert!(bytes.len() <= 512);
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["registration_revision"], 2);
        assert_eq!(value["launch_generation"], 1);
        assert_eq!(value["provider_claude_fixture"], 1);
        assert_eq!(value["workspace_id"], PROJECT);
        assert_eq!(value["print_messages"], true);
        assert_eq!(value.as_object().unwrap().len(), 5);
        assert!(!bytes.windows(MARKER.len()).any(|bytes| bytes == MARKER));
        assert_eq!(
            fs::metadata(temp.0.join("provider-claude-fixture.json"))
                .unwrap()
                .mode()
                & 0o7777,
            0o600
        );
        assert_eq!(
            receipt(&temp.0, (dev, ino), 3, 2, || Ok(())),
            Err(wire::Error::Unavailable)
        );
        assert_eq!(
            fs::read(temp.0.join("provider-claude-fixture.json")).unwrap(),
            bytes
        );
    }
    #[test]
    fn final_retirement_invalidates_created_receipt_and_preserves_previous_evidence() {
        use std::sync::atomic::AtomicUsize;
        let temp = Temp::new();
        let calls = AtomicUsize::new(0);
        assert_eq!(
            receipt(&temp.0, temp.identity(), 2, 1, || {
                if calls.fetch_add(1, Ordering::SeqCst) >= 2 {
                    Err(wire::Error::StateChanged)
                } else {
                    Ok(())
                }
            }),
            Err(wire::Error::StateChanged)
        );
        assert!(fs::read(temp.0.join("provider-claude-fixture.json"))
            .unwrap()
            .is_empty());
        fs::remove_file(temp.0.join("provider-claude-fixture.json")).unwrap();
        receipt(&temp.0, temp.identity(), 2, 1, || Ok(())).unwrap();
        let before = fs::read(temp.0.join("provider-claude-fixture.json")).unwrap();
        assert_eq!(
            receipt(&temp.0, temp.identity(), 3, 2, || Err(
                wire::Error::StateChanged
            )),
            Err(wire::Error::StateChanged)
        );
        assert_eq!(
            fs::read(temp.0.join("provider-claude-fixture.json")).unwrap(),
            before
        );
    }

    #[test]
    fn failed_write_invalidates_original_inode_without_deleting_replacement() {
        use std::sync::atomic::AtomicUsize;
        let temp = Temp::new();
        let calls = AtomicUsize::new(0);
        let original = temp.0.join("captured.json");
        let named = temp.0.join("provider-claude-fixture.json");
        assert_eq!(
            receipt(&temp.0, temp.identity(), 2, 1, || {
                if calls.fetch_add(1, Ordering::SeqCst) == 2 {
                    fs::rename(&named, &original).unwrap();
                    fs::write(&named, b"replacement").unwrap();
                    Err(wire::Error::StateChanged)
                } else {
                    Ok(())
                }
            }),
            Err(wire::Error::StateChanged)
        );
        assert!(fs::read(original).unwrap().is_empty());
        assert_eq!(fs::read(named).unwrap(), b"replacement");
    }
}
