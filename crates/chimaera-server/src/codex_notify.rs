//! Terminal Codex identity for Pro-configured projects: chain the user's own
//! notify command, then verify the native rollout before promising a resume.
//! No transcript contents cross the wire. Other projects' Codex TUIs keep
//! their argv unchanged.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::Context;

const CONFIG_CAP: u64 = 1024 * 1024;
const HEADER_CAP: u64 = 64 * 1024;
const SCAN_CAP: usize = 32_768;
pub(crate) static ROLLOUT_WORK: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// A malformed/unreadable notify is an error: callers leave the user's own
/// hook in place rather than override a command we cannot faithfully chain.
/// `None` when the file or its `notify` key is absent.
fn configured_notify(config: &Path) -> anyhow::Result<Option<Vec<String>>> {
    let contents = match crate::doc_check::read_regular(config, CONFIG_CAP) {
        Ok(Some(bytes)) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        _ => anyhow::bail!("cannot read bounded Codex config"),
    };
    let text = std::str::from_utf8(&contents)
        .map_err(|_| anyhow::anyhow!("invalid Codex config encoding"))?;
    // TOML errors retain the full source, which can contain credentials.
    let config: toml::Value =
        toml::from_str(text).map_err(|_| anyhow::anyhow!("invalid Codex config"))?;
    let Some(notify) = config.get("notify") else {
        return Ok(None);
    };
    notify
        .as_array()
        .context("Codex notify is not an argv array")?
        .iter()
        .map(|arg| {
            arg.as_str()
                .filter(|s| !s.contains('\0'))
                .map(str::to_string)
                .context("Codex notify contains an invalid argument")
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .map(Some)
}

/// The notify Codex itself would run for this project: a project's own
/// `.codex/config.toml` layers over the one in its Codex home.
fn user_notify(project: &Path, home: &Path) -> anyhow::Result<Vec<String>> {
    if let Some(notify) = configured_notify(&project.join(".codex").join("config.toml"))? {
        return Ok(notify);
    }
    Ok(configured_notify(&home.join("config.toml"))?.unwrap_or_default())
}

/// Codex's own config home as a terminal session sees it. Terminal agents
/// start through the user's login shell, which may export `CODEX_HOME`
/// (common on HPC) where the daemon's own environment does not; that is
/// probed once per daemon life, bounded, reading only that one variable.
pub(crate) async fn codex_home(state: &crate::AppState) -> PathBuf {
    static LOGIN: tokio::sync::OnceCell<Option<PathBuf>> = tokio::sync::OnceCell::const_new();
    // Unit tests never run the developer's login shell.
    let login = if cfg!(test) {
        None
    } else {
        LOGIN.get_or_init(login_codex_home).await.clone()
    };
    login.unwrap_or_else(|| {
        state
            .codex_config_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    })
}
async fn login_codex_home() -> Option<PathBuf> {
    let mut command = tokio::process::Command::new(crate::launcher::login_shell());
    command
        .arg("-ilc")
        .arg("printf 'CODEX_HOME=%s\\n' \"${CODEX_HOME-}\"");
    probe_codex_home(&mut command, std::time::Duration::from_secs(6)).await
}
async fn probe_codex_home(
    command: &mut tokio::process::Command,
    timeout: std::time::Duration,
) -> Option<PathBuf> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let (success, stdout) = tokio::time::timeout(
        timeout,
        crate::cloud::providers::bounded_process_output(command),
    )
    .await
    .ok()?
    .ok()?;
    if !success {
        return None;
    }
    env_codex_home(std::str::from_utf8(&stdout).ok()?)
}
/// The last exact assignment wins; login rc banners are ignored. Only an
/// absolute path counts. The probe prints this variable alone, never `env`.
fn env_codex_home(env: &str) -> Option<PathBuf> {
    env.lines()
        .rev()
        .find_map(|line| line.strip_prefix("CODEX_HOME="))
        .filter(|value| value.starts_with('/'))
        .map(PathBuf::from)
}
/// Under the daemon's data dir (not the runtime dir that HPC hosts scrub
/// nightly, which would silently stop the chained user notify).
fn shim_dir(state: &crate::AppState) -> PathBuf {
    state.shims_dir.join("codex-notify")
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    Ok(())
}

/// The key goes in a private curl header file, never process arguments. JSON
/// goes through stdin; the user's argv receives the identical final argument.
pub(crate) fn write_shim(
    dir: &Path,
    notify: &[String],
    session_id: &str,
    key: &str,
    port: u16,
) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let path = dir.join(format!("{session_id}-codex-notify.sh"));
    let header = dir.join(format!("{session_id}-codex-notify.hdr"));
    write_private(&header, format!("Authorization: Bearer {key}\n").as_bytes())?;
    let chain = if notify.is_empty() {
        "exit 0".to_string()
    } else {
        format!(
            "exec {} \"$1\"",
            notify
                .iter()
                .map(|s| quote(s))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    let script = format!(
        "#!/bin/sh\n# Delivery is best effort; a stopped daemon must not swallow the user's notify.\nprintf '%s' \"$1\" | curl --silent --fail --connect-timeout 1 --max-time 2 --noproxy '*' -H @{} -H 'Content-Type: application/json' --data-binary @- 'http://127.0.0.1:{port}/api/v1/agent-events/{session_id}?event=codex-notify' >/dev/null 2>&1\n{chain}\n",
        quote(&header.to_string_lossy()),
    );
    write_private(&path, script.as_bytes())?;
    Ok(path)
}

/// The notify override for a Codex TUI in a Pro-configured project (its
/// pause state and restart resume need the turn-complete signal); none
/// elsewhere, so a free user's Codex argv and notify stay exactly theirs.
pub(crate) async fn args(state: &crate::AppState, id: &str, key: &str) -> Vec<String> {
    let Some(workspace) = crate::lock(&state.session_workspaces).get(id).cloned() else {
        return Vec::new();
    };
    args_in(state, &workspace, id, key).await
}
/// `args` for a session not registered to its workspace yet (a fresh spawn).
pub(crate) async fn args_in(
    state: &crate::AppState,
    workspace: &str,
    id: &str,
    key: &str,
) -> Vec<String> {
    if crate::pro::workspace_profile(state, workspace).is_none() {
        return Vec::new();
    }
    let Some(project) = crate::lock(&state.workspaces)
        .get(workspace)
        .map(|workspace| workspace.root)
    else {
        return Vec::new();
    };
    let home = codex_home(state).await;
    let dir = shim_dir(state);
    let id = id.to_string();
    let key = key.to_string();
    let port = state.port;
    let written = tokio::task::spawn_blocking(move || {
        let notify = user_notify(&project, &home)?;
        write_shim(&dir, &notify, &id, &key, port)
    })
    .await;
    match written {
        Ok(Ok(path)) => crate::launcher::codex_notify_args(&path),
        _ => {
            // Parser and filesystem errors can retain config text or private paths.
            tracing::warn!("Codex identity hook unavailable; preserving user notify");
            Vec::new()
        }
    }
}

pub(crate) fn remove_shim(state: &crate::AppState, id: &str) {
    let dir = shim_dir(state);
    for ext in ["sh", "hdr"] {
        let _ = std::fs::remove_file(dir.join(format!("{id}-codex-notify.{ext}")));
    }
}

pub(crate) fn valid_thread_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

/// A filename alone is not identity. Read just the capped session_meta header,
/// checking both id and cwd. open_regular rejects pipes/devices without hanging.
pub(crate) fn verify_rollout(path: &Path, id: &str, cwd: &Path) -> bool {
    if !valid_thread_id(id) {
        return false;
    }
    let Ok((file, _)) = crate::fs::open_regular(path) else {
        return false;
    };
    let mut line = String::new();
    if BufReader::new(file.take(HEADER_CAP + 1))
        .read_line(&mut line)
        .is_err()
        || line.len() as u64 > HEADER_CAP
    {
        return false;
    }
    let Ok(header) = serde_json::from_str::<serde_json::Value>(&line) else {
        return false;
    };
    header["type"] == "session_meta"
        && header["payload"]["id"].as_str() == Some(id)
        && header["payload"]["cwd"]
            .as_str()
            .is_some_and(|value| Path::new(value) == cwd)
}

/// Codex's dated rollout store has exactly three directory levels. Bound both
/// metadata work and wall time; never recurse into arbitrary user directories.
pub(crate) fn find_rollout(home: &Path, id: &str, cwd: &Path) -> Option<PathBuf> {
    if !valid_thread_id(id) {
        return None;
    }
    let started = std::time::Instant::now();
    let suffix = format!("-{id}.jsonl");
    let mut pending = vec![(home.join("sessions"), 0)];
    let mut visited = 0;
    while let Some((dir, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > SCAN_CAP || started.elapsed() > std::time::Duration::from_secs(2) {
                return None;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if depth < 3 && kind.is_dir() {
                pending.push((entry.path(), depth + 1));
            } else if kind.is_file()
                && entry.file_name().to_string_lossy().ends_with(&suffix)
                && verify_rollout(&entry.path(), id, cwd)
            {
                return Some(entry.path());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "01a0e110-de29-75e2-9262-7a8c893b2a3c";

    #[test]
    fn rollout_requires_matching_bounded_header() {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-codex-rollout-{}",
            chimaera_core::generate_token()
        ));
        let dated = dir.join("sessions/2026/09/26");
        std::fs::create_dir_all(&dated).unwrap();
        let path = dated.join(format!("rollout-date-{ID}.jsonl"));
        std::fs::write(
            &path,
            serde_json::json!({"type":"session_meta", "payload":{"id":ID,"cwd":"/work"}})
                .to_string()
                + "\n",
        )
        .unwrap();
        assert_eq!(
            find_rollout(&dir, ID, Path::new("/work")),
            Some(path.clone())
        );
        assert!(!verify_rollout(&path, ID, Path::new("/other")));
        assert!(find_rollout(&dir, "../escape", Path::new("/work")).is_none());
        std::fs::write(&path, "x".repeat(HEADER_CAP as usize + 1)).unwrap();
        assert!(!verify_rollout(&path, ID, Path::new("/work")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn shim_preserves_notify_argv_and_payload_even_when_daemon_is_down() {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-codex-notify-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let user = dir.join("user's hook.sh");
        let received = dir.join("received");
        std::fs::write(
            &user,
            format!(
                "printf '%s\\n' \"$1\" \"$2\" > {}\n",
                quote(&received.to_string_lossy())
            ),
        )
        .unwrap();
        let config = dir.join("config.toml");
        let notify = serde_json::json!(["/bin/sh", user, "literal $HOME `false` ' \\"]);
        std::fs::write(&config, format!("notify = {notify}\n")).unwrap();
        let chained = user_notify(&dir.join("project"), &dir).unwrap();
        let script = write_shim(&dir, &chained, "s-test", "private-key", 1).unwrap();
        let payload = "{\"thread-id\":\"test\",\"message\":\"$(false) '$HOME'\"}";
        let status = std::process::Command::new("/bin/sh")
            .arg(&script)
            .arg(payload)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string(received).unwrap(),
            format!("literal $HOME `false` ' \\\n{payload}\n")
        );
        let script_text = std::fs::read_to_string(&script).unwrap();
        assert!(!script_text.contains("private-key"));
        assert_eq!(
            std::fs::metadata(dir.join("s-test-codex-notify.hdr"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The chained notify is the one Codex itself would run: a project's own
    /// `.codex/config.toml` wins over its Codex home, and a login shell's
    /// `CODEX_HOME` names that home.
    #[test]
    fn the_chained_notify_is_the_one_codex_would_run() {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-codex-effective-{}",
            chimaera_core::generate_token()
        ));
        let (home, project) = (dir.join("home"), dir.join("project"));
        std::fs::create_dir_all(project.join(".codex")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("config.toml"), "notify = [\"home-hook\"]\n").unwrap();
        assert_eq!(user_notify(&project, &home).unwrap(), ["home-hook"]);
        std::fs::write(
            project.join(".codex/config.toml"),
            "notify = [\"project-hook\", \"--flag\"]\n",
        )
        .unwrap();
        assert_eq!(
            user_notify(&project, &home).unwrap(),
            ["project-hook", "--flag"]
        );
        // A project that turns notify off keeps it off.
        std::fs::write(project.join(".codex/config.toml"), "notify = []\n").unwrap();
        assert!(user_notify(&project, &home).unwrap().is_empty());
        std::fs::write(project.join(".codex/config.toml"), "model = \"x\"\n").unwrap();
        assert_eq!(user_notify(&project, &home).unwrap(), ["home-hook"]);
        assert_eq!(
            env_codex_home("Welcome to the cluster\nPATH=/bin\nCODEX_HOME=/scratch/u/.codex\n"),
            Some(PathBuf::from("/scratch/u/.codex"))
        );
        assert_eq!(env_codex_home("CODEX_HOME=relative\n"), None);
        assert_eq!(env_codex_home("PATH=/bin\n"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn malformed_notify_errors_never_retain_config_contents() {
        let dir = std::env::temp_dir().join(format!(
            "codex-config-error-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "api_key = \"synthetic-private-token\"\nnotify = [broken\n",
        )
        .unwrap();
        let error = configured_notify(&path).unwrap_err();
        assert_eq!(error.to_string(), "invalid Codex config");
        for cause in error.chain() {
            assert!(!format!("{cause:?} {cause}").contains("synthetic-private-token"));
        }
        assert!(!format!("{error:?} {error:#}").contains("synthetic-private-token"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn login_home_probe_is_bounded_and_requires_success() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "printf 'CODEX_HOME=/tmp/codex-home\\n'"]);
        assert_eq!(
            probe_codex_home(&mut command, std::time::Duration::from_secs(2)).await,
            Some(PathBuf::from("/tmp/codex-home"))
        );
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "printf 'CODEX_HOME=/tmp/codex-home\\n'; exit 1"]);
        assert!(
            probe_codex_home(&mut command, std::time::Duration::from_secs(2))
                .await
                .is_none()
        );
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "head -c 70000 /dev/zero"]);
        assert!(
            probe_codex_home(&mut command, std::time::Duration::from_secs(2))
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn login_probe_cancellation_timeout_and_exit_stop_descendants() {
        for mode in ["cancel", "timeout", "exit"] {
            let dir = std::env::temp_dir().join(format!(
                "codex-probe-group-{}",
                chimaera_core::generate_token()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let pidfile = dir.join("pid");
            let tail = if mode == "exit" { "exit 0" } else { "wait" };
            let script = format!(
                "sleep 30 & printf '%s' \"$!\" > {}; {tail}",
                quote(&pidfile.to_string_lossy())
            );
            let mut command = tokio::process::Command::new("/bin/sh");
            command.args(["-c", &script]);
            let timeout = if mode == "timeout" {
                std::time::Duration::from_millis(500)
            } else {
                std::time::Duration::from_secs(3)
            };
            let task = tokio::spawn(async move { probe_codex_home(&mut command, timeout).await });
            let pid = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if let Ok(contents) = std::fs::read_to_string(&pidfile) {
                        if let Ok(pid) = contents.parse::<u32>() {
                            break pid;
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            if mode == "cancel" {
                task.abort();
            }
            let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
                .await
                .unwrap();
            if mode != "cancel" {
                assert!(result.unwrap().is_none());
            }
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    // A zombie is stopped; some CI PID1s reap slowly.
                    let running = std::process::Command::new("/bin/ps")
                        .args(["-o", "stat=", "-p", &pid.to_string()])
                        .output()
                        .unwrap();
                    let stat = String::from_utf8_lossy(&running.stdout);
                    if stat.trim().is_empty() || stat.trim().starts_with('Z') {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
