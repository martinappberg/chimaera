//! Terminal Codex identity for Pro-configured projects: chain the user's own
//! notify command, then verify the native rollout before promising a resume.
//! No transcript contents cross the wire. Other projects' Codex TUIs keep
//! their argv unchanged.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::Context;

const CONFIG_CAP: u64 = 1024 * 1024;

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

/// `args` for a session not registered to its workspace yet (a fresh spawn).
pub(crate) async fn args_in(
    state: &crate::AppState,
    workspace: &str,
    id: &str,
    key: &str,
) -> Vec<String> {
    if !crate::pro::workspace_in_scope(state, workspace) {
        return Vec::new();
    }
    let Some(project) = crate::lock(&state.workspaces)
        .get(workspace)
        .map(|workspace| workspace.root)
    else {
        return Vec::new();
    };
    let home = crate::codex_rollout::codex_home(state).await;
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
#[cfg(test)]
mod tests {
    use super::*;

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
}
