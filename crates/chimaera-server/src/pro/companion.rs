//! Host-only installed companion identity. No private policy or execution authority.
use anyhow::{ensure, Context, Result};
use rustix::fs::{self as at, Mode, OFlags};
use serde::Deserialize;
#[cfg(any(test, feature = "daemon-extension-fixture"))]
use serde::Serialize;
use sha2::Digest;
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
};
const IMAGE_CAP: u64 = 128 * 1024 * 1024;
const METADATA_CAP: usize = 4096;
const PACKAGED: &str = "/usr/local/bin";
#[derive(Deserialize)]
#[cfg_attr(any(test, feature = "daemon-extension-fixture"), derive(Serialize))]
#[serde(deny_unknown_fields)]
struct Metadata {
    schema: u8,
    role: String,
    daemon_version: String,
    daemon_build: String,
    wire_version: u8,
    target: String,
    length: u64,
    sha256: String,
}
#[derive(PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    bytes: u64,
    mode: u32,
    uid: u32,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Identity {
    fn of(m: &fs::Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
            bytes: m.len(),
            mode: m.mode(),
            uid: m.uid(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        }
    }
}
pub(super) struct CompatibleImage {
    file: File,
    expected: Metadata,
    // Capture retains the exact original install directory, not a later alias.
    _directory: File,
}
fn flags(directory: bool) -> OFlags {
    OFlags::RDONLY
        | OFlags::NOFOLLOW
        | OFlags::NONBLOCK
        | OFlags::CLOEXEC
        | if directory {
            OFlags::DIRECTORY
        } else {
            OFlags::empty()
        }
}
fn trusted(m: &fs::Metadata) -> bool {
    (m.uid() == 0 || m.uid() == unsafe { nix::libc::geteuid() }) && m.mode() & 0o7022 == 0
}
fn target() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("darwin-aarch64"),
        ("macos", "x86_64") => Ok("darwin-x86_64"),
        ("linux", "aarch64") => Ok("linux-aarch64"),
        ("linux", "x86_64") => Ok("linux-x86_64"),
        _ => anyhow::bail!("companion platform unsupported"),
    }
}
fn atom(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes().any(|b| b.is_ascii_alphanumeric())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn validate(m: &Metadata) -> Result<()> {
    ensure!(
        m.schema == 1
            && m.role == "portable_configuration"
            && m.daemon_version == chimaera_core::VERSION
            && atom(&m.daemon_build)
            && chimaera_core::builds_match(chimaera_core::BUILD_ID, Some(&m.daemon_build))
            && m.wire_version == super::config_wire::VERSION as u8
            && m.target == target()?
            && m.length > 0
            && m.length <= IMAGE_CAP
            && m.sha256.len() == 64
            && m.sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "installed companion metadata incompatible"
    );
    Ok(())
}
fn high(file: File) -> Result<File> {
    let fd = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 64) };
    ensure!(fd >= 64, "installed companion descriptor unavailable");
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn leaf(directory: &File, name: &str) -> Result<File> {
    Ok(File::from(at::openat(
        directory,
        name,
        flags(false),
        Mode::empty(),
    )?))
}
fn read_metadata(file: &mut File, managed: bool) -> Result<Metadata> {
    let before = file.metadata()?;
    ensure!(
        before.is_file()
            && trusted(&before)
            && before.len() <= METADATA_CAP as u64
            && (!managed || before.mode() & 0o7777 == 0o400),
        "installed companion metadata refused"
    );
    let mut bytes = Vec::new();
    Read::by_ref(file)
        .take((METADATA_CAP + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= METADATA_CAP && Identity::of(&before) == Identity::of(&file.metadata()?),
        "installed companion metadata changed"
    );
    let m: Metadata = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("installed companion metadata refused"))?;
    validate(&m)?;
    Ok(m)
}
impl CompatibleImage {
    pub(super) fn verify(&self) -> Result<()> {
        let before = self.file.metadata()?;
        ensure!(
            before.is_file()
                && trusted(&before)
                && before.mode() & 0o111 != 0
                && before.len() == self.expected.length,
            "installed companion image refused"
        );
        let mut file = self.file.try_clone()?;
        file.seek(SeekFrom::Start(0))?;
        let mut hash = sha2::Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0u8; 16384];
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            bytes = bytes
                .checked_add(n as u64)
                .context("installed companion size overflow")?;
            ensure!(
                bytes <= IMAGE_CAP && bytes <= self.expected.length,
                "installed companion image changed"
            );
            hash.update(&buffer[..n]);
        }
        let digest: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        ensure!(
            bytes == self.expected.length
                && digest == self.expected.sha256
                && Identity::of(&before) == Identity::of(&self.file.metadata()?),
            "installed companion image changed"
        );
        Ok(())
    }
    pub(super) fn into_file(self) -> Result<File> {
        self.verify()?;
        Ok(self.file)
    }
}
fn anchor(path: &Path) -> Result<Option<File>> {
    ensure!(
        path.is_absolute() && path.as_os_str().len() <= 8192,
        "installed companion anchor refused"
    );
    let resolved = match path.canonicalize() {
        Ok(path) => path,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A present dangling alias is invalid, not an absent installation.
            match fs::symlink_metadata(path) {
                Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                _ => return Err(anyhow::anyhow!("installed companion anchor unavailable")),
            }
        }
        Err(e) => return Err(e.into()),
    };
    let before = fs::symlink_metadata(&resolved)?;
    ensure!(
        before.is_dir() && trusted(&before),
        "installed companion anchor refused"
    );
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags(true).bits() as i32)
        .open(&resolved)?;
    let actual = file.metadata()?;
    ensure!(
        actual.is_dir()
            && trusted(&actual)
            && before.dev() == actual.dev()
            && before.ino() == actual.ino(),
        "installed companion anchor changed"
    );
    Ok(Some(file))
}
fn build() -> Result<&'static str> {
    let build = if chimaera_core::BUILD_ID.starts_with("unknown") {
        chimaera_core::BUILD_ID
    } else {
        chimaera_core::build_ref(chimaera_core::BUILD_ID)
    };
    ensure!(atom(build), "installed companion build refused");
    Ok(build)
}
fn managed(data: &Path) -> Result<Option<CompatibleImage>> {
    let Some(mut directory) = anchor(data)? else {
        return Ok(None);
    };
    for name in ["companions", "portable-config", build()?, target()?] {
        let child = match at::openat(&directory, name, flags(true), Mode::empty()) {
            Ok(fd) => File::from(fd),
            Err(e) if e == rustix::io::Errno::NOENT => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let m = child.metadata()?;
        ensure!(
            m.is_dir() && trusted(&m) && m.mode() & 0o7777 == 0o700,
            "installed companion directory refused"
        );
        directory = child;
    }
    capture_in(directory, "metadata.json", "helper", true).map(Some)
}
pub(super) fn capture() -> Result<CompatibleImage> {
    let companions = chimaera_core::managed_companions_dir();
    let data = companions
        .parent()
        .context("installed companion anchor unavailable")?;
    if let Some(image) = managed(data)? {
        return Ok(image);
    }
    let directory = anchor(Path::new(PACKAGED))?.context("installed companion unavailable")?;
    capture_in(
        directory,
        "chimaera-pro-transfer.metadata.json",
        "chimaera-pro-transfer",
        false,
    )
}

fn capture_in(
    directory: File,
    metadata_name: &str,
    image_name: &str,
    managed: bool,
) -> Result<CompatibleImage> {
    let mut metadata = leaf(&directory, metadata_name)?;
    let expected = read_metadata(&mut metadata, managed)?;
    let file = high(leaf(&directory, image_name)?)?;
    ensure!(
        !managed || file.metadata()?.mode() & 0o7777 == 0o500,
        "installed companion image refused"
    );
    let image = CompatibleImage {
        file,
        expected,
        _directory: directory,
    };
    image.verify()?;
    // Named leaves must still be the originals at capture. Later atomic
    // installation replacement cannot redirect these retained descriptors.
    ensure!(
        Identity::of(&metadata.metadata()?)
            == Identity::of(&leaf(&image._directory, metadata_name)?.metadata()?)
            && Identity::of(&image.file.metadata()?)
                == Identity::of(&leaf(&image._directory, image_name)?.metadata()?),
        "installed companion binding changed"
    );
    Ok(image)
}
pub(super) async fn preflight() -> Result<CompatibleImage> {
    super::transport::prepare_image(capture).await
}

/// Isolated optional-companion integrations re-execute with a different state
/// home. Publish only the original verified parent's bytes/closed receipt into
/// that child's ordinary managed path; never select another executable.
#[cfg(feature = "daemon-extension-fixture")]
pub(super) fn fixture_child(data: &Path) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let image = capture()?;
    image.verify()?;
    fs::create_dir_all(data)?;
    fs::set_permissions(data, fs::Permissions::from_mode(0o700))?;
    let mut parent = anchor(data)?.context("fixture companion anchor unavailable")?;
    for name in ["companions", "portable-config", build()?, target()?] {
        at::mkdirat(&parent, name, Mode::from_raw_mode(0o700))?;
        parent = File::from(at::openat(&parent, name, flags(true), Mode::empty())?);
    }
    let mut output = File::from(at::openat(
        &parent,
        "helper",
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o500),
    )?);
    let mut source = image.file.try_clone()?;
    source.seek(SeekFrom::Start(0))?;
    let bytes = std::io::copy(&mut source.take(IMAGE_CAP + 1), &mut output)?;
    ensure!(
        bytes == image.expected.length,
        "fixture companion copy changed"
    );
    output.sync_all()?;
    image.verify()?;
    let mut metadata = File::from(at::openat(
        &parent,
        "metadata.json",
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o400),
    )?);
    metadata.write_all(&serde_json::to_vec(&image.expected)?)?;
    metadata.sync_all()?;
    parent.sync_all()?;
    ensure!(
        managed(data)?.is_some(),
        "fixture companion publication refused"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    fn temp() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "chimaera-companion-{}",
            chimaera_core::generate_token()
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        root
    }
    fn install(data: &Path) -> std::path::PathBuf {
        let mut dir = data.to_path_buf();
        for name in [
            "companions",
            "portable-config",
            build().unwrap(),
            target().unwrap(),
        ] {
            dir.push(name);
            fs::create_dir(&dir).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let bytes = b"original captured fixture bytes";
        fs::write(dir.join("helper"), bytes).unwrap();
        fs::set_permissions(dir.join("helper"), fs::Permissions::from_mode(0o500)).unwrap();
        let m = Metadata {
            schema: 1,
            role: "portable_configuration".into(),
            daemon_version: chimaera_core::VERSION.into(),
            daemon_build: chimaera_core::BUILD_ID.into(),
            wire_version: 1,
            target: target().unwrap().into(),
            length: bytes.len() as u64,
            sha256: sha2::Sha256::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        };
        fs::write(dir.join("metadata.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        fs::set_permissions(dir.join("metadata.json"), fs::Permissions::from_mode(0o400)).unwrap();
        dir
    }
    #[test]
    fn captured_anchor_and_image_survive_alias_and_leaf_replacement() {
        let root = temp();
        let data = root.join("data");
        fs::create_dir(&data).unwrap();
        fs::set_permissions(&data, fs::Permissions::from_mode(0o700)).unwrap();
        let directory = install(&data);
        let alias = root.join("home-alias");
        symlink(&data, &alias).unwrap();
        let image = managed(&alias).unwrap().unwrap();
        fs::remove_file(&alias).unwrap();
        symlink(root.join("missing"), &alias).unwrap();
        fs::rename(directory.join("helper"), directory.join("old-helper")).unwrap();
        fs::write(directory.join("helper"), b"replacement must not be adopted").unwrap();
        let mut original = image.into_file().unwrap();
        original.seek(SeekFrom::Start(0)).unwrap();
        let mut bytes = Vec::new();
        original.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"original captured fixture bytes");
        assert!(managed(&alias).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn malformed_missing_unknown_and_changed_selected_images_refuse() {
        let root = temp();
        assert!(managed(&root).unwrap().is_none());
        let dir = install(&root);
        let image = managed(&root).unwrap().unwrap();
        fs::set_permissions(dir.join("helper"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(dir.join("helper"), b"modified captured fixture bytes").unwrap();
        fs::set_permissions(dir.join("helper"), fs::Permissions::from_mode(0o500)).unwrap();
        assert!(image.into_file().is_err());
        fs::set_permissions(dir.join("helper"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(dir.join("helper"), b"original captured fixture bytes").unwrap();
        fs::set_permissions(dir.join("helper"), fs::Permissions::from_mode(0o500)).unwrap();
        assert!(managed(&root).unwrap().is_some());
        let metadata = dir.join("metadata.json");
        let original = fs::read(&metadata).unwrap();
        for field in [
            "schema",
            "role",
            "wire_version",
            "daemon_version",
            "daemon_build",
            "target",
            "length",
            "sha256",
            "unknown",
        ] {
            let mut m: serde_json::Value = serde_json::from_slice(&original).unwrap();
            m[field] = match field {
                "schema" | "wire_version" => serde_json::json!(2),
                "length" => serde_json::json!(IMAGE_CAP + 1),
                "sha256" => serde_json::json!("0".repeat(64)),
                _ => serde_json::json!("untrusted-value"),
            };
            fs::set_permissions(&metadata, fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(&metadata, serde_json::to_vec(&m).unwrap()).unwrap();
            fs::set_permissions(&metadata, fs::Permissions::from_mode(0o400)).unwrap();
            let error = managed(&root)
                .err()
                .expect("invalid selected metadata refused");
            assert!(!error.to_string().contains("untrusted-value"));
        }
        fs::set_permissions(&metadata, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&metadata, vec![b' '; METADATA_CAP + 1]).unwrap();
        fs::set_permissions(&metadata, fs::Permissions::from_mode(0o400)).unwrap();
        assert!(managed(&root).is_err());
        fs::remove_file(&metadata).unwrap();
        assert!(managed(&root).is_err());
        fs::remove_file(dir.join("helper")).unwrap();
        symlink("missing", dir.join("helper")).unwrap();
        fs::write(&metadata, &original).unwrap();
        fs::set_permissions(&metadata, fs::Permissions::from_mode(0o400)).unwrap();
        assert!(managed(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
