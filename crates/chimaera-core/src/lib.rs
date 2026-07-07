//! Shared types and helpers for the chimaera daemon and CLI.

use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Version of the chimaera workspace.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

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
        assert!(loaded.is_alive(), "our own pid is alive");

        Manifest::remove().unwrap();
        assert!(Manifest::load().unwrap().is_none());
        Manifest::remove().unwrap(); // idempotent

        std::fs::remove_dir_all(&tmp_home).ok();
    }
}
