//! Export selected agent configuration as a home-relative overlay. Login files
//! are never inputs; symlinks cannot escape the selected roots.
use super::policy;
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::{
    fs,
    io::{BufRead, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Default, Serialize)]
pub(super) struct Report {
    pub files: usize,
    pub bytes: u64,
    pub excluded: usize,
    pub missing_environment: Vec<String>,
}

pub(super) struct Sources {
    pub home: PathBuf,
    pub claude: PathBuf,
    pub codex: PathBuf,
    pub workspace: PathBuf,
}

pub(super) fn export(sources: Sources, destination: &Path, budget: u64) -> Result<Report> {
    fs::create_dir_all(destination)?;
    let mut job = Export {
        destination,
        workspace: &sources.workspace,
        remaining: budget.min(512 * 1024 * 1024),
        visited: 0,
        started: Instant::now(),
        report: Report::default(),
    };
    for name in [
        "settings.json",
        "CLAUDE.md",
        "keybindings.json",
        "skills",
        "plugins",
    ] {
        job.copy_tree(&sources.claude.join(name), &Path::new(".claude").join(name))?;
    }
    for name in ["config.toml", "AGENTS.md", "instructions.md", "skills"] {
        job.copy_tree(&sources.codex.join(name), &Path::new(".codex").join(name))?;
    }
    job.copy_tree(
        &sources.home.join(".claude.json"),
        Path::new(".claude.json"),
    )?;
    // Only literal author identity travels; includes, helpers and signing keys
    // are host-specific and can contain credentials or execute commands.
    if let Ok(bytes) = read_regular(&sources.home.join(".gitconfig")) {
        let identity = git_identity(&bytes);
        if !identity.is_empty() && identity.len() as u64 <= job.remaining {
            fs::write(destination.join(".gitconfig"), &identity)?;
            job.remaining -= identity.len() as u64;
            job.report.bytes += identity.len() as u64;
            job.report.files += 1;
        }
    }
    let encoded = crate::launcher::encode_cwd(&sources.workspace);
    job.copy_tree(
        &sources.claude.join("projects").join(&encoded),
        &Path::new(".claude/projects").join(&encoded),
    )?;
    // Only rollouts whose real header names this mirrored project may travel.
    let sessions = sources.codex.join("sessions");
    job.rollouts(&sessions, &sessions, &sources.workspace, 0)?;
    fs::write(
        destination.join("missing-environment.json"),
        serde_json::to_vec(&job.report.missing_environment)?,
    )?;
    Ok(job.report)
}

struct Export<'a> {
    destination: &'a Path,
    workspace: &'a Path,
    remaining: u64,
    visited: usize,
    started: Instant,
    report: Report,
}
impl Export<'_> {
    fn tick(&mut self) -> Result<()> {
        self.visited += 1;
        ensure!(
            self.visited <= 32_768 && self.started.elapsed() < Duration::from_secs(10),
            "agent configuration scan exceeds its budget"
        );
        Ok(())
    }
    fn copy_tree(&mut self, source: &Path, relative: &Path) -> Result<()> {
        self.tick()?;
        if !policy::allowed_path(relative) {
            self.report.excluded += 1;
            return Ok(());
        }
        let metadata = match fs::symlink_metadata(source) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            self.report.excluded += 1;
            return Ok(());
        }
        if metadata.is_dir() {
            if relative.components().count() >= 16 {
                self.report.excluded += 1;
                return Ok(());
            }
            for entry in fs::read_dir(source)? {
                let entry = entry?;
                if ["node_modules", "target", ".venv", "__pycache__", ".cache"]
                    .iter()
                    .any(|name| entry.file_name() == *name)
                {
                    continue;
                }
                self.copy_tree(&entry.path(), &relative.join(entry.file_name()))?;
            }
        } else if metadata.is_file() {
            self.copy_file(source, relative, metadata.len())?;
        }
        Ok(())
    }
    fn copy_file(&mut self, source: &Path, relative: &Path, length: u64) -> Result<()> {
        if length > policy::MAX_CONFIG_FILE {
            self.report.excluded += 1;
            return Ok(());
        }
        ensure!(
            length <= self.remaining,
            "agent configuration exceeds mirror storage budget"
        );
        let mut bytes = read_regular(source)?;
        let extension = relative
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default();
        if extension == "json" {
            let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                self.report.excluded += 1;
                return Ok(());
            };
            self.sanitize_config(&mut value, relative);
            bytes = serde_json::to_vec_pretty(&value)?;
        } else if extension == "toml" {
            let Ok(text) = std::str::from_utf8(&bytes) else {
                self.report.excluded += 1;
                return Ok(());
            };
            let Ok(value) = toml::from_str::<toml::Value>(text) else {
                self.report.excluded += 1;
                return Ok(());
            };
            let mut json = serde_json::to_value(value)?;
            self.sanitize_config(&mut json, relative);
            bytes = write_toml(&json)?.into_bytes();
        }
        // This also guards hand-written instructions and plugin scripts whose
        // authors accidentally embedded recognizable login material.
        if policy::contains_credential(&bytes) {
            self.report.excluded += 1;
            return Ok(());
        }
        ensure!(
            bytes.len() as u64 <= self.remaining,
            "agent configuration exceeds mirror storage budget"
        );
        let target = self.destination.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        output.write_all(&bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            output.set_permissions(fs::Permissions::from_mode(
                0o600 | (fs::metadata(source)?.permissions().mode() & 0o111),
            ))?;
        }
        self.remaining -= bytes.len() as u64;
        self.report.bytes += bytes.len() as u64;
        self.report.files += 1;
        Ok(())
    }
    fn sanitize_config(&mut self, value: &mut serde_json::Value, relative: &Path) {
        // Redacted plugin metadata and examples are not active environment
        // requirements. Even active files can contain other projects' settings.
        let active = matches!(
            relative.to_str(),
            Some(".claude/settings.json" | ".claude.json" | ".codex/config.toml")
        ) || relative.file_name().is_some_and(|name| name == ".mcp.json");
        let mut declared = std::collections::HashSet::new();
        if active {
            let mut collect = |config: &serde_json::Value| {
                if let Some(env) = config.get("env").and_then(serde_json::Value::as_object) {
                    declared.extend(env.keys().filter(|name| name.len() <= 128).cloned());
                }
            };
            collect(value);
            for key in ["mcp_servers", "mcpServers"] {
                if let Some(servers) = value.get(key).and_then(serde_json::Value::as_object) {
                    for server in servers.values() {
                        collect(server);
                    }
                }
            }
        }
        let mut omitted = Vec::new();
        policy::sanitize(value, &mut omitted, Some(self.workspace));
        for name in omitted {
            if declared.contains(&name)
                && self.report.missing_environment.len() < 128
                && !self.report.missing_environment.contains(&name)
            {
                self.report.missing_environment.push(name);
            }
        }
    }

    fn rollouts(
        &mut self,
        directory: &Path,
        root: &Path,
        workspace: &Path,
        depth: usize,
    ) -> Result<()> {
        if depth > 3 {
            return Ok(());
        }
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            self.tick()?;
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                self.rollouts(&entry.path(), root, workspace, depth + 1)?;
            } else if kind.is_file()
                && entry.file_name().to_string_lossy().starts_with("rollout-")
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "jsonl")
            {
                let mut line = Vec::new();
                std::io::BufReader::new(fs::File::open(entry.path())?)
                    .take(64 * 1024)
                    .read_until(b'\n', &mut line)?;
                let Ok(header) = serde_json::from_slice::<serde_json::Value>(&line) else {
                    continue;
                };
                if header["type"] == "session_meta"
                    && header["payload"]["cwd"]
                        .as_str()
                        .is_some_and(|cwd| Path::new(cwd) == workspace)
                {
                    let path = entry.path();
                    let relative = Path::new(".codex/sessions").join(path.strip_prefix(root)?);
                    self.copy_file(&path, &relative, entry.metadata()?.len())?;
                }
            }
        }
        Ok(())
    }
}

/// An intentionally small writer for sanitized configuration. Inline tables
/// preserve types without enabling a new TOML writer dependency in the daemon.
fn write_toml(value: &serde_json::Value) -> Result<String> {
    fn atom(value: &serde_json::Value) -> Result<String> {
        Ok(match value {
            serde_json::Value::String(value) => serde_json::to_string(value)?,
            serde_json::Value::Bool(value) => value.to_string(),
            serde_json::Value::Number(value) => value.to_string(),
            serde_json::Value::Array(values) => format!(
                "[{}]",
                values
                    .iter()
                    .map(atom)
                    .collect::<Result<Vec<_>>>()?
                    .join(", ")
            ),
            serde_json::Value::Object(values) => format!(
                "{{ {} }}",
                values
                    .iter()
                    .map(|(key, value)| Ok(format!(
                        "{} = {}",
                        serde_json::to_string(key)?,
                        atom(value)?
                    )))
                    .collect::<Result<Vec<_>>>()?
                    .join(", ")
            ),
            serde_json::Value::Null => anyhow::bail!("unsupported null in agent configuration"),
        })
    }
    let object = value
        .as_object()
        .context("agent configuration must be a table")?;
    object
        .iter()
        .map(|(key, value)| {
            Ok(format!(
                "{} = {}\n",
                serde_json::to_string(key)?,
                atom(value)?
            ))
        })
        .collect()
}

/// Apply only the exported home-relative allowlist. Existing login material
/// and symlinked directories are never replaced by a remote overlay.
pub(super) fn import(source: &Path, home: &Path, workspace: &Path) -> Result<()> {
    let mut pending = vec![(source.to_path_buf(), PathBuf::new())];
    let mut visited = 0;
    while let Some((directory, relative)) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            visited += 1;
            ensure!(visited <= 32_768, "configuration overlay exceeds limit");
            let entry = entry?;
            let relative = relative.join(entry.file_name());
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
            let mut cursor = home.to_path_buf();
            for part in relative.components() {
                cursor.push(part);
                ensure!(
                    !fs::symlink_metadata(&cursor).is_ok_and(|m| m.file_type().is_symlink()),
                    "configuration destination contains a symlink"
                );
            }
            let kind = entry.file_type()?;
            ensure!(
                !kind.is_symlink(),
                "configuration overlay contains a symlink"
            );
            if kind.is_dir() {
                fs::create_dir_all(&cursor)?;
                pending.push((entry.path(), relative));
            } else if kind.is_file() {
                // Imported configs pass the same sanitizer again; a service
                // cannot smuggle tokens under a normally safe settings path.
                let metadata = entry.metadata()?;
                ensure!(
                    metadata.len() <= policy::MAX_CONFIG_FILE,
                    "configuration file exceeds limit"
                );
                let mut bytes = read_regular(&entry.path())?;
                let extension = relative
                    .extension()
                    .and_then(|v| v.to_str())
                    .unwrap_or_default();
                if extension == "json" || extension == "toml" {
                    let mut value = parse_config(&bytes, extension)?;
                    policy::sanitize(&mut value, &mut Vec::new(), Some(workspace));
                    if cursor.exists() {
                        let mut existing = parse_config(&read_regular(&cursor)?, extension)?;
                        merge_config(&mut existing, value);
                        value = existing;
                    }
                    bytes = if extension == "json" {
                        serde_json::to_vec_pretty(&value)?
                    } else {
                        write_toml(&value)?.into_bytes()
                    };
                } else {
                    if relative == Path::new(".gitconfig") {
                        bytes = git_identity(&bytes);
                        if cursor.exists() {
                            bytes = merge_git_preferences(&read_regular(&cursor)?, &bytes)?;
                        }
                    } else {
                        ensure!(
                            !policy::contains_credential(&bytes),
                            "configuration overlay contains a credential"
                        );
                        // Native history and independently edited instructions are
                        // append-only/local authority; active sessions use bundles.
                        if cursor.exists() {
                            continue;
                        }
                    }
                }
                ensure!(
                    bytes.len() as u64 <= policy::MAX_CONFIG_FILE,
                    "merged configuration exceeds limit"
                );
                let temporary =
                    cursor.with_extension(format!("chimaera-{}", chimaera_core::generate_token()));
                let mut options = fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut output = options.open(&temporary)?;
                output.write_all(&bytes)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    output.set_permissions(fs::Permissions::from_mode(
                        0o600 | (metadata.permissions().mode() & 0o111),
                    ))?;
                }
                output.sync_all()?;
                fs::rename(&temporary, &cursor)?;
            }
        }
    }
    Ok(())
}

fn read_regular(path: &Path) -> Result<Vec<u8>> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
        );
    }
    let file = options.open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "configuration is not a regular file"
    );
    let mut bytes = Vec::new();
    file.take(policy::MAX_CONFIG_FILE + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= policy::MAX_CONFIG_FILE,
        "configuration file exceeds limit"
    );
    Ok(bytes)
}
fn parse_config(bytes: &[u8], extension: &str) -> Result<serde_json::Value> {
    if extension == "json" {
        Ok(serde_json::from_slice(bytes)?)
    } else {
        Ok(serde_json::to_value(toml::from_str::<toml::Value>(
            std::str::from_utf8(bytes)?,
        )?)?)
    }
}
fn merge_config(existing: &mut serde_json::Value, incoming: serde_json::Value) {
    match (existing, incoming) {
        (serde_json::Value::Object(existing), serde_json::Value::Object(incoming)) => {
            for (key, value) in incoming {
                if let Some(previous) = existing.get_mut(&key) {
                    if matches!(key.as_str(), "mcp_servers" | "mcpServers") {
                        merge_mcp_connections(previous, value);
                    } else {
                        merge_config(previous, value);
                    }
                } else {
                    existing.insert(key, value);
                }
            }
        }
        (existing, incoming) => *existing = incoming,
    }
}
fn merge_mcp_connections(existing: &mut serde_json::Value, incoming: serde_json::Value) {
    let (Some(existing), serde_json::Value::Object(incoming)) =
        (existing.as_object_mut(), incoming)
    else {
        return;
    };
    for (name, connection) in incoming {
        if let Some(previous) = existing.get_mut(&name) {
            // A destination's existing login belongs to its exact connection.
            // Never retarget retained credentials to a new URL or process just
            // because the source happens to use the same integration name.
            let same_binding = [
                "url",
                "type",
                "command",
                "args",
                "cwd",
                "bearer_token_env_var",
                "env_http_headers",
            ]
            .iter()
            .all(|key| previous.get(key) == connection.get(key));
            // Omitted credential values retain destination authority. Any
            // supplied environment/header change may alter that authority's
            // destination or executable lookup, so retain the whole connection.
            let same_environment = ["env", "headers", "http_headers"].iter().all(|key| {
                connection.get(key).is_none_or(|incoming| {
                    incoming.as_object().is_some_and(|incoming| {
                        incoming.iter().all(|(name, value)| {
                            previous.get(key).and_then(|existing| existing.get(name)) == Some(value)
                        })
                    })
                })
            });
            if same_binding && same_environment {
                merge_config(previous, connection);
            }
        } else {
            existing.insert(name, connection);
        }
    }
}

/// The block a Git config's portable preferences live in on the machine
/// that received them. `git_identity` never reads from inside it.
const GIT_PREFERENCES_START: &str = "# chimaera portable Git preferences begin";
const GIT_PREFERENCES_END: &str = "# chimaera portable Git preferences end";

fn merge_git_preferences(existing: &[u8], incoming: &[u8]) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(existing)?;
    let mut output = String::new();
    let mut managed = false;
    for line in text.lines() {
        if line == GIT_PREFERENCES_START {
            ensure!(!managed, "nested portable Git preferences");
            managed = true;
            continue;
        }
        if line == GIT_PREFERENCES_END {
            ensure!(managed, "unmatched portable Git preferences");
            managed = false;
            continue;
        }
        if !managed {
            output.push_str(line);
            output.push('\n');
        }
    }
    ensure!(!managed, "unterminated portable Git preferences");
    output.push_str(GIT_PREFERENCES_START);
    output.push('\n');
    output.push_str(std::str::from_utf8(incoming)?);
    output.push_str(GIT_PREFERENCES_END);
    output.push('\n');
    ensure!(
        output.len() as u64 <= policy::MAX_CONFIG_FILE,
        "Git preferences exceed limit"
    );
    Ok(output.into_bytes())
}

/// The author identity and simple aliases a Git config carries, once each. A
/// file that already holds a managed block (a cloud copy, or a computer that
/// received one) contributes nothing from inside it, so the identity never
/// grows by one copy per round trip between a computer and the cloud; a key
/// named twice keeps its last value, as Git reads it.
fn git_identity(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    let mut section = "";
    let mut name: Option<String> = None;
    let mut email: Option<String> = None;
    let mut aliases: Vec<(String, String)> = Vec::new();
    let mut managed = false;
    for line in text.lines() {
        let line = line.trim();
        if line == GIT_PREFERENCES_START {
            managed = true;
            continue;
        }
        if line == GIT_PREFERENCES_END {
            managed = false;
            continue;
        }
        if managed {
            continue;
        }
        if line.starts_with('[') {
            section = if line.eq_ignore_ascii_case("[user]") {
                "user"
            } else if line.eq_ignore_ascii_case("[alias]") {
                "alias"
            } else {
                ""
            };
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            if value.len() > 2048
                || value.chars().any(char::is_control)
                || value.contains(['\\', ';', '#'])
                || policy::contains_credential(value.as_bytes())
            {
                continue;
            }
            if section == "user" && matches!(key.as_str(), "name" | "email") {
                if key == "name" {
                    name = Some(value.to_owned());
                } else {
                    email = Some(value.to_owned());
                }
            } else if section == "alias"
                && key.len() <= 64
                && key
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
            {
                // Only simple command/flag aliases travel. Free-form arguments
                // can contain credentials even without a recognizable prefix.
                let literal = value.trim_matches('"');
                let mut words = literal.split_whitespace();
                let command = words.next().unwrap_or_default();
                let simple_flags = words.all(|flag| {
                    matches!(
                        flag,
                        "-s" | "-sb"
                            | "--short"
                            | "--branch"
                            | "--oneline"
                            | "--graph"
                            | "--decorate"
                            | "--all"
                            | "-a"
                            | "-v"
                            | "-vv"
                            | "--stat"
                            | "--cached"
                            | "--staged"
                            | "--color=auto"
                            | "--rebase"
                            | "--ff-only"
                            | "--amend"
                            | "--no-edit"
                            | "--verbose"
                            | "--prune"
                            | "--tags"
                    )
                });
                if simple_flags
                    && matches!(
                        command,
                        "status"
                            | "log"
                            | "show"
                            | "diff"
                            | "branch"
                            | "checkout"
                            | "switch"
                            | "fetch"
                            | "pull"
                            | "push"
                            | "commit"
                            | "merge"
                            | "rebase"
                            | "worktree"
                            | "stash"
                            | "reset"
                            | "restore"
                            | "add"
                            | "rm"
                            | "cherry-pick"
                    )
                {
                    match aliases.iter_mut().find(|(known, _)| *known == key) {
                        Some(alias) => alias.1 = value.to_owned(),
                        None => aliases.push((key, value.to_owned())),
                    }
                }
            }
        }
    }
    let mut identity = String::new();
    if let Some(name) = name {
        identity.push_str(&format!("\tname = {name}\n"));
    }
    if let Some(email) = email {
        identity.push_str(&format!("\temail = {email}\n"));
    }
    let aliases: String = aliases
        .iter()
        .map(|(key, value)| format!("\t{key} = {value}\n"))
        .collect();
    format!(
        "{}{}",
        if identity.is_empty() {
            String::new()
        } else {
            format!("[user]\n{identity}")
        },
        if aliases.is_empty() {
            String::new()
        } else {
            format!("[alias]\n{aliases}")
        }
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlay_updates_preferences_without_replacing_local_credentials() {
        let mut incoming = serde_json::json!({"model":"new","env":{"PATH":"/new","SECRET_TOKEN":"foreign"},"password":"foreign"});
        policy::sanitize(&mut incoming, &mut Vec::new(), None);
        let mut existing = serde_json::json!({"model":"old","env":{"SECRET_TOKEN":"cloud-only"},"password":"local-only"});
        merge_config(&mut existing, incoming);
        assert_eq!(existing["model"], "new");
        assert_eq!(existing["env"]["SECRET_TOKEN"], "cloud-only");
        assert_eq!(existing["env"]["PATH"], "/new");
        assert_eq!(existing["password"], "local-only");
        let identity = String::from_utf8(git_identity(b"[user]\nname = Dev\nemail = dev@example.invalid\nsigningkey = private\n[credential]\nhelper = secret\n[alias]\nst = status --short\nunsafe = !curl private\nup = fetch https://alice:plain-password@example.test/private.git\nsecret = fetch --token plain-password\n[include]\npath = /private\n")).unwrap();
        assert!(identity.contains("Dev"));
        assert!(identity.contains("st = status --short"));
        assert!(!identity.contains("unsafe") && !identity.contains("plain-password"));
        assert!(!identity.contains("private") && !identity.contains("secret"));
        let host = b"[credential]\nhelper = host-login-helper\n";
        let merged = merge_git_preferences(host, identity.as_bytes()).unwrap();
        assert_eq!(
            merge_git_preferences(&merged, identity.as_bytes()).unwrap(),
            merged
        );
        assert!(String::from_utf8(merged)
            .unwrap()
            .contains("helper = host-login-helper"));
    }
    #[test]
    fn git_identity_never_grows_by_its_own_managed_block() {
        // A computer's config after it received the cloud's preferences: its
        // own [user] plus the managed block, whose copies must not count.
        let received = b"[user]\n\tname = Dev\n\temail = dev@example.invalid\n# chimaera portable Git preferences begin\n[user]\n\tname = Dev\n\temail = dev@example.invalid\n\tname = Dev\n\temail = dev@example.invalid\n# chimaera portable Git preferences end\n[user]\n\temail = later@example.invalid\n";
        let identity = String::from_utf8(git_identity(received)).unwrap();
        assert_eq!(identity.matches("name = ").count(), 1);
        assert_eq!(identity.matches("email = ").count(), 1);
        // A key named twice keeps its last value, as Git reads it.
        assert!(identity.contains("email = later@example.invalid"));
        // Round trips settle: merging what a machine sends back changes nothing.
        let merged = merge_git_preferences(received, identity.as_bytes()).unwrap();
        let again = git_identity(&merged);
        assert_eq!(again, identity.as_bytes());
        assert_eq!(
            merge_git_preferences(&merged, &again).unwrap(),
            merged,
            "a second round trip is a fixed point"
        );
    }
    #[test]
    fn mcp_overlay_never_retargets_destination_credentials() {
        for (key, original, replacement) in [
            (
                "url",
                serde_json::json!("https://old.example/mcp"),
                serde_json::json!("https://new.example/mcp"),
            ),
            (
                "command",
                serde_json::json!("npx"),
                serde_json::json!("node"),
            ),
            (
                "args",
                serde_json::json!(["server-a"]),
                serde_json::json!(["server-b"]),
            ),
            (
                "bearer_token_env_var",
                serde_json::json!("SERVICE_TOKEN"),
                serde_json::json!("OTHER_TOKEN"),
            ),
        ] {
            let mut existing = serde_json::json!({"mcp_servers":{"service":{"env":{"API_TOKEN":"cloud-only"},"timeout":10}}});
            existing["mcp_servers"]["service"][key] = original;
            let before = existing.clone();
            let mut incoming = serde_json::json!({"mcp_servers":{"service":{"timeout":20}}});
            incoming["mcp_servers"]["service"][key] = replacement;
            merge_config(&mut existing, incoming);
            assert_eq!(existing, before, "binding field {key}");
        }
        for (container, key) in [
            ("env", "API_BASE_URL"),
            ("env", "PATH"),
            ("headers", "X-Service-Endpoint"),
            ("http_headers", "X-Tenant"),
        ] {
            let mut existing = serde_json::json!({"mcp_servers":{"service":{"url":"https://same.example/mcp", "env":{"API_TOKEN":"cloud-only"},"timeout":10}}});
            existing["mcp_servers"]["service"][container][key] = serde_json::json!("original");
            let before = existing.clone();
            let mut incoming = serde_json::json!({"mcp_servers":{"service":{"url":"https://same.example/mcp","timeout":20}}});
            incoming["mcp_servers"]["service"][container] = serde_json::json!({key:"changed"});
            merge_config(&mut existing, incoming);
            assert_eq!(existing, before, "binding configuration {container}/{key}");
        }
        let mut existing = serde_json::json!({"mcpServers":{"service":{"url":"https://same.example/mcp","authorization":"cloud-only","timeout":10}}});
        merge_config(
            &mut existing,
            serde_json::json!({"mcpServers":{"service":{"url":"https://same.example/mcp","timeout":20}}}),
        );
        assert_eq!(
            existing["mcpServers"]["service"]["authorization"],
            "cloud-only"
        );
        assert_eq!(existing["mcpServers"]["service"]["timeout"], 20);
    }

    #[test]
    fn only_active_environment_omissions_are_reported() {
        let root = std::env::temp_dir();
        let mut export = Export {
            destination: &root,
            workspace: &root,
            remaining: 1_000_000,
            visited: 0,
            started: Instant::now(),
            report: Report::default(),
        };
        let mut metadata = serde_json::json!({"name":"token documentation", "env":{"EXAMPLE_TOKEN":"not-an-active-setting"}});
        export.sanitize_config(&mut metadata, Path::new(".claude/plugins/demo/plugin.json"));
        assert!(export.report.missing_environment.is_empty());
        assert!(metadata.get("name").is_none());
        let mut active = serde_json::json!({"env":{"API_TOKEN":"not-copied", "CODEX_HOME":"/Users/example/.codex"},"projects":{"other":{"env":{"UNRELATED_TOKEN":"not-copied"}}},"mcp_servers":{"service":{"url":"https://example.invalid/mcp", "env":{"SERVICE_TOKEN":"not-copied"}}}});
        export.sanitize_config(&mut active, Path::new(".codex/config.toml"));
        assert_eq!(
            export.report.missing_environment,
            ["API_TOKEN", "SERVICE_TOKEN"]
        );
        assert!(!serde_json::to_string(&active)
            .unwrap()
            .contains("not-copied"));
    }

    #[test]
    fn sanitized_toml_retains_nested_servers_and_types() {
        let mut value = serde_json::json!({"model":"codex", "mcp_servers":{"demo":{"command":"node", "args":["server.js"], "env":{"PATH":"/usr/bin", "SECRET_TOKEN":"bad"}}}, "flag":true});
        let mut missing = Vec::new();
        policy::sanitize(&mut value, &mut missing, None);
        let text = write_toml(&value).unwrap();
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(
            parsed["mcp_servers"]["demo"]["command"].as_str(),
            Some("node")
        );
        assert_eq!(parsed["flag"].as_bool(), Some(true));
        assert!(!text.contains("bad"));
    }
    #[test]
    fn export_does_not_follow_symlinks_or_copy_login_files() {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-config-export-{}",
            chimaera_core::generate_token()
        ));
        let claude = dir.join("home/.claude");
        let codex = dir.join("home/.codex");
        fs::create_dir_all(claude.join("skills/demo")).unwrap();
        fs::create_dir_all(&codex).unwrap();
        fs::write(
            claude.join("settings.json"),
            br#"{"env":{"API_TOKEN":"hidden","PATH":"/bin"},"model":"default"}"#,
        )
        .unwrap();
        fs::write(claude.join(".credentials.json"), "MUST_NOT_COPY").unwrap();
        fs::write(codex.join("auth.json"), "MUST_NOT_COPY").unwrap();
        fs::write(claude.join("skills/demo/SKILL.md"), "# Useful instructions").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            claude.join(".credentials.json"),
            claude.join("skills/demo/secret.txt"),
        )
        .unwrap();
        let destination = dir.join("export");
        let report = export(
            Sources {
                home: dir.join("home"),
                claude,
                codex,
                workspace: dir.join("project"),
            },
            &destination,
            1024 * 1024,
        )
        .unwrap();
        assert!(!destination.join(".claude/.credentials.json").exists());
        assert!(!destination.join(".codex/auth.json").exists());
        assert!(!destination.join(".claude/skills/demo/secret.txt").exists());
        assert!(report.missing_environment.contains(&"API_TOKEN".into()));
        assert!(
            !fs::read_to_string(destination.join(".claude/settings.json"))
                .unwrap()
                .contains("hidden")
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
