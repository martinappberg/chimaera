//! Conservative transfer policy. A path denied here cannot be opted into by a
//! project ignore file; account and agent login material never leaves its host.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Component, Path};

pub(super) const MAX_FILE_BYTES: u64 = 100_000_000;
pub(super) const MAX_PATHS: usize = 100_000;
pub(super) const MAX_CONFIG_FILE: u64 = 8 * 1024 * 1024;

pub(super) fn allowed_path(path: &Path) -> bool {
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
        ) || name == ".env"
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

pub(super) fn secret_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase().replace('-', "_");
    matches!(
        name.as_str(),
        "auth"
            | "authorization"
            | "cookie"
            | "cookies"
            | "credentials"
            | "password"
            | "passwd"
            | "secret"
            | "token"
            | "api_key"
            | "apikey"
            | "access_key"
            | "private_key"
    ) || [
        "_token",
        "_password",
        "_secret",
        "_api_key",
        "_access_key",
        "_private_key",
        "_credential",
    ]
    .iter()
    .any(|part| name.contains(part))
        || name.starts_with("auth_")
        || name.starts_with("bearer_")
}

pub(super) fn contains_credential(bytes: &[u8]) -> bool {
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

pub(super) fn sanitize(value: &mut Value, missing: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            map.retain(|key, value| {
                if secret_name(key) {
                    note_missing(missing, key);
                    return false;
                }
                if key.eq_ignore_ascii_case("env") {
                    if let Value::Object(env) = value {
                        env.retain(|name, value| {
                            let benign = matches!(
                                name.as_str(),
                                "PATH"
                                    | "NODE_ENV"
                                    | "LANG"
                                    | "LC_ALL"
                                    | "TZ"
                                    | "PYTHONPATH"
                                    | "VIRTUAL_ENV"
                            ) && !value
                                .as_str()
                                .is_some_and(|s| contains_credential(s.as_bytes()));
                            if !benign {
                                note_missing(missing, name);
                            }
                            benign
                        });
                    } else {
                        return false;
                    }
                }
                if value.as_str().is_some_and(unsafe_string) {
                    note_missing(missing, key);
                    return false;
                }
                sanitize(value, missing);
                true
            });
        }
        Value::Array(values) => {
            let mut skip_next = false;
            values.retain(|value| {
                if skip_next {
                    skip_next = false;
                    return false;
                }
                if let Some(flag) = value.as_str().and_then(|value| value.strip_prefix("--")) {
                    let (key, inline) = flag
                        .split_once('=')
                        .map_or((flag, false), |(key, _)| (key, true));
                    if secret_name(key) {
                        skip_next = !inline;
                        note_missing(missing, key);
                        return false;
                    }
                }
                true
            });
            values.retain(|v| !v.as_str().is_some_and(unsafe_string));
            for value in values {
                sanitize(value, missing);
            }
        }
        _ => {}
    }
}

fn unsafe_string(value: &str) -> bool {
    if value.split_whitespace().any(|part| {
        let part = part.trim_start_matches('-');
        part.split_once('=')
            .is_some_and(|(name, _)| secret_name(name))
            || (value.contains(' ') && secret_name(part))
    }) {
        return true;
    }
    if contains_credential(value.as_bytes()) {
        return true;
    }
    if let Ok(uri) = value.parse::<axum::http::Uri>() {
        if uri.authority().is_some_and(|a| a.as_str().contains('@')) {
            return true;
        }
        if uri.query().is_some_and(|query| {
            query
                .split('&')
                .any(|part| secret_name(part.split('=').next().unwrap_or_default()))
        }) {
            return true;
        }
    }
    false
}
fn note_missing(missing: &mut Vec<String>, name: &str) {
    if missing.len() < 128 && name.len() <= 128 && !missing.iter().any(|old| old == name) {
        missing.push(name.into());
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct CloudProfile {
    #[serde(default)]
    pub setup_command: Option<String>,
    #[serde(default)]
    pub laptop_only: Vec<String>,
    #[serde(default)]
    pub deferred: Vec<String>,
    #[serde(default)]
    pub missing_environment: Vec<String>,
}
impl CloudProfile {
    pub fn observe_command(&mut self, command: &str) {
        let command = command.trim();
        if command.is_empty() || command.len() > 2048 || contains_credential(command.as_bytes()) {
            return;
        }
        let first = command.split_whitespace().next().unwrap_or_default();
        let first = first.rsplit('/').next().unwrap_or(first);
        if matches!(
            first,
            "xcodebuild" | "xcrun" | "simctl" | "osascript" | "open" | "nvidia-smi" | "nvcc"
        ) && self.laptop_only.len() < 64
            && !self.laptop_only.iter().any(|old| old == command)
        {
            self.laptop_only.push(command.into());
        }
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.setup_command
                .as_ref()
                .is_none_or(|s| s.len() <= 16 * 1024 && !contains_credential(s.as_bytes()))
                && self.laptop_only.len() <= 64
                && self.deferred.len() <= 64
                && self.missing_environment.len() <= 128,
            "cloud profile exceeds limits or contains a credential"
        );
        anyhow::ensure!(
            self.laptop_only
                .iter()
                .chain(&self.deferred)
                .chain(&self.missing_environment)
                .all(|v| v.len() <= 2048 && !contains_credential(v.as_bytes())),
            "cloud profile entry exceeds limits or contains a credential"
        );
        Ok(())
    }
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
        ] {
            assert!(!allowed_path(Path::new(path)), "{path}");
        }
        assert!(allowed_path(Path::new("src/main.rs")));
        assert!(allowed_path(Path::new("skills/example/SKILL.md")));
        assert!(contains_credential(b"-----BEGIN OPENSSH PRIVATE KEY-----"));
        assert!(contains_credential(b"sk-abcdefghijklmnopqrstuv"));
        assert!(!contains_credential(b"task-list markdown"));
    }
    #[test]
    fn config_filter_removes_nested_secrets_and_reports_environment_names() {
        let mut config = serde_json::json!({"model":"codex", "mcp_servers":{"example":{"url":"https://mcp.test/", "env":{"API_TOKEN":"credential", "PATH":"/usr/bin", "CUSTOM_AUTH":"secret"}, "authorization":"Bearer secret"}}, "api_key":"credential", "safe":"hello"});
        let mut missing = Vec::new();
        sanitize(&mut config, &mut missing);
        assert!(config.get("api_key").is_none());
        assert!(config["mcp_servers"]["example"]
            .get("authorization")
            .is_none());
        assert_eq!(
            config["mcp_servers"]["example"]["env"],
            serde_json::json!({"PATH":"/usr/bin"})
        );
        assert!(missing.iter().any(|name| name == "API_TOKEN"));
        assert!(!serde_json::to_string(&config)
            .unwrap()
            .contains("credential"));
    }
    #[test]
    fn profile_learns_platform_steps_without_running_them() {
        let mut profile = CloudProfile::default();
        profile.observe_command("xcodebuild test");
        profile.observe_command("xcodebuild test");
        profile.observe_command("npm test");
        assert_eq!(profile.laptop_only, ["xcodebuild test"]);
        profile.validate().unwrap();
    }
}
