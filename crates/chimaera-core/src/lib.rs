//! Shared types and helpers for the chimaera daemon and CLI.

pub mod shellint;

use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Version of the chimaera workspace.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The project's repository URL (from `Cargo.toml`), e.g.
/// `https://github.com/<owner>/<repo>`. Used to locate the GitHub release a
/// build came from when auto-fetching the matching daemon for a remote host.
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// Build identity of this binary: `<git-short-hash>[-dirty].<build-unix-secs>`
/// (e.g. `ff52221-dirty.1783438290`), embedded by build.rs. Builds outside a
/// git checkout embed `unknown.<secs>`.
pub const BUILD_ID: &str = env!("CHIMAERA_BUILD_ID");

/// The source-identity part of a build id — everything before the build
/// timestamp (`ff52221-dirty.1783438290` → `ff52221-dirty`).
pub fn build_ref(id: &str) -> &str {
    id.rsplit_once('.').map_or(id, |(r, _)| r)
}

/// Whether two build ids denote the same source. Timestamps are deliberately
/// ignored: the same commit is cross-compiled for clusters (`just dist`) and
/// built natively (the connecting CLI/app) at different moments, and those
/// must compare equal or every connect would "update" the daemon forever.
/// `None` (a manifest that predates build ids) never matches — missing means
/// ancient by definition — and `unknown` hashes only match bytewise-identical
/// ids, never on hash alone.
pub fn builds_match(local: &str, remote: Option<&str>) -> bool {
    let Some(remote) = remote else { return false };
    if local == remote {
        return true;
    }
    let (l, r) = (build_ref(local), build_ref(remote));
    l == r && !l.is_empty() && !l.starts_with("unknown")
}

/// Per-user data directory (`~/.chimaera`), created on demand.
pub fn data_dir() -> PathBuf {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".chimaera");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("failed to create data dir {}: {e}", dir.display());
    }
    dir
}

/// Per-user config directory (`$XDG_CONFIG_HOME/chimaera`, else
/// `~/.config/chimaera`), created on demand. Holds user-editable files
/// (settings.json) as opposed to daemon-owned state in [`data_dir`].
pub fn config_dir() -> PathBuf {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(base) if !base.is_empty() => PathBuf::from(base),
        _ => dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config"),
    };
    let dir = base.join("chimaera");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("failed to create config dir {}: {e}", dir.display());
    }
    dir
}

/// Per-user runtime directory: `$XDG_RUNTIME_DIR/chimaera` if set, else
/// `/tmp/chimaera-$UID`. Created with mode 0700 on demand.
pub fn runtime_dir() -> PathBuf {
    let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(base) if !base.is_empty() => PathBuf::from(base).join("chimaera"),
        _ => PathBuf::from(format!(
            "/tmp/chimaera-{}",
            nix::unistd::Uid::current().as_raw()
        )),
    };
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    if let Err(e) = builder.create(&dir) {
        tracing::warn!("failed to create runtime dir {}: {e}", dir.display());
    }
    dir
}

/// Generate an auth token: 32 random bytes as lowercase hex (64 chars).
pub fn generate_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// On-disk record of a running chimaera daemon.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Manifest {
    pub hostname: String,
    pub port: u16,
    pub token: String,
    pub pid: u32,
    pub version: String,
    pub started_at: u64,
    /// Build id of the daemon binary ([`BUILD_ID`] at `serve` time). `None`
    /// on manifests written by builds that predate build ids — which is
    /// itself the signal: missing = ancient = outdated.
    #[serde(default)]
    pub build: Option<String>,
}

impl Manifest {
    /// Path of the manifest file: `data_dir()/manifest.json`.
    pub fn path() -> PathBuf {
        data_dir().join("manifest.json")
    }

    /// Load the manifest if it exists. `Ok(None)` when the file is absent.
    pub fn load() -> anyhow::Result<Option<Manifest>> {
        let contents = match std::fs::read_to_string(Self::path()) {
            Ok(c) => c,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Ok(Some(serde_json::from_str(&contents)?))
    }

    /// Atomically write the manifest (tmp file + rename), mode 0600.
    pub fn write(&self) -> anyhow::Result<()> {
        let path = Self::path();
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Remove the manifest file. Ok if it does not exist.
    pub fn remove() -> anyhow::Result<()> {
        match std::fs::remove_file(Self::path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Whether the recorded pid is alive (signal 0 probe).
    pub fn is_alive(&self) -> bool {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(self.pid as i32), None).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_64_lowercase_hex_chars() {
        let token = generate_token();
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')));
        assert_ne!(generate_token(), token, "tokens should be random");
    }

    #[test]
    fn manifest_round_trip() {
        let tmp_home =
            std::env::temp_dir().join(format!("chimaera-core-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp_home).unwrap();
        std::env::set_var("HOME", &tmp_home);

        assert!(Manifest::load().unwrap().is_none(), "no manifest yet");

        let manifest = Manifest {
            hostname: "testhost".to_string(),
            port: 43210,
            token: generate_token(),
            pid: std::process::id(),
            version: VERSION.to_string(),
            started_at: 1_750_000_000,
            build: Some(BUILD_ID.to_string()),
        };
        manifest.write().unwrap();

        let mode = std::fs::metadata(Manifest::path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "manifest should be chmod 0600");

        let loaded = Manifest::load().unwrap().unwrap();
        assert_eq!(loaded.hostname, manifest.hostname);
        assert_eq!(loaded.port, manifest.port);
        assert_eq!(loaded.token, manifest.token);
        assert_eq!(loaded.pid, manifest.pid);
        assert_eq!(loaded.version, manifest.version);
        assert_eq!(loaded.started_at, manifest.started_at);
        assert_eq!(loaded.build, manifest.build, "build id round-trips");
        assert!(loaded.is_alive(), "our own pid is alive");

        Manifest::remove().unwrap();
        assert!(Manifest::load().unwrap().is_none());
        Manifest::remove().unwrap(); // idempotent

        std::fs::remove_dir_all(&tmp_home).ok();
    }

    /// Manifests written before build ids existed (no `build` field) must
    /// keep parsing — a missing build is data ("ancient"), not an error.
    #[test]
    fn manifest_without_build_field_parses() {
        let old = r#"{
            "hostname": "cluster",
            "port": 9700,
            "token": "abc",
            "pid": 1234,
            "version": "0.0.1",
            "started_at": 1750000000
        }"#;
        let m: Manifest = serde_json::from_str(old).unwrap();
        assert_eq!(m.build, None, "missing field reads as None");
        assert_eq!(m.port, 9700);
    }

    #[test]
    fn build_id_has_source_ref_and_timestamp() {
        let (source, secs) = BUILD_ID.rsplit_once('.').expect("hash.time shape");
        assert!(!source.is_empty());
        assert!(
            secs.parse::<u64>().is_ok(),
            "timestamp is unix secs: {secs}"
        );
        assert_eq!(build_ref(BUILD_ID), source);
    }

    #[test]
    fn builds_match_ignores_timestamp_but_not_source() {
        // Same commit, different build moments (native app vs musl dist).
        assert!(builds_match("ff52221.100", Some("ff52221.999")));
        assert!(builds_match("ff52221-dirty.100", Some("ff52221-dirty.999")));
        assert!(builds_match("ff52221.100", Some("ff52221.100")));
        // Different source never matches.
        assert!(!builds_match("ff52221.100", Some("d4e587f.100")));
        assert!(!builds_match("ff52221.100", Some("ff52221-dirty.100")));
        // Missing = pre-build-id = ancient.
        assert!(!builds_match("ff52221.100", None));
        // Non-git builds only match bytewise-identical ids.
        assert!(!builds_match("unknown.100", Some("unknown.999")));
        assert!(builds_match("unknown.100", Some("unknown.100")));
    }
}
