//! Bounded subprocess transports keep optional TLS and Git out of the daemon.
use std::{path::Path, process::Stdio, time::Duration};

use anyhow::{bail, ensure, Context, Result};
use serde::de::DeserializeOwned;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{OnceCell, Semaphore},
};

static CHILDREN: Semaphore = Semaphore::const_new(2);
static MIRROR_GIT: OnceCell<MirrorGit> = OnceCell::const_new();
type GitVersion = (u32, u32, u32);
const FIXED_HTTP_GIT: GitVersion = (2, 45, 0);
const HTTP_GIT_HINT: &str = "mirror Git HTTP transfer failed; an older Git/curl combination may truncate large uploads. Update Git to 2.45 or newer and restart Chimaera";

struct MirrorGit {
    binary: &'static str,
    version: Option<GitVersion>,
}

fn known_fixed(version: Option<GitVersion>) -> bool {
    version.is_some_and(|version| version >= FIXED_HTTP_GIT)
}

fn choose_git(path: Option<GitVersion>, system: Option<GitVersion>, macos: bool) -> MirrorGit {
    // An old Apple Git must not replace a newer package-manager Git. Older
    // Linux builds may carry the fix or use an unaffected curl, so keep PATH.
    if macos && !known_fixed(path) && known_fixed(system) {
        MirrorGit {
            binary: "/usr/bin/git",
            version: system,
        }
    } else {
        MirrorGit {
            binary: "git",
            version: path,
        }
    }
}

fn git_version(bytes: &[u8]) -> Option<GitVersion> {
    let line = std::str::from_utf8(bytes).ok()?.lines().next()?;
    let version = line
        .strip_prefix("git version ")?
        .split_whitespace()
        .next()?;
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch: String = parts
        .next()?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    Some((major, minor, patch.parse().ok()?))
}

async fn probe_git(binary: &str) -> Option<GitVersion> {
    let mut command = clean_command(binary);
    command.arg("--version");
    // Discovery already reserved a child slot. Each credential-free probe
    // has its own short deadline and never starts a login shell.
    let output = run_reserved(command, vec![], Duration::from_secs(2), 256)
        .await
        .ok()?;
    output
        .success
        .then(|| git_version(&output.stdout))
        .flatten()
}

async fn discover_git<'a>(
    cell: &'a OnceCell<MirrorGit>,
    children: &Semaphore,
    queue_timeout: Duration,
) -> Result<&'a MirrorGit> {
    cell.get_or_try_init(|| async {
        // Capacity pressure is transient: leave the cell empty so a later
        // mirror attempt retries instead of permanently selecting unknown Git.
        let _permit = tokio::time::timeout(queue_timeout, children.acquire())
            .await
            .context("mirror Git discovery is waiting for another helper; retry shortly")??;
        let path = probe_git("git").await;
        let system = if cfg!(target_os = "macos") && !known_fixed(path) {
            probe_git("/usr/bin/git").await
        } else {
            None
        };
        Ok(choose_git(path, system, cfg!(target_os = "macos")))
    })
    .await
}

async fn mirror_git() -> Result<&'static MirrorGit> {
    discover_git(&MIRROR_GIT, &CHILDREN, Duration::from_secs(30)).await
}

fn http_transfer(args: &[&str]) -> bool {
    args.first()
        .is_some_and(|arg| matches!(*arg, "fetch" | "push" | "clone"))
        && args
            .iter()
            .any(|arg| arg.starts_with("https://") || arg.starts_with("http://"))
}
pub(super) const JSON_CAP: usize = 2 * 1024 * 1024;
pub(super) const PATH_CAP: usize = 8 * 1024 * 1024;

pub(super) struct Output {
    pub success: bool,
    pub stdout: Vec<u8>,
}

fn clean_command(binary: &str) -> Command {
    let mut command = Command::new(binary);
    command.env_clear();
    // User tracing/config injection must never turn a memory-only mirror
    // password into a trace file. Preserve only transport/certificate/runtime
    // settings needed by ordinary managed network environments.
    for key in [
        "PATH",
        "TMPDIR",
        "TMP",
        "TEMP",
        "SYSTEMROOT",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "https_proxy",
        "http_proxy",
        "all_proxy",
        "no_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "CURL_CA_BUNDLE",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command.env("LC_ALL", "C");
    command
}

pub(super) async fn child_permit() -> Result<tokio::sync::SemaphorePermit<'static>> {
    Ok(CHILDREN.acquire().await?)
}

pub(super) async fn run(
    command: Command,
    input: Vec<u8>,
    timeout: Duration,
    cap: usize,
) -> Result<Output> {
    let _permit = CHILDREN.acquire().await?;
    run_reserved(command, input, timeout, cap).await
}

/// The caller must hold one CHILDREN permit for this entire future.
async fn run_reserved(
    mut command: Command,
    input: Vec<u8>,
    timeout: Duration,
    cap: usize,
) -> Result<Output> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("could not start mirror helper")?;
    let mut stdin = child.stdin.take().context("helper input unavailable")?;
    let stdout = child.stdout.take().context("helper output unavailable")?;
    let stderr = child
        .stderr
        .take()
        .context("helper diagnostics unavailable")?;
    tokio::time::timeout(timeout, async move {
        let send = async move {
            if !input.is_empty() {
                stdin.write_all(&input).await?;
            }
            stdin.shutdown().await
        };
        let (_, stdout, _, status) = tokio::try_join!(
            send,
            read_bounded(stdout, cap),
            read_bounded(stderr, 16 * 1024),
            child.wait()
        )?;
        Ok::<_, anyhow::Error>(Output {
            success: status.success(),
            stdout,
        })
    })
    .await
    .context("mirror helper timed out")?
}

async fn read_bounded(mut input: impl AsyncRead + Unpin, cap: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    (&mut input)
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > cap {
        return Err(std::io::Error::other("helper response exceeded limit"));
    }
    Ok(bytes)
}

/// Off-loopback transport always uses TLS. URL credentials, fragments and
/// query secrets cannot enter command arguments, curl history or redirect hops.
pub(super) fn endpoint(value: &str) -> Result<String> {
    ensure!(
        value.len() <= 2048 && !value.chars().any(char::is_control),
        "invalid service URL"
    );
    let uri: axum::http::Uri = value.parse().context("invalid service URL")?;
    let scheme = uri.scheme_str().context("service URL needs a scheme")?;
    let authority = uri.authority().context("service URL needs a host")?;
    ensure!(
        !authority.as_str().contains('@') && uri.query().is_none() && !value.contains('#'),
        "service URL cannot contain credentials or query parameters"
    );
    ensure!(
        scheme == "https" || (scheme == "http" && uri.host() == Some("127.0.0.1")),
        "service URL must use TLS"
    );
    Ok(value.trim_end_matches('/').to_string())
}

pub(super) fn checked_url(base: &str, suffix: &str) -> Result<String> {
    let base = endpoint(base)?;
    ensure!(
        suffix.starts_with('/')
            && !suffix.starts_with("//")
            && !suffix.contains(['\r', '\n', '#'])
            && !suffix.contains(".."),
        "invalid service path"
    );
    Ok(format!("{base}{suffix}"))
}

fn quote(value: &str) -> Result<String> {
    ensure!(
        !value.contains(['\r', '\n', '\0']),
        "invalid transport value"
    );
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

fn curl(
    url: &str,
    method: &str,
    token: &str,
    body: Option<&serde_json::Value>,
) -> Result<(Command, Vec<u8>)> {
    ensure!(
        matches!(method, "GET" | "POST" | "PUT" | "DELETE"),
        "unsupported service method"
    );
    ensure!(
        !token.is_empty() && token.len() <= 8192,
        "invalid service credential"
    );
    let mut config = format!(
        "url = {}\nrequest = {}\nheader = {}\n",
        quote(url)?,
        quote(method)?,
        quote(&format!("Authorization: Bearer {token}"))?
    );
    if let Some(body) = body {
        let body = serde_json::to_string(body)?;
        ensure!(body.len() <= JSON_CAP, "service request exceeds limit");
        config.push_str(&format!(
            "header = \"Content-Type: application/json\"\ndata-binary = {}\n",
            quote(&body)?
        ));
    }
    let mut command = clean_command("curl");
    command.args([
        "--disable",
        "--silent",
        "--show-error",
        "--max-time",
        "12",
        "--connect-timeout",
        "4",
        "--proto",
        "=http,https",
        "--proto-redir",
        "=https",
        "--max-redirs",
        "0",
        "--config",
        "-",
    ]);
    Ok((command, config.into_bytes()))
}

pub(super) struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}
impl Response {
    pub fn json<T: DeserializeOwned>(self) -> Result<T> {
        ensure!(
            (200..300).contains(&self.status),
            "service request returned HTTP {}",
            self.status
        );
        serde_json::from_slice(&self.body).context("invalid service response")
    }
}

pub(super) async fn request(
    base: &str,
    path: &str,
    method: &str,
    token: &str,
    body: Option<&serde_json::Value>,
) -> Result<Response> {
    let url = checked_url(base, path)?;
    let (mut command, input) = curl(&url, method, token, body)?;
    command.args(["--write-out", "\n%{http_code}"]);
    let timeout = if path.ends_with("/pro/handoff") {
        command.args(["--max-time", "90"]);
        Duration::from_secs(93)
    } else {
        Duration::from_secs(15)
    };
    let mut output = run(command, input, timeout, JSON_CAP + 4).await?;
    ensure!(
        output.success && output.stdout.len() >= 4,
        "service is unavailable"
    );
    let marker = output.stdout.len() - 4;
    ensure!(output.stdout[marker] == b'\n', "invalid service response");
    let status = std::str::from_utf8(&output.stdout[marker + 1..])?.parse()?;
    output.stdout.truncate(marker);
    Ok(Response {
        status,
        body: output.stdout,
    })
}

/// The clean mirror repository owns its Git configuration. A helper reads its
/// password from the child environment only; neither argv nor .git/config
/// contains the credential, and redirects are forbidden.
pub(super) async fn git(dir: &Path, credentials: Option<(&str, &str)>) -> Result<Command> {
    let mut command = clean_command(mirror_git().await?.binary);
    command
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "http.followRedirects=false",
            "-c",
            "credential.helper=",
            "-c",
            "pack.threads=1",
            "-c",
            "pack.windowMemory=16m",
            "-c",
            "core.bigFileThreshold=1m",
        ]);
    if let Some((username, password)) = credentials {
        command.env("CHIMAERA_MIRROR_USERNAME", username).env("CHIMAERA_MIRROR_PASSWORD", password)
            .args(["-c", "credential.helper=!f() { test \"$1\" = get || exit 0; printf 'username=%s\\npassword=%s\\n' \"$CHIMAERA_MIRROR_USERNAME\" \"$CHIMAERA_MIRROR_PASSWORD\"; }; f"]);
    }
    Ok(command)
}

pub(super) async fn git_output(
    mut command: Command,
    args: &[&str],
    input: Vec<u8>,
) -> Result<Vec<u8>> {
    command.args(args);
    // The service permits a streamed transfer for fifteen minutes. Keep a
    // finite client deadline just beyond it so large initial histories can
    // complete; cancellation still kills the child and releases its permit.
    let timeout = if args
        .first()
        .is_some_and(|arg| matches!(*arg, "fetch" | "push" | "clone"))
    {
        Duration::from_secs(16 * 60)
    } else {
        Duration::from_secs(45)
    };
    let compatibility_hint = http_transfer(args) && !known_fixed(mirror_git().await?.version);
    let result = run(command, input, timeout, PATH_CAP).await;
    let output = if compatibility_hint {
        result.context(HTTP_GIT_HINT)?
    } else {
        result?
    };
    if !output.success {
        if compatibility_hint {
            bail!(HTTP_GIT_HINT);
        }
        bail!("mirror Git operation failed");
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mirror_git_selection_preserves_modern_path_and_only_uses_fixed_macos_fallback() {
        let old = Some((2, 43, 0));
        let fixed = Some((2, 45, 0));
        let newer = Some((2, 54, 0));
        for path in [fixed, newer, Some((3, 0, 0))] {
            for system in [None, old, fixed, newer] {
                assert_eq!(choose_git(path, system, true).binary, "git");
            }
        }
        for path in [None, old, Some((2, 44, 9))] {
            for system in [fixed, newer] {
                let selected = choose_git(path, system, true);
                assert_eq!(selected.binary, "/usr/bin/git");
                assert_eq!(selected.version, system);
            }
            for system in [None, old] {
                let selected = choose_git(path, system, true);
                assert_eq!(selected.binary, "git");
                assert_eq!(selected.version, path);
            }
        }
        for path in [None, old, fixed, newer] {
            let selected = choose_git(path, newer, false);
            assert_eq!(selected.binary, "git");
            assert_eq!(selected.version, path);
        }
    }

    #[test]
    fn version_parser_and_diagnostic_scope_are_conservative() {
        assert_eq!(
            git_version(b"git version 2.54.0 (Apple Git-157)\n"),
            Some((2, 54, 0))
        );
        assert_eq!(
            git_version(b"git version 2.45.1.windows.1\n"),
            Some((2, 45, 1))
        );
        for invalid in [
            b"git version 2.45".as_slice(),
            b"banner\ngit version 2.54.0",
            b"git version unknown",
            b"git version 2.x.0",
            b"\xff",
        ] {
            assert_eq!(git_version(invalid), None);
        }
        assert!(http_transfer(&[
            "push",
            "--atomic",
            "https://mirror.test/repository.git",
            "refs/heads/main"
        ]));
        assert!(http_transfer(&[
            "fetch",
            "http://127.0.0.1:1234/repository.git"
        ]));
        for args in [
            vec!["fetch", "/local/repository.git"],
            vec!["push", "git@example.test:repo"],
            vec!["config", "remote.origin.url", "https://example.test/repo"],
            vec!["status"],
        ] {
            assert!(!http_transfer(&args));
        }
    }

    #[tokio::test]
    async fn concurrent_selection_is_cached_and_commands_keep_credentials_out_of_arguments() {
        let (first, second) = tokio::join!(mirror_git(), mirror_git());
        let (first, second) = (first.unwrap(), second.unwrap());
        assert!(std::ptr::eq(first, second));
        let command = git(Path::new("."), Some(("fixture-user", "fixture-password")))
            .await
            .unwrap();
        let command = command.as_std();
        assert_eq!(command.get_program(), first.binary);
        assert!(!command
            .get_args()
            .any(|arg| arg.to_string_lossy().contains("fixture-password")));
        assert!(command
            .get_envs()
            .any(|(key, value)| key == "CHIMAERA_MIRROR_PASSWORD"
                && value.is_some_and(|value| value == "fixture-password")));
        assert!(!command.get_envs().any(|(key, _)| key == "GIT_TRACE"
            || key == "GIT_TRACE_CURL"
            || key == "GIT_EXEC_PATH"));
    }

    #[tokio::test]
    async fn capacity_timeout_does_not_cache_unknown_git() {
        let cell = OnceCell::new();
        let children = Semaphore::new(0);
        assert!(discover_git(&cell, &children, Duration::from_millis(1))
            .await
            .is_err());
        assert!(cell.get().is_none());
        children.add_permits(1);
        let selected = discover_git(&cell, &children, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(std::ptr::eq(selected, cell.get().unwrap()));
        assert_eq!(children.available_permits(), 1);
    }

    #[test]
    fn transport_refuses_credentials_cleartext_and_config_injection() {
        for value in [
            "http://example.test",
            "https://a:b@example.test",
            "https://example.test/?token=x",
            "https://example.test/#fragment",
            "https://example.test\nurl=bad",
            "file:///etc/passwd",
        ] {
            assert!(endpoint(value).is_err(), "{value}");
        }
        assert_eq!(
            endpoint("https://example.test/prefix/").unwrap(),
            "https://example.test/prefix"
        );
        assert!(endpoint("http://127.0.0.1:1234").is_ok());
        assert!(quote("secret\nurl=bad").is_err());
        assert!(checked_url("https://example.test", "//other.test").is_err());
        assert!(checked_url("https://example.test", "/../token").is_err());
    }
}
