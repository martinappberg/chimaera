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
            policy::sanitize(&mut value, &mut self.report.missing_environment);
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
            policy::sanitize(&mut json, &mut self.report.missing_environment);
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
pub(super) fn import(source: &Path, home: &Path) -> Result<()> {
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
                    policy::sanitize(&mut value, &mut Vec::new());
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
                    }
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
                    merge_config(previous, value);
                } else {
                    existing.insert(key, value);
                }
            }
        }
        (existing, incoming) => *existing = incoming,
    }
}
fn git_identity(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    let mut user = false;
    let mut identity = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            user = line.eq_ignore_ascii_case("[user]");
            continue;
        }
        if !user {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if ["name", "email"]
                .iter()
                .any(|name| key.trim().eq_ignore_ascii_case(name))
            {
                let value = value.trim();
                if value.len() <= 512
                    && !value.chars().any(char::is_control)
                    && !value.contains(['\\', ';', '#'])
                    && !policy::contains_credential(value.as_bytes())
                {
                    identity.push_str(&format!(
                        "\t{} = {}\n",
                        key.trim().to_ascii_lowercase(),
                        value
                    ));
                }
            }
        }
    }
    if identity.is_empty() {
        Vec::new()
    } else {
        format!("[user]\n{identity}").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlay_updates_preferences_without_replacing_local_credentials() {
        let mut incoming = serde_json::json!({"model":"new","env":{"PATH":"/new","SECRET_TOKEN":"foreign"},"password":"foreign"});
        policy::sanitize(&mut incoming, &mut Vec::new());
        let mut existing = serde_json::json!({"model":"old","env":{"SECRET_TOKEN":"cloud-only"},"password":"local-only"});
        merge_config(&mut existing, incoming);
        assert_eq!(existing["model"], "new");
        assert_eq!(existing["env"]["SECRET_TOKEN"], "cloud-only");
        assert_eq!(existing["env"]["PATH"], "/new");
        assert_eq!(existing["password"], "local-only");
        let identity = String::from_utf8(git_identity(b"[user]\nname = Dev\nemail = dev@example.invalid\nsigningkey = private\n[credential]\nhelper = secret\n[include]\npath = /private\n")).unwrap();
        assert!(identity.contains("Dev"));
        assert!(!identity.contains("private") && !identity.contains("secret"));
    }
    #[test]
    fn sanitized_toml_retains_nested_servers_and_types() {
        let mut value = serde_json::json!({"model":"codex", "mcp_servers":{"demo":{"command":"node", "args":["server.js"], "env":{"PATH":"/usr/bin", "SECRET_TOKEN":"bad"}}}, "flag":true});
        let mut missing = Vec::new();
        policy::sanitize(&mut value, &mut missing);
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
