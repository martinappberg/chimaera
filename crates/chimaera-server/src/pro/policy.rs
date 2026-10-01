//! Conservative transfer policy. A path denied here cannot be opted into by a
//! project ignore file; account and agent login material never leaves its host.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Component, Path};

pub(super) const MAX_FILE_BYTES: u64 = 100_000_000;
pub(super) const MAX_PATHS: usize = 100_000;
pub(super) const MAX_CONFIG_FILE: u64 = 8 * 1024 * 1024;
/// Dependency and cache folders a project rebuilds wherever it runs. As
/// untracked content they never travel, also where no `.gitignore` says so (a
/// plain folder has none): copying them would spend the storage allowance and
/// the path cap on bytes the other side recreates. A file the project TRACKS
/// in Git under one of these names still travels.
pub(super) const REBUILT_DIRS: [&str; 5] =
    ["node_modules", "target", ".venv", "__pycache__", ".cache"];

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

pub(super) fn sanitize(value: &mut Value, missing: &mut Vec<String>, workspace: Option<&Path>) {
    match value {
        Value::Object(map) => {
            map.retain(|key, value| {
                if matches!(key.as_str(), "mcp_servers" | "mcpServers") {
                    if let Value::Object(servers) = value {
                        servers.retain(|_, server| !host_only_mcp(server, workspace));
                        for server in servers.values_mut() {
                            sanitize_mcp(server, missing, workspace);
                        }
                        return true;
                    }
                    return false;
                }
                if secret_name(key) {
                    return false;
                }
                if key.eq_ignore_ascii_case("env") {
                    if let Value::Object(env) = value {
                        env.retain(|name, value| {
                            // Host runtime paths are recreated by the destination,
                            // not missing service credentials the user must supply.
                            if host_environment(name, value, workspace) {
                                return false;
                            }
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
                            if !benign && environment_name(name) {
                                note_missing(missing, name);
                            }
                            benign
                        });
                    } else {
                        return false;
                    }
                }
                if value.as_str().is_some_and(unsafe_string) {
                    return false;
                }
                sanitize(value, missing, workspace);
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
                        return false;
                    }
                }
                true
            });
            values.retain(|v| !v.as_str().is_some_and(unsafe_string));
            for value in values {
                sanitize(value, missing, workspace);
            }
        }
        _ => {}
    }
}

/// These are variable names, never values. Unknown secret-shaped fields still
/// pass through the general redactor; only this documented MCP reference survives.
fn sanitize_mcp(server: &mut Value, missing: &mut Vec<String>, workspace: Option<&Path>) {
    let reference = server.as_object_mut().and_then(|map| {
        map.remove("bearer_token_env_var")
            .filter(|value| value.as_str().is_some_and(environment_name))
    });
    sanitize(server, missing, workspace);
    if let (Some(map), Some(reference)) = (server.as_object_mut(), reference) {
        map.insert("bearer_token_env_var".into(), reference);
    }
}

fn environment_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphabetic() || index > 0 && byte.is_ascii_digit()
        })
}

fn host_environment(name: &str, value: &Value, workspace: Option<&Path>) -> bool {
    matches!(
        name,
        "HOME"
            | "USERPROFILE"
            | "CODEX_HOME"
            | "CLAUDE_CONFIG_DIR"
            | "TMPDIR"
            | "XDG_CONFIG_HOME"
            | "XDG_DATA_HOME"
            | "XDG_CACHE_HOME"
            | "XDG_RUNTIME_DIR"
    ) || matches!(name, "PATH" | "PYTHONPATH" | "VIRTUAL_ENV")
        && value
            .as_str()
            .is_some_and(|value| host_path_list(value, workspace))
}

fn host_path(value: &str, workspace: Option<&Path>) -> bool {
    let value = value.replace('\\', "/");
    if let Some(root) = workspace.and_then(Path::to_str) {
        let root = root.replace('\\', "/");
        if Path::new(&value).starts_with(&root)
            && !Path::new(&value)
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return false;
        }
    }
    let lower = value.to_ascii_lowercase();
    [
        "/Users/",
        "/home/",
        "/root/",
        "/Applications/",
        "/System/Applications/",
        "/private/var/folders/",
        "/var/folders/",
    ]
    .iter()
    .any(|prefix| value.starts_with(prefix))
        || lower.as_bytes().get(1) == Some(&b':')
            && (lower[2..].starts_with("/users/") || lower[2..].starts_with("/program files/"))
}

fn host_path_list(value: &str, workspace: Option<&Path>) -> bool {
    host_path(value, workspace)
        || value
            .split([':', ';'])
            .any(|part| host_path(part, workspace))
}

/// Device-bound app helpers and loopback services cannot become cloud MCPs by
/// copying their settings. Absolute paths within the copied project are valid.
fn host_only_mcp(server: &Value, workspace: Option<&Path>) -> bool {
    let command = server.get("command").and_then(Value::as_str);
    if command
        .is_some_and(|command| host_path(command, workspace) || command == "SkyComputerUseClient")
    {
        return true;
    }
    if server
        .get("args")
        .and_then(Value::as_array)
        .is_some_and(|args| {
            args.iter()
                .filter_map(Value::as_str)
                .any(|arg| host_path(arg, workspace))
        })
    {
        return true;
    }
    if server
        .get("env")
        .and_then(Value::as_object)
        .is_some_and(|env| {
            env.iter().any(|(name, value)| {
                (name.starts_with("NODE_REPL_")
                    || matches!(name.as_str(), "SKY_CUA_SERVICE_PATH" | "CODEX_CLI_PATH"))
                    && value
                        .as_str()
                        .is_some_and(|value| host_path_list(value, workspace))
            })
        })
    {
        return true;
    }
    server.get("url").and_then(Value::as_str)
        .and_then(|url| url.parse::<axum::http::Uri>().ok())
        .is_some_and(|uri| uri.host().is_some_and(|host| {
            host.trim_end_matches('.').eq_ignore_ascii_case("localhost")
                || host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|address| {
                    address.is_loopback() || address.is_unspecified()
                        || matches!(address, std::net::IpAddr::V6(v6) if v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback() || v4.is_unspecified()))
                })
        }))
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CloudProfile {
    /// Runs before conversations continue on a cloud machine. Only the user
    /// sets it (Chimaera Pro, `PUT /pro/profile`).
    #[serde(default)]
    pub setup_command: Option<String>,
    /// Additive: a setup command an agent proposed (`update_cloud_profile`).
    /// It never runs until the user confirms it, which moves it into
    /// `setup_command`; injected repository text cannot schedule execution
    /// on the credentialed cloud machine by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_setup_command: Option<String>,
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
                .iter()
                .chain(&self.pending_setup_command)
                .all(|s| s.len() <= 16 * 1024 && !contains_credential(s.as_bytes()))
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
        assert!(allowed_path(Path::new("skills/example/SKILL.md")));
        assert!(contains_credential(b"-----BEGIN OPENSSH PRIVATE KEY-----"));
        assert!(contains_credential(b"sk-abcdefghijklmnopqrstuv"));
        assert!(!contains_credential(b"task-list markdown"));
    }
    #[test]
    fn config_filter_removes_nested_secrets_and_reports_environment_names() {
        let mut config = serde_json::json!({"model":"codex", "mcp_servers":{"example":{"url":"https://mcp.test/", "env":{"API_TOKEN":"credential", "PATH":"/usr/bin", "CUSTOM_AUTH":"secret"}, "authorization":"Bearer secret"}}, "api_key":"credential", "safe":"hello"});
        let mut missing = Vec::new();
        sanitize(&mut config, &mut missing, None);
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
    fn metadata_redaction_is_not_an_environment_requirement() {
        let mut config = serde_json::json!({"name":"token help", "description":"use auth for this plugin", "api_key":"private", "args":["--token", "private"], "env":{"SERVICE_TOKEN":"private", "invalid-name":"private", "CODEX_HOME":"/Users/example/.codex", "PATH":"/Users/example/bin:/usr/bin"}});
        let mut omitted = Vec::new();
        sanitize(&mut config, &mut omitted, None);
        assert_eq!(omitted, ["SERVICE_TOKEN"]);
        assert!(config.get("name").is_none());
        assert!(config.get("description").is_none());
        assert!(!serde_json::to_string(&config).unwrap().contains("private"));
    }

    #[test]
    fn portable_mcp_configs_keep_references_but_not_credentials_or_desktop_helpers() {
        let mut config = serde_json::json!({"mcp_servers":{
            "node_repl":{"command":"/Applications/Codex.app/Contents/node_repl", "env":{"BROWSER_USE_AVAILABLE_BACKENDS":"desktop", "CODEX_HOME":"/Users/example/.codex"}},
            "goldfish":{"command":"/Users/example/.local/bin/goldfish-mcp"},
            "computer-use":{"command":"SkyComputerUseClient"},
            "loopback":{"url":"http://localhost:4000/mcp"},
            "docs":{"url":"https://docs.example/mcp", "bearer_token_env_var":"DOCS_TOKEN", "authorization":"private", "enabled":true},
            "portable":{"command":"npx", "args":["-y", "example-server"], "env":{"SERVICE_TOKEN":"private","NODE_ENV":"production"}},
            "literal":{"url":"https://literal.example/mcp", "bearer_token_env_var":"sk-abcdefghijklmnopqrstuv"}
        }});
        let mut omitted = Vec::new();
        sanitize(&mut config, &mut omitted, None);
        let servers = config["mcp_servers"].as_object().unwrap();
        assert_eq!(servers.len(), 3);
        assert_eq!(servers["docs"]["bearer_token_env_var"], "DOCS_TOKEN");
        assert_eq!(servers["docs"]["enabled"], true);
        assert_eq!(servers["portable"]["command"], "npx");
        assert_eq!(
            servers["portable"]["args"],
            serde_json::json!(["-y", "example-server"])
        );
        assert_eq!(servers["portable"]["env"]["NODE_ENV"], "production");
        assert!(servers["literal"].get("bearer_token_env_var").is_none());
        assert_eq!(omitted, ["SERVICE_TOKEN"]);
        assert!(!serde_json::to_string(&config).unwrap().contains("private"));
    }

    #[test]
    fn copied_project_paths_remain_portable_but_device_paths_and_loopbacks_do_not() {
        for root in [
            "/Users/example/project",
            "/home/example/project",
            "C:/Users/example/project",
        ] {
            let mut config = serde_json::json!({"mcp_servers":{
                "project_script":{"command":"node", "args":[format!("{root}/tools/mcp.js")]},
                "project_binary":{"command":format!("{root}/tools/mcp"), "args":[root]},
                "escaped":{"command":format!("{root}/../.local/bin/helper")},
                "mac_app":{"command":"/Applications/Codex.app/Contents/node_repl"},
                "linux_local":{"command":"/home/example/.local/bin/goldfish-mcp"},
                "windows_local":{"command":"C:\\Users\\example\\AppData\\Local\\helper.exe"},
                "loopback4":{"url":"http://127.0.0.2:4000/mcp"},
                "loopback6":{"url":"http://[::1]:4000/mcp"},
                "unspecified4":{"url":"http://0.0.0.0:4000/mcp"},
                "unspecified6":{"url":"http://[::]:4000/mcp"}
            }});
            sanitize(&mut config, &mut Vec::new(), Some(Path::new(root)));
            let servers = config["mcp_servers"].as_object().unwrap();
            assert_eq!(servers.len(), 2, "{root}: {servers:?}");
            assert!(servers.contains_key("project_script"));
            assert!(servers.contains_key("project_binary"));
        }
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
