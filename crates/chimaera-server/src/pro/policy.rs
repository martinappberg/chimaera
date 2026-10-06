//! Conservative transfer policy. A path denied here cannot be opted into by a
//! project ignore file; account and agent login material never leaves its host.
use std::path::{Component, Path};

pub const MAX_FILE_BYTES: u64 = 100_000_000;
pub const MAX_PATHS: usize = 100_000;
pub(super) const MAX_CONFIG_FILE: u64 = 8 * 1024 * 1024;
/// Dependency and cache folders a project rebuilds wherever it runs. As
/// untracked content they never travel, also where no `.gitignore` says so (a
/// plain folder has none): copying them would spend the storage allowance and
/// the path cap on bytes the other side recreates. A file the project TRACKS
/// in Git under one of these names still travels.
pub const REBUILT_DIRS: [&str; 5] = ["node_modules", "target", ".venv", "__pycache__", ".cache"];

pub fn allowed_path(path: &Path) -> bool {
    if path.is_absolute() {
        return false;
    }
    for part in path.components() {
        let Component::Normal(name) = part else {
            return false;
        };
        let Some(name) = name.to_str() else {
            return false;
        };
        let name = name.to_ascii_lowercase();
        if matches!(
            name.as_str(),
            ".git"
                // A folder's workspace-identity marker names this computer;
                // it must never travel with a copy of the project.
                | ".chimaera-workspace"
                | ".ssh"
                | ".aws"
                | ".azure"
                | ".gnupg"
                | "auth.json"
                | ".credentials.json"
                | "credentials"
                | "credentials.json"
                | "keychain"
                | "keychains"
                | "application_default_credentials.json"
        ) || name.starts_with(crate::persist::PROJECT_STAGING_PREFIX)
            // A return's kept copy of the user's own version stays here.
            || super::canonical::kept_copy_name(&name)
            || name == ".env"
            || name.starts_with(".env.")
            || name.starts_with(".env-")
            || name.starts_with("id_rsa")
            || name.starts_with("id_ed25519")
            || [".pem", ".p12", ".pfx", ".key", ".keychain", ".keychain-db"]
                .iter()
                .any(|suffix| name.ends_with(suffix))
        {
            return false;
        }
    }
    true
}

pub fn contains_credential(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    if text.contains("PRIVATE KEY-----") {
        return true;
    }
    for prefix in ["sk-", "ghp_", "github_pat_", "xoxb-", "xoxp-", "AKIA"] {
        for (offset, _) in text.match_indices(prefix) {
            let rest = &text[offset + prefix.len()..];
            if rest
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
                .count()
                >= 16
            {
                return true;
            }
        }
    }
    false
}

/// Environment variable NAMES a move's agent-configuration export left out
/// (values never travel): written by the sender, never by an agent, and only
/// ever shown to agents as names.
pub fn validate_missing_environment(names: &[String]) -> anyhow::Result<()> {
    anyhow::ensure!(
        names.len() <= 128
            && names
                .iter()
                .all(|v| v.len() <= 2048 && !contains_credential(v.as_bytes())),
        "missing environment names exceed limits or contain a credential"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_and_path_escapes_cannot_be_mirrored() {
        for path in [
            "../auth.json",
            "/etc/passwd",
            ".env",
            ".env.production",
            "nested/.ssh/id_rsa",
            "skills/auth.json",
            "plugin/.credentials.json",
            "tls/private.pem",
            ".git/config",
            // The workspace identity marker, at any depth (a nested project's
            // own marker must not travel either).
            ".chimaera-workspace",
            "nested/project/.chimaera-workspace",
            ".git/chimaera-workspace",
        ] {
            assert!(!allowed_path(Path::new(path)), "{path}");
        }
        // Only the marker's exact name: neighbours are ordinary files.
        assert!(allowed_path(Path::new("docs/chimaera-workspace.md")));
        assert!(allowed_path(Path::new("chimaera-workspace")));
        assert!(allowed_path(Path::new("src/main.rs")));
        // A filename resembling a kept-copy suffix can still be Unicode.
        assert!(allowed_path(Path::new("docs/a.mine-123456789012é")));
        assert!(allowed_path(Path::new("skills/example/SKILL.md")));
        assert!(contains_credential(b"-----BEGIN OPENSSH PRIVATE KEY-----"));
        assert!(contains_credential(b"sk-abcdefghijklmnopqrstuv"));
        assert!(!contains_credential(b"task-list markdown"));
    }
}
