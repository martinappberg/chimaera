//! Public transaction host for the optional portable-configuration companion.
//! Live destination mutation, snapshots and recovery remain daemon-owned.
use super::{companion, config_wire as wire, policy, transport};
use anyhow::{ensure, Result};
use std::os::{
    fd::{AsRawFd, FromRawFd},
    unix::fs::{MetadataExt, OpenOptionsExt},
};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
const HELPER: &str = "/usr/local/bin/chimaera-pro-transfer";
// Logical host roles only; these numbers are never dup2 targets.
const HELPER_ROLE: i32 = 59;
const DIRECTORY_ROLES: [i32; 4] = [60, 61, 62, 63];
pub(super) use wire::Report;
pub(super) struct Sources {
    pub home: PathBuf,
    pub claude: PathBuf,
    pub codex: PathBuf,
    pub workspace: PathBuf,
}

/// High CLOEXEC source descriptors cannot alias spawn plumbing. Directory
/// roots are pinned before queuing; the executable is captured by the admitted
/// owner and all originals remain retained until helper settlement.
fn capture(path: &Path, directory: bool) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    let mut flags =
        rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::CLOEXEC;
    if directory {
        flags |= rustix::fs::OFlags::DIRECTORY;
    }
    options.custom_flags(flags.bits() as i32);
    let file = options.open(path)?;
    let meta = file.metadata()?;
    ensure!(
        if directory {
            meta.is_dir()
        } else {
            meta.is_file()
                && meta.mode() & 0o111 != 0
                && meta.mode() & 0o7022 == 0
                && (meta.uid() == 0 || meta.uid() == unsafe { nix::libc::geteuid() })
        },
        "transfer companion descriptor refused"
    );
    let fd = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 64) };
    ensure!(fd >= 64, "transfer companion descriptor unavailable");
    // fcntl returned a fresh owned descriptor; the original is not transferred.
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}
fn optional(path: &Path) -> Result<Option<fs::File>> {
    match fs::symlink_metadata(path) {
        Ok(_) => capture(&path.canonicalize()?, true).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
struct Captured {
    files: Vec<(i32, fs::File)>,
}
impl Captured {
    #[cfg(target_os = "linux")]
    fn command(self: &Arc<Self>) -> tokio::process::Command {
        let helper = self
            .files
            .iter()
            .find(|(role, _)| *role == HELPER_ROLE)
            .expect("captured helper role")
            .1
            .as_raw_fd();
        self.command_for(format!("/proc/self/fd/{helper}"))
    }
    fn command_for(self: &Arc<Self>, path: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(path);
        command.env_clear().env("LC_ALL", "C");
        let descriptors = self.clone();
        // Only exact owned descriptors lose CLOEXEC. No dup2 target can collide
        // with stdio, the spawn error pipe, or an unrelated daemon descriptor.
        unsafe {
            command.pre_exec(move || {
                for (_, file) in &descriptors.files {
                    if nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_SETFD, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        command
    }
}
enum HelperImage {
    Compatible(companion::CompatibleImage),
    // Only an explicitly selected ignored test can provide an original image.
    #[cfg(test)]
    CapturedFile(fs::File),
}
async fn dispatch(
    request: wire::Request,
    directories: Vec<(i32, fs::File)>,
    image: HelperImage,
) -> Result<Report> {
    ensure!(
        wire::valid_request(&request),
        "transfer companion request refused"
    );
    let bytes = serde_json::to_vec(&request)?;
    ensure!(
        bytes.len() <= wire::REQUEST_CAP,
        "transfer companion request exceeds limit"
    );
    // The fixed executable is captured/copied only inside the original admitted
    // owner. Queue timeout cannot orphan a prepared image or create a child.
    let prepare: transport::CompanionPrepare = Box::new(move |_| {
        let mut files = directories;
        let executable = match image {
            HelperImage::Compatible(image) => image.into_file()?,
            #[cfg(test)]
            HelperImage::CapturedFile(file) => file,
        };
        files.push((HELPER_ROLE, executable));
        let captured = Arc::new(Captured { files });
        #[cfg(target_os = "linux")]
        let prepared = transport::PreparedCompanion {
            file: None,
            command: captured.command(),
            cleanup: None,
        };
        #[cfg(target_os = "macos")]
        let prepared = {
            let source = &captured.files.last().expect("captured helper role").1;
            let stage = super::config_exec::Stage::copy(source)?;
            let mut command = captured.command_for(stage.path());
            stage.configure(&mut command);
            transport::PreparedCompanion {
                file: None,
                command,
                cleanup: Some(Box::new(move || stage.cleanup())),
            }
        };
        Ok(prepared)
    });
    let output = transport::run_companion(
        tokio::process::Command::new(HELPER),
        bytes,
        Duration::from_secs(10),
        wire::REPLY_CAP,
        prepare,
    )
    .await?;
    ensure!(output.success, "portable configuration companion failed");
    let report = parse_reply(&request, &output.stdout)?;
    Ok(report)
}
fn parse_reply(request: &wire::Request, bytes: &[u8]) -> Result<Report> {
    ensure!(
        bytes.len() <= wire::REPLY_CAP,
        "transfer companion receipt exceeds limit"
    );
    let reply: wire::Reply = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("transfer companion receipt refused"))?;
    ensure!(
        reply.version == wire::VERSION && wire::valid_report(&reply.report, request.budget),
        "transfer companion receipt refused"
    );
    if matches!(request.operation, wire::Operation::MergeStaged { .. }) {
        ensure!(
            reply.report.files == 0
                && reply.report.bytes == 0
                && reply.report.excluded == 0
                && reply.report.missing_environment.is_empty(),
            "transfer companion merge receipt refused"
        );
    }
    Ok(reply.report)
}

struct PreparedExport {
    request: wire::Request,
    files: Vec<(i32, fs::File)>,
    destination: PathBuf,
}
impl PreparedExport {
    async fn complete(self, image: HelperImage) -> Result<Report> {
        let budget = self.request.budget;
        let report = dispatch(self.request, self.files, image).await?;
        tokio::task::spawn_blocking(move || validate_stage(&self.destination, budget, None, true))
            .await??;
        Ok(report)
    }
}
pub(super) async fn export_with_image(
    sources: Sources,
    destination: &Path,
    budget: u64,
    image: companion::CompatibleImage,
) -> Result<Report> {
    capture_export(sources, destination, budget)
        .await?
        .complete(HelperImage::Compatible(image))
        .await
}
async fn capture_export(
    sources: Sources,
    destination: &Path,
    budget: u64,
) -> Result<PreparedExport> {
    let destination = destination.to_path_buf();
    let checked_destination = destination.clone();
    let workspace = sources.workspace.clone();
    let (files, home, claude, codex, output) = tokio::task::spawn_blocking(move || -> Result<_> {
        fs::create_dir_all(&destination)?;
        let mut files = vec![
            (
                DIRECTORY_ROLES[0],
                capture(&sources.home.canonicalize()?, true)?,
            ),
            (DIRECTORY_ROLES[3], capture(&destination, true)?),
        ];
        let claude = optional(&sources.claude)?;
        let codex = optional(&sources.codex)?;
        let identities = (
            files[0].1.as_raw_fd(),
            claude.as_ref().map(AsRawFd::as_raw_fd),
            codex.as_ref().map(AsRawFd::as_raw_fd),
            files[1].1.as_raw_fd(),
        );
        if let Some(file) = claude {
            files.push((DIRECTORY_ROLES[1], file));
        }
        if let Some(file) = codex {
            files.push((DIRECTORY_ROLES[2], file));
        }
        Ok((
            files,
            identities.0,
            identities.1,
            identities.2,
            identities.3,
        ))
    })
    .await??;
    Ok(PreparedExport {
        request: wire::Request {
            version: wire::VERSION,
            operation: wire::Operation::Export {
                home,
                claude,
                codex,
                output,
            },
            workspace,
            budget: budget.min(512 * 1024 * 1024),
        },
        files,
        destination: checked_destination,
    })
}

/// Public before-image selection and changes stay under the original hydration
/// owner. Only the private after-image is writable by the companion.
fn prepare_stage(source: &Path, home: &Path, stage: &Path, budget: u64) -> Result<()> {
    let mut incoming = std::collections::BTreeSet::new();
    let mut pending = vec![PathBuf::new()];
    let mut count = 0usize;
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(source.join(&dir))? {
            let entry = entry?;
            count += 1;
            ensure!(count <= 32_768, "configuration overlay exceeds limit");
            let relative = dir.join(entry.file_name());
            if !policy::allowed_path(&relative) {
                continue;
            }
            let permitted = relative.starts_with(".claude")
                || relative.starts_with(".codex")
                || relative == Path::new(".claude.json")
                || relative == Path::new(".gitconfig");
            if !permitted {
                continue;
            }
            let kind = entry.file_type()?;
            ensure!(
                !kind.is_symlink(),
                "configuration overlay contains a symlink"
            );
            if kind.is_dir() {
                pending.push(relative);
            } else if kind.is_file() {
                incoming.insert(relative);
            }
        }
    }
    let before = stage.join("before");
    let after = stage.join("after");
    super::install::snapshot(
        home,
        &before,
        &|path| {
            incoming
                .iter()
                .any(|target| target == path || target.starts_with(path))
        },
        budget,
    )?;
    super::install::snapshot(&before, &after, &|_| true, budget)?;
    Ok(())
}
struct PreparedImport {
    request: wire::Request,
    files: Vec<(i32, fs::File)>,
    home: PathBuf,
    stage: PathBuf,
}
impl PreparedImport {
    async fn complete(self, image: HelperImage) -> Result<Vec<super::install::Write>> {
        let budget = self.request.budget;
        dispatch(self.request, self.files, image).await?;
        tokio::task::spawn_blocking(move || {
            validate_stage(
                &self.stage.join("after"),
                budget,
                Some(&self.stage.join("before")),
                false,
            )?;
            super::install::changes(
                &self.home,
                &self.stage.join("before"),
                &self.stage.join("after"),
            )
        })
        .await?
    }
}
pub(super) async fn prepare_import_with_image(
    source: &Path,
    home: &Path,
    workspace: &Path,
    stage: &Path,
    budget: u64,
    image: companion::CompatibleImage,
) -> Result<Vec<super::install::Write>> {
    capture_import(source, home, workspace, stage, budget)
        .await?
        .complete(HelperImage::Compatible(image))
        .await
}
async fn capture_import(
    source: &Path,
    home: &Path,
    workspace: &Path,
    stage: &Path,
    budget: u64,
) -> Result<PreparedImport> {
    let (source, home, stage, workspace) = (
        source.to_path_buf(),
        home.to_path_buf(),
        stage.to_path_buf(),
        workspace.to_path_buf(),
    );
    let (original_home, original_stage) = (home.clone(), stage.clone());
    let files = tokio::task::spawn_blocking(move || -> Result<_> {
        prepare_stage(&source, &home, &stage, budget)?;
        Ok(vec![
            (DIRECTORY_ROLES[0], capture(&source, true)?),
            (DIRECTORY_ROLES[1], capture(&stage.join("after"), true)?),
        ])
    })
    .await??;
    Ok(PreparedImport {
        request: wire::Request {
            version: wire::VERSION,
            operation: wire::Operation::MergeStaged {
                overlay: files[0].1.as_raw_fd(),
                after: files[1].1.as_raw_fd(),
            },
            workspace,
            budget: budget.min(512 * 1024 * 1024),
        },
        files,
        home: original_home,
        stage: original_stage,
    })
}

/// A receipt cannot expand the captured home overlay into arbitrary live writes.
/// This is generic destination/path/storage validation; merge policy stays private.
fn validate_stage(root: &Path, budget: u64, before: Option<&Path>, exported: bool) -> Result<()> {
    let pinned = capture(root, true)?;
    let identity = pinned.metadata()?;
    let before = before.map(|path| capture(path, true)).transpose()?;
    let mut pending = vec![PathBuf::new()];
    let mut count = 0usize;
    let mut remaining = budget;
    while let Some(directory) = pending.pop() {
        let _directory = crate::download::open_beneath(
            &pinned,
            &directory,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
        )?;
        for entry in fs::read_dir(root.join(&directory))? {
            let entry = entry?;
            count += 1;
            ensure!(count <= 32768, "configuration stage exceeds path limit");
            let relative = directory.join(entry.file_name());
            let permitted = relative.starts_with(".claude")
                || relative.starts_with(".codex")
                || relative == Path::new(".claude.json")
                || relative == Path::new(".gitconfig")
                || exported && relative == Path::new("missing-environment.json");
            ensure!(
                permitted && policy::allowed_path(&relative) && relative.components().count() <= 64,
                "configuration stage path refused"
            );
            let mut file = crate::download::open_beneath(
                &pinned,
                &relative,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::NONBLOCK
                    | rustix::fs::OFlags::CLOEXEC,
            )?;
            let meta = file.metadata()?;
            if meta.is_dir() {
                pending.push(relative);
            } else {
                ensure!(
                    meta.is_file()
                        && meta.nlink() == 1
                        && meta.len() <= policy::MAX_CONFIG_FILE
                        && (exported && relative == Path::new("missing-environment.json")
                            || meta.len() <= remaining),
                    "configuration stage file refused"
                );
                let mut bytes = Vec::new();
                Read::by_ref(&mut file)
                    .take(policy::MAX_CONFIG_FILE + 1)
                    .read_to_end(&mut bytes)?;
                ensure!(
                    bytes.len() as u64 == meta.len(),
                    "configuration stage file changed"
                );
                let diagnostic = exported && relative == Path::new("missing-environment.json");
                if diagnostic {
                    ensure!(
                        bytes.len() <= 16 * 1024,
                        "configuration diagnostics exceed limit"
                    );
                } else {
                    remaining -= meta.len();
                }
                let previous = if let Some(before) = &before {
                    match crate::download::open_beneath(
                        before,
                        &relative,
                        rustix::fs::OFlags::RDONLY
                            | rustix::fs::OFlags::NOFOLLOW
                            | rustix::fs::OFlags::NONBLOCK
                            | rustix::fs::OFlags::CLOEXEC,
                    ) {
                        Ok(mut old) => {
                            ensure!(
                                old.metadata()?.is_file(),
                                "configuration before-image refused"
                            );
                            let mut bytes = Vec::new();
                            Read::by_ref(&mut old)
                                .take(policy::MAX_CONFIG_FILE + 1)
                                .read_to_end(&mut bytes)?;
                            ensure!(
                                bytes.len() as u64 <= policy::MAX_CONFIG_FILE,
                                "configuration before-image exceeds limit"
                            );
                            bytes
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                        Err(error) => return Err(error.into()),
                    }
                } else {
                    Vec::new()
                };
                ensure!(
                    !new_credentials(&previous, &bytes),
                    "configuration stage introduces credentials"
                );
            }
        }
    }
    let current = fs::symlink_metadata(root)?;
    ensure!(
        current.is_dir() && current.dev() == identity.dev() && current.ino() == identity.ino(),
        "configuration stage root changed"
    );
    Ok(())
}

/// Existing local login material can survive a preference merge, but no new
/// recognizable token/key may arrive through a companion's after-image.
fn credentials(bytes: &[u8]) -> std::collections::HashSet<&[u8]> {
    let mut values = std::collections::HashSet::new();
    for prefix in [
        b"sk-".as_slice(),
        b"ghp_",
        b"github_pat_",
        b"xoxb-",
        b"xoxp-",
        b"AKIA",
    ] {
        let mut offset = 0;
        while offset + prefix.len() <= bytes.len() {
            let Some(index) = bytes[offset..]
                .windows(prefix.len())
                .position(|word| word == prefix)
            else {
                break;
            };
            let start = offset + index;
            let end = start
                + prefix.len()
                + bytes[start + prefix.len()..]
                    .iter()
                    .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_' || **b == b'-')
                    .count();
            if end - start - prefix.len() >= 16 {
                values.insert(&bytes[start..end]);
            }
            offset = start + prefix.len();
        }
    }
    let marker = b"PRIVATE KEY-----";
    let mut offset = 0;
    while offset + marker.len() <= bytes.len() {
        let Some(index) = bytes[offset..]
            .windows(marker.len())
            .position(|word| word == marker)
        else {
            break;
        };
        let marker_start = offset + index;
        let start = bytes[..marker_start]
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |i| i + 1);
        let body = marker_start + marker.len();
        let end = bytes[body..]
            .windows(marker.len())
            .position(|word| word == marker)
            .map_or(bytes.len(), |i| body + i + marker.len());
        values.insert(&bytes[start..end]);
        offset = end;
    }
    values
}
fn new_credentials(before: &[u8], after: &[u8]) -> bool {
    if !policy::contains_credential(after) {
        return false;
    }
    let original = credentials(before);
    let current = credentials(after);
    if current.is_empty() {
        return true;
    }
    current.iter().any(|value| !original.contains(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Root supplies a frozen ordinary private helper, never a production resolver.
    #[tokio::test]
    #[ignore = "requires the explicit frozen private helper artifact and SHA256"]
    async fn actual_private_helper_export_and_staged_merge_keep_original_captured_inputs() {
        use sha2::{Digest, Sha256};
        use std::io::Write;
        use std::os::unix::fs::{symlink, DirBuilderExt, PermissionsExt};

        let artifact = PathBuf::from(
            std::env::var_os("CHIMAERA_TEST_PRO_TRANSFER").expect("fixed helper artifact required"),
        );
        let expected = std::env::var("CHIMAERA_TEST_PRO_TRANSFER_SHA256")
            .expect("frozen helper SHA256 required");
        assert!(expected.len() == 64 && expected.bytes().all(|b| b.is_ascii_hexdigit()));
        let mut original = capture(&artifact, false).unwrap();
        let identity = original.metadata().unwrap();
        assert!(identity.len() > 0 && identity.len() <= 128 * 1024 * 1024);
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "chimaera-transfer-composed-{}",
            chimaera_core::generate_token()
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let helper = root.join("helper");
        let mut copy = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&helper)
            .unwrap();
        assert_eq!(
            std::io::copy(
                &mut Read::by_ref(&mut original).take(128 * 1024 * 1024 + 1),
                &mut copy
            )
            .unwrap(),
            identity.len()
        );
        copy.sync_all().unwrap();
        copy.set_permissions(fs::Permissions::from_mode(0o500))
            .unwrap();
        drop(copy);
        let after = original.metadata().unwrap();
        assert_eq!(
            (
                identity.dev(),
                identity.ino(),
                identity.len(),
                identity.mtime(),
                identity.mtime_nsec(),
                identity.ctime(),
                identity.ctime_nsec()
            ),
            (
                after.dev(),
                after.ino(),
                after.len(),
                after.mtime(),
                after.mtime_nsec(),
                after.ctime(),
                after.ctime_nsec()
            )
        );
        let mut copied = fs::File::open(&helper).unwrap();
        let mut digest = Sha256::new();
        let mut block = [0u8; 64 * 1024];
        loop {
            let length = copied.read(&mut block).unwrap();
            if length == 0 {
                break;
            }
            digest.update(&block[..length]);
        }
        assert_eq!(
            digest
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            expected.to_ascii_lowercase()
        );
        drop(copied);
        let export_image = capture(&helper, false).unwrap();
        let merge_image = capture(&helper, false).unwrap();
        fs::rename(&helper, root.join("original-helper")).unwrap();
        // A replacement must never execute, even on the named-image Mac path.
        let mut replacement = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&helper)
            .unwrap();
        writeln!(
            replacement,
            "#!/bin/sh\nprintf wrong > '{}'/replacement-executed\nexit 73",
            root.display()
        )
        .unwrap();
        drop(replacement);

        for name in ["home", "claude", "codex"] {
            fs::create_dir_all(root.join("source").join(name)).unwrap();
        }
        let settings = serde_json::json!({
            "model":"incoming", "env":{"SECRET_TOKEN":"sk-syntheticabcdefghijklmnopqrstuv"},
            "mcpServers":{"service":{"url":"https://fixture.invalid/mcp","timeout":20}}
        });
        fs::write(
            root.join("source/claude/settings.json"),
            serde_json::to_vec(&settings).unwrap(),
        )
        .unwrap();
        fs::write(
            root.join("source/claude/.credentials.json"),
            b"excluded-login",
        )
        .unwrap();
        fs::write(root.join("outside"), b"outside-selection").unwrap();
        symlink(root.join("outside"), root.join("source/claude/CLAUDE.md")).unwrap();
        fs::write(
            root.join("source/codex/config.toml"),
            b"model = \"fixture-model\"\n",
        )
        .unwrap();
        fs::write(root.join("source/codex/AGENTS.md"), b"fixture instructions").unwrap();
        fs::write(root.join("source/codex/auth.json"), b"excluded-login").unwrap();
        fs::write(root.join("source/home/.gitconfig"), b"[user]\nname = Fixture\nemail = fixture@example.invalid\n[credential]\nhelper = excluded\n").unwrap();
        let overlay = root.join("overlay");
        let budget = 1024 * 1024;
        let prepared = capture_export(
            Sources {
                home: root.join("source/home"),
                claude: root.join("source/claude"),
                codex: root.join("source/codex"),
                workspace: root.join("workspace"),
            },
            &overlay,
            budget,
        )
        .await
        .unwrap();
        fs::rename(root.join("source"), root.join("original-source")).unwrap();
        for name in ["home", "claude", "codex"] {
            fs::create_dir_all(root.join("source").join(name)).unwrap();
        }
        fs::write(
            root.join("source/claude/settings.json"),
            b"{\"model\":\"replacement\"}",
        )
        .unwrap();
        let report = prepared
            .complete(HelperImage::CapturedFile(export_image))
            .await
            .unwrap();
        assert_eq!(report.files, 4);
        assert!(report.bytes > 0 && report.bytes <= budget && report.excluded > 0);
        let exported: serde_json::Value =
            serde_json::from_slice(&fs::read(overlay.join(".claude/settings.json")).unwrap())
                .unwrap();
        assert_eq!(exported["model"], "incoming");
        assert!(exported["env"].get("SECRET_TOKEN").is_none());
        for relative in [
            ".claude/.credentials.json",
            ".codex/auth.json",
            ".claude/CLAUDE.md",
        ] {
            assert!(!overlay.join(relative).exists());
        }
        assert_eq!(
            fs::read(overlay.join(".codex/AGENTS.md")).unwrap(),
            b"fixture instructions"
        );
        assert!(
            !String::from_utf8(fs::read(overlay.join(".gitconfig")).unwrap())
                .unwrap()
                .contains("helper")
        );
        assert_eq!(
            fs::read(root.join("original-source/claude/settings.json")).unwrap(),
            serde_json::to_vec(&settings).unwrap()
        );

        let home = root.join("destination");
        fs::create_dir_all(home.join(".claude")).unwrap();
        let local = serde_json::json!({"model":"local", "mcpServers":{"service":{
            "url":"https://fixture.invalid/mcp", "timeout":10,
            "env":{"API_TOKEN":"sk-destinationabcdefghijklmnopqrstuv"}
        }}});
        let local_bytes = serde_json::to_vec(&local).unwrap();
        fs::write(home.join(".claude/settings.json"), &local_bytes).unwrap();
        fs::write(home.join(".claude/.credentials.json"), b"destination-login").unwrap();
        let stage = root.join("stage");
        let prepared = capture_import(&overlay, &home, &root.join("workspace"), &stage, budget)
            .await
            .unwrap();
        fs::rename(&overlay, root.join("original-overlay")).unwrap();
        fs::create_dir_all(overlay.join(".claude")).unwrap();
        fs::write(
            overlay.join(".claude/settings.json"),
            b"{\"model\":\"replacement\"}",
        )
        .unwrap();
        let writes = prepared
            .complete(HelperImage::CapturedFile(merge_image))
            .await
            .unwrap();
        assert!(!writes.is_empty());
        assert!(writes
            .iter()
            .all(|write| write.root == home && write.relative.is_relative()));
        assert!(writes
            .iter()
            .any(|write| write.relative == Path::new(".claude/settings.json")));
        let merged: serde_json::Value =
            serde_json::from_slice(&fs::read(stage.join("after/.claude/settings.json")).unwrap())
                .unwrap();
        assert_eq!(merged["model"], "incoming");
        assert_eq!(merged["mcpServers"]["service"]["timeout"], 20);
        assert_eq!(
            merged["mcpServers"]["service"]["env"],
            local["mcpServers"]["service"]["env"]
        );
        assert_eq!(
            fs::read(home.join(".claude/settings.json")).unwrap(),
            local_bytes
        );
        assert_eq!(
            fs::read(home.join(".claude/.credentials.json")).unwrap(),
            b"destination-login"
        );
        assert_eq!(
            fs::read(stage.join("before/.claude/settings.json")).unwrap(),
            local_bytes
        );
        assert_eq!(
            fs::read(root.join("original-overlay/.claude/settings.json")).unwrap(),
            serde_json::to_vec_pretty(&exported).unwrap()
        );
        assert!(!home.join(".codex").exists());
        assert!(!home.join(".gitconfig").exists());
        assert!(!root.join("replacement-executed").exists());
        // The returned write plan is deliberately never applied to live home.
        drop(writes);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn staging_failure_never_mutates_live_destination_or_issues_changes() {
        // The installer rejects symlink ancestors; macOS temp_dir uses /var.
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "chimaera-transfer-host-{}",
            chimaera_core::generate_token()
        ));
        fs::create_dir_all(root.join("home/.claude")).unwrap();
        fs::create_dir_all(root.join("source/.claude")).unwrap();
        let original = b"{\"model\":\"local\",\"env\":{\"SECRET_TOKEN\":\"local-only\"}}";
        fs::write(root.join("home/.claude/settings.json"), original).unwrap();
        fs::write(
            root.join("source/.claude/settings.json"),
            b"{\"model\":\"incoming\"}",
        )
        .unwrap();
        prepare_stage(
            &root.join("source"),
            &root.join("home"),
            &root.join("stage"),
            1024 * 1024,
        )
        .unwrap();
        // A failed/hostile companion may damage its private after-image only.
        fs::write(root.join("stage/after/.claude/settings.json"), b"invalid").unwrap();
        assert_eq!(
            fs::read(root.join("home/.claude/settings.json")).unwrap(),
            original
        );
        assert_eq!(
            fs::read(root.join("stage/before/.claude/settings.json")).unwrap(),
            original
        );
        fs::write(root.join("stage/after/arbitrary-host-file"), b"refuse").unwrap();
        assert!(validate_stage(
            &root.join("stage/after"),
            1024 * 1024,
            Some(&root.join("stage/before")),
            false
        )
        .is_err());
        assert_eq!(
            fs::read(root.join("home/.claude/settings.json")).unwrap(),
            original
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn closed_receipt_rejects_hostile_malformed_or_unbounded_fields() {
        for bytes in [
            b"null".as_slice(),
            b"{}",
            b"{\"version\":1,\"report\":{},\"command\":\"sh\"}",
        ] {
            assert!(serde_json::from_slice::<wire::Reply>(bytes).is_err());
        }
        let mut report = Report {
            files: 32769,
            ..Report::default()
        };
        assert!(!wire::valid_report(&report, 100));
        report.files = 0;
        report.missing_environment = vec!["not-a-variable".into()];
        assert!(!wire::valid_report(&report, 100));
        report.missing_environment.clear();
        report.bytes = 101;
        assert!(!wire::valid_report(&report, 100));
    }
    #[test]
    fn merged_preferences_keep_only_exact_original_credentials() {
        let original = b"{\"model\":\"old\",\"token\":\"sk-abcdefghijklmnopqrstuv\"}";
        assert!(!new_credentials(
            original,
            b"{\"model\":\"new\",\"token\":\"sk-abcdefghijklmnopqrstuv\"}"
        ));
        assert!(new_credentials(original, b"sk-differentabcdefghijklmnop"));
        for token in [
            b"ghp_abcdefghijklmnopqrstuv".as_slice(),
            b"github_pat_abcdefghijklmnopqrstuv",
            b"xoxb-abcdefghijklmnopqrstuv",
            b"xoxp-abcdefghijklmnopqrstuv",
            b"AKIAabcdefghijklmnopqrstuv",
        ] {
            assert!(new_credentials(b"", token));
            assert!(!new_credentials(token, token));
        }
        assert!(new_credentials(
            b"",
            b"-----BEGIN OPENSSH PRIVATE KEY-----\nchanged\n-----END OPENSSH PRIVATE KEY-----"
        ));
        let key =
            b"-----BEGIN OPENSSH PRIVATE KEY-----\noriginal\n-----END OPENSSH PRIVATE KEY-----";
        assert!(!new_credentials(key, key));
        assert!(new_credentials(
            key,
            b"-----BEGIN OPENSSH PRIVATE KEY-----\nreplacement\n-----END OPENSSH PRIVATE KEY-----"
        ));
    }
    #[test]
    fn missing_companion_never_has_an_export_fallback() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-transfer-missing-{}",
            chimaera_core::generate_token()
        ));
        assert!(capture(&root, false).is_err());
    }
    #[cfg(target_os = "linux")]
    fn shell() -> fs::File {
        capture(&Path::new("/bin/sh").canonicalize().unwrap(), false).unwrap()
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn captured_descriptor_exec_and_directory_projection_work_without_installation() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-transfer-descriptors-{}",
            chimaera_core::generate_token()
        ));
        fs::create_dir(&root).unwrap();
        let captured = Arc::new(Captured {
            files: vec![
                (HELPER_ROLE, shell()),
                (DIRECTORY_ROLES[0], capture(&root, true).unwrap()),
            ],
        });
        let mut command = captured.command();
        let directory = captured
            .files
            .iter()
            .find(|(role, _)| *role == DIRECTORY_ROLES[0])
            .unwrap()
            .1
            .as_raw_fd();
        let script=format!("test -d /dev/fd/{directory} && printf '%s' '{{\"version\":1,\"report\":{{\"files\":0,\"bytes\":0,\"excluded\":0,\"missing_environment\":[]}}}}'");
        command.args(["-c", &script]);
        let output = transport::run(command, Vec::new(), Duration::from_secs(2), wire::REPLY_CAP)
            .await
            .unwrap();
        assert!(output.success);
        let request = wire::Request {
            version: wire::VERSION,
            operation: wire::Operation::MergeStaged {
                overlay: directory,
                after: captured.files[0].1.as_raw_fd(),
            },
            workspace: root.clone(),
            budget: 100,
        };
        assert!(parse_reply(&request, &output.stdout).is_ok());
        assert!(parse_reply(&request, b"not a receipt").is_err());
        assert!(parse_reply(&request, &vec![b' '; wire::REPLY_CAP + 1]).is_err());
        drop(captured);
        fs::remove_dir(root).unwrap();
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn canceled_observer_retains_original_cache_until_actual_helper_cleanup() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-transfer-cancel-{}",
            chimaera_core::generate_token()
        ));
        fs::create_dir(&root).unwrap();
        let marker = root.join("started");
        let mutex = Arc::new(tokio::sync::Mutex::new(()));
        let guard = Arc::new(mutex.clone().lock_owned().await);
        let captured = Arc::new(Captured {
            files: vec![(HELPER_ROLE, shell())],
        });
        let mut command = captured.command();
        command.args([
            "-c",
            &format!(
                "printf started > '{}'; exec /bin/sleep 30",
                marker.display()
            ),
        ]);
        let workspace = format!("transfer-test-{}", chimaera_core::generate_token());
        let task = tokio::spawn(async move {
            transport::cache_scope(
                &workspace,
                guard,
                transport::run(command, Vec::new(), Duration::from_secs(10), 32),
            )
            .await
        });
        let started = tokio::time::timeout(Duration::from_secs(2), async {
            while !tokio::fs::try_exists(&marker).await.unwrap() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(started.is_ok());
        assert!(mutex.try_lock().is_err());
        task.abort();
        let _ = task.await;
        // Observer cancellation cannot free this guard; only retained child cleanup can.
        let settled = tokio::time::timeout(Duration::from_secs(8), mutex.lock())
            .await
            .unwrap();
        drop(settled);
        drop(captured);
        fs::remove_dir_all(root).unwrap();
    }
}
