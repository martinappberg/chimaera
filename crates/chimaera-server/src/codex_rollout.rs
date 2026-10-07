//! Codex's own transcript store, as every Codex surface reads it: the
//! config home a terminal session sees, and the dated rollout file that
//! proves a thread id belongs to a project before a resume promises it. No
//! transcript contents are read beyond the one header line.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

const HEADER_CAP: u64 = 64 * 1024;
const SCAN_CAP: usize = 32_768;
/// Rollout lookups walk a dated tree; two at a time, never a pile-up.
pub(crate) static ROLLOUT_WORK: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

/// Codex's own config home as a terminal session sees it. Terminal agents
/// start through the user's login shell, which may export `CODEX_HOME`
/// (common on HPC) where the daemon's own environment does not; that is
/// probed once per daemon life, bounded, reading only that one variable.
pub async fn codex_home(state: &crate::AppState) -> PathBuf {
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
    let (success, stdout) = tokio::time::timeout(timeout, bounded_output(command))
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
/// Success and stdout of `command`, both bounded; a stopped or oversized
/// probe is no answer.
async fn bounded_output(
    command: &mut tokio::process::Command,
) -> Result<(bool, Vec<u8>), &'static str> {
    let out = crate::process::output(command).await?;
    Ok((out.success, out.stdout))
}

pub fn valid_thread_id(id: &str) -> bool {
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
pub fn find_rollout(home: &Path, id: &str, cwd: &Path) -> Option<PathBuf> {
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

    fn quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }

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
    fn the_login_shell_answer_is_the_last_absolute_assignment() {
        assert_eq!(
            env_codex_home("Welcome to the cluster\nPATH=/bin\nCODEX_HOME=/scratch/u/.codex\n"),
            Some(PathBuf::from("/scratch/u/.codex"))
        );
        assert_eq!(env_codex_home("CODEX_HOME=relative\n"), None);
        assert_eq!(env_codex_home("PATH=/bin\n"), None);
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
