//! Bounded subprocess transports keep optional TLS and Git out of the daemon.
use std::{
    collections::HashSet,
    future::Future,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, LazyLock, Mutex,
    },
    time::Duration,
};

use anyhow::{bail, ensure, Context, Result};
use serde::de::DeserializeOwned;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{OnceCell, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore},
};

static CHILDREN: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(2)));
/// Account and keeper requests have their own small budget: a lease renewal
/// must never queue behind a 16-minute push or fetch holding both Git slots.
static REQUESTS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(6)));
static MIRROR_GIT: OnceCell<MirrorGit> = OnceCell::const_new();
static UNCERTAIN_CACHES: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static UNCERTAIN_OVERFLOW: AtomicBool = AtomicBool::new(false);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
tokio::task_local! {
    static CACHE_GUARD: CacheContext;
}

#[derive(Clone)]
struct CacheContext {
    workspace: String,
    _guard: Arc<OwnedMutexGuard<()>>,
}

fn uncertain(workspace: Option<&str>) {
    let Some(workspace) = workspace else {
        return;
    };
    let mut caches = crate::lock(&UNCERTAIN_CACHES);
    if caches.len() >= 128 && !caches.contains(workspace) {
        UNCERTAIN_OVERFLOW.store(true, Ordering::Release);
    } else {
        caches.insert(workspace.to_owned());
    }
}

/// Cache users retain the same exclusion inside an owned subprocess task.
/// Cancellation of the request must not authorize the next cache writer.
pub(super) async fn cache_scope<T>(
    workspace: &str,
    guard: Arc<OwnedMutexGuard<()>>,
    future: impl Future<Output = T>,
) -> T {
    CACHE_GUARD
        .scope(
            CacheContext {
                workspace: workspace.to_owned(),
                _guard: guard,
            },
            future,
        )
        .await
}

pub(super) fn cache_quiescent(workspace: &str) -> Result<()> {
    ensure!(
        !UNCERTAIN_OVERFLOW.load(Ordering::Acquire)
            && !crate::lock(&UNCERTAIN_CACHES).contains(workspace),
        "Mirror helper cleanup could not be verified; cache recovery is unavailable"
    );
    Ok(())
}
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

async fn probe_git(binary: &str, permit: Arc<OwnedSemaphorePermit>) -> Option<GitVersion> {
    let mut command = clean_command(binary);
    command.arg("--version");
    // Discovery already reserved a child slot. Each credential-free probe
    // has its own short deadline and never starts a login shell.
    let output = run_reserved(command, vec![], Duration::from_secs(2), 256, permit, None)
        .await
        .ok()?;
    output
        .success
        .then(|| git_version(&output.stdout))
        .flatten()
}

async fn discover_git<'a>(
    cell: &'a OnceCell<MirrorGit>,
    children: &Arc<Semaphore>,
    queue_timeout: Duration,
) -> Result<&'a MirrorGit> {
    cell.get_or_try_init(|| async {
        // Capacity pressure is transient: leave the cell empty so a later
        // mirror attempt retries instead of permanently selecting unknown Git.
        let permit = Arc::new(
            tokio::time::timeout(queue_timeout, children.clone().acquire_owned())
                .await
                .context("mirror Git discovery is waiting for another helper; retry shortly")??,
        );
        let path = probe_git("git", permit.clone()).await;
        let system = if cfg!(target_os = "macos") && !known_fixed(path) {
            probe_git("/usr/bin/git", permit.clone()).await
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
    diagnostic: &'static str,
    damaged_object: bool,
    /// The mirror service answered 503: its admission queue was full and
    /// nothing was transferred.
    busy: bool,
}

/// A busy mirror answers 503 with `Retry-After` before doing any work, so a
/// transfer it refused is safe to repeat. Git does not surface response
/// headers; the daemon waits the service's documented Retry-After (10 s),
/// growing with each attempt, plus jitter, a bounded number of times.
const MIRROR_BUSY_RETRIES: u32 = 3;
#[cfg(not(test))]
const MIRROR_RETRY_AFTER: Duration = Duration::from_secs(10);
#[cfg(test)]
const MIRROR_RETRY_AFTER: Duration = Duration::from_millis(20);

fn busy_delay(attempt: u32) -> Duration {
    let jitter_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |now| now.subsec_nanos() as u64)
        % (MIRROR_RETRY_AFTER.as_millis() as u64 / 2).max(1);
    MIRROR_RETRY_AFTER * attempt + Duration::from_millis(jitter_ms)
}

/// Rebuilds a helper command created by `git()` (always on a cleared
/// environment) so a refused transfer can be run again unchanged.
fn replicate(command: &Command) -> Command {
    let source = command.as_std();
    let mut copy = Command::new(source.get_program());
    copy.env_clear();
    for (key, value) in source.get_envs() {
        match value {
            Some(value) => copy.env(key, value),
            None => copy.env_remove(key),
        };
    }
    copy.args(source.get_args());
    if let Some(directory) = source.get_current_dir() {
        copy.current_dir(directory);
    }
    copy
}

impl Output {
    pub(super) fn object_damage(&self) -> bool {
        self.damaged_object
            || self.stdout.split(|b| *b == b'\n').any(|line| {
                let Ok(line) = std::str::from_utf8(line) else {
                    return false;
                };
                let mut words = line.split_whitespace();
                matches!(words.next(), Some("missing"))
                    && matches!(words.next(), Some("blob" | "tree" | "commit" | "tag"))
                    && words.next().is_some_and(|oid| {
                        matches!(oid.len(), 40 | 64) && oid.bytes().all(|b| b.is_ascii_hexdigit())
                    })
                    && words.next().is_none()
            })
    }
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

/// Git helper slots currently in use (the idle/drain accounting).
pub(super) fn helpers_busy() -> usize {
    2usize.saturating_sub(CHILDREN.available_permits())
}
pub(super) fn helpers_idle() -> bool {
    helpers_busy() == 0
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
    let permit = Arc::new(CHILDREN.clone().acquire_owned().await?);
    run_reserved(command, input, timeout, cap, permit, None).await
}

/// Stream one staged blob into a private, exclusively created file. The owned
/// helper retains cache exclusion through cancellation/descendant cleanup;
/// rejected or incomplete bytes never survive as a successful artifact.
pub(super) async fn run_file(
    command: Command,
    destination: PathBuf,
    timeout: Duration,
    cap: u64,
) -> Result<u64> {
    let permit = Arc::new(CHILDREN.clone().acquire_owned().await?);
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(&destination).await?;
    let sink = FileSink {
        file,
        destination: destination.clone(),
        cap,
    };
    let output = match run_reserved(command, vec![], timeout, 0, permit, Some(sink)).await {
        Ok(output) => output,
        Err(error) => {
            let _ = tokio::fs::remove_file(&destination).await;
            return Err(error);
        }
    };
    ensure!(output.success, "staged Git blob export failed");
    Ok(tokio::fs::metadata(destination).await?.len())
}

struct FileSink {
    file: tokio::fs::File,
    destination: PathBuf,
    cap: u64,
}

async fn run_reserved(
    command: Command,
    input: Vec<u8>,
    timeout: Duration,
    cap: usize,
    permit: Arc<OwnedSemaphorePermit>,
    file: Option<FileSink>,
) -> Result<Output> {
    let cache = CACHE_GUARD.try_with(Clone::clone).ok();
    if let Some(cache) = &cache {
        cache_quiescent(&cache.workspace)?;
    }
    let (cancel, canceled) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let workspace = cache.as_ref().map(|cache| cache.workspace.as_str());
        let _permit = permit;
        let mut completion = HelperCompletion {
            verified: false,
            workspace,
        };
        let destination = file.as_ref().map(|sink| sink.destination.clone());
        let result = run_owned(command, input, timeout, cap, canceled, workspace, file).await;
        if !matches!(&result, Ok(output) if output.success) {
            if let Some(destination) = destination {
                tokio::fs::remove_file(destination).await?;
            }
        }
        completion.verified = true;
        result
    });
    let result = task.await.context("mirror helper task failed")?;
    drop(cancel);
    result
}

struct HelperCompletion<'a> {
    verified: bool,
    workspace: Option<&'a str>,
}
impl Drop for HelperCompletion<'_> {
    fn drop(&mut self) {
        if !self.verified {
            uncertain(self.workspace);
        }
    }
}

struct Helper<'a> {
    child: tokio::process::Child,
    cleaned: bool,
    signaled: bool,
    workspace: Option<&'a str>,
    #[cfg(unix)]
    group: Option<rustix::process::Pid>,
}
impl Helper<'_> {
    fn stop(&mut self) {
        if self.signaled {
            return;
        }
        self.signaled = true;
        #[cfg(unix)]
        if let Some(group) = self.group {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        let _ = self.child.start_kill();
    }

    async fn exited(&mut self) -> std::io::Result<bool> {
        #[cfg(unix)]
        {
            use rustix::process::{waitid, WaitId, WaitIdOptions};
            let group = self
                .group
                .ok_or_else(|| std::io::Error::other("helper identity unavailable"))?;
            let mut changed =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
            loop {
                match waitid(
                    WaitId::Pid(group),
                    WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
                ) {
                    Ok(Some(status)) => return Ok(status.exit_status() == Some(0)),
                    Ok(None) => {}
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(error) => return Err(error.into()),
                }
                // Keep the exited leader unreaped until the group signal. Its
                // reserved PID cannot become an unrelated process group.
                tokio::select! {
                    _ = changed.recv() => {},
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {},
                }
            }
        }
        #[cfg(not(unix))]
        Ok(self.child.wait().await?.success())
    }
    async fn finish(&mut self, _completed: bool) -> Result<()> {
        self.stop();
        let cleanup = async {
            self.child.wait().await?;
            #[cfg(not(unix))]
            ensure!(
                _completed,
                "mirror helper descendant cleanup is unsupported"
            );
            #[cfg(unix)]
            if let Some(group) = self.group {
                loop {
                    match rustix::process::test_kill_process_group(group) {
                        Err(rustix::io::Errno::SRCH) => break,
                        // macOS can transiently return EPERM while a killed
                        // orphan group is being reaped. It still counts as
                        // present: retain exclusion until ESRCH or timeout.
                        Ok(()) | Err(rustix::io::Errno::PERM) => {
                            tokio::time::sleep(Duration::from_millis(10)).await
                        }
                        Err(rustix::io::Errno::INTR) => continue,
                        Err(_) => bail!("mirror helper group cleanup failed"),
                    }
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        let result = tokio::time::timeout(CLEANUP_TIMEOUT, cleanup).await;
        if !matches!(result, Ok(Ok(()))) {
            uncertain(self.workspace);
            bail!("mirror helper cleanup could not be verified");
        }
        #[cfg(unix)]
        {
            self.group = None;
        }
        self.cleaned = true;
        Ok(())
    }
}
impl Drop for Helper<'_> {
    fn drop(&mut self) {
        if !self.cleaned {
            uncertain(self.workspace);
        }
        self.stop();
    }
}

async fn run_owned(
    mut command: Command,
    input: Vec<u8>,
    timeout: Duration,
    cap: usize,
    mut canceled: tokio::sync::oneshot::Receiver<()>,
    workspace: Option<&str>,
    mut file: Option<FileSink>,
) -> Result<Output> {
    if matches!(
        canceled.try_recv(),
        Ok(()) | Err(tokio::sync::oneshot::error::TryRecvError::Closed)
    ) {
        bail!("mirror helper request canceled");
    }
    #[cfg(unix)]
    command.process_group(0);
    let child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("could not start mirror helper")?;
    #[cfg(unix)]
    let group = child
        .id()
        .and_then(|id| rustix::process::Pid::from_raw(id as i32));
    let mut helper = Helper {
        child,
        cleaned: false,
        signaled: false,
        workspace,
        #[cfg(unix)]
        group,
    };
    let work = async {
        let mut stdin = helper
            .child
            .stdin
            .take()
            .context("helper input unavailable")?;
        let stdout = helper
            .child
            .stdout
            .take()
            .context("helper output unavailable")?;
        let stderr = helper
            .child
            .stderr
            .take()
            .context("helper diagnostics unavailable")?;
        let send = async move {
            if !input.is_empty() {
                stdin.write_all(&input).await?;
            }
            stdin.shutdown().await
        };
        let (_, stdout, stderr, status) = tokio::try_join!(
            send,
            read_output(stdout, cap, &mut file),
            read_bounded(stderr, 16 * 1024),
            helper.exited()
        )?;
        let diagnostic = helper_diagnostic(&stderr);
        let text = String::from_utf8_lossy(&stderr).to_ascii_lowercase();
        let damaged_object = diagnostic == "repository"
            && [
                "bad object",
                "invalid object",
                "bad tree",
                "not a valid object name",
                "unable to read tree",
                "corrupt",
            ]
            .iter()
            .any(|pattern| text.contains(pattern));
        // Git reports an HTTP status as "The requested URL returned error:
        // 503" (or "HTTP 503" for a failed RPC); only 503 means "not admitted".
        let busy = !status && (text.contains("error: 503") || text.contains("http 503"));
        Ok::<_, anyhow::Error>(Output {
            success: status,
            stdout,
            diagnostic,
            damaged_object,
            busy,
        })
    };
    let result = tokio::select! {
        result = tokio::time::timeout(timeout, work) => result.context("mirror helper timed out").and_then(|v| v),
        _ = &mut canceled => Err(anyhow::anyhow!("mirror helper request canceled")),
    };
    let cleanup = helper.finish(result.is_ok()).await;
    drop(file);
    cleanup?;
    result
}

async fn read_output(
    mut input: impl AsyncRead + Unpin,
    cap: usize,
    file: &mut Option<FileSink>,
) -> std::io::Result<Vec<u8>> {
    let Some(sink) = file else {
        return read_bounded(input, cap).await;
    };
    let mut buffer = [0u8; 16 * 1024];
    let mut overlap = Vec::with_capacity(128);
    let mut length = 0u64;
    loop {
        let count = input.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        length = length
            .checked_add(count as u64)
            .ok_or_else(|| std::io::Error::other("staged Git blob exceeds limit"))?;
        if length > sink.cap {
            return Err(std::io::Error::other("staged Git blob exceeds limit"));
        }
        overlap.extend_from_slice(&buffer[..count]);
        if super::policy::contains_credential(&overlap) {
            return Err(std::io::Error::other(
                "staged Git blob contains credentials",
            ));
        }
        sink.file.write_all(&buffer[..count]).await?;
        let keep = overlap.len().saturating_sub(128);
        overlap.drain(..keep);
    }
    sink.file.sync_all().await?;
    Ok(Vec::new())
}

// Never retain helper stderr: Git can include credentials, remote URLs or
// private paths. Only fixed categories may reach diagnostic logs.
fn helper_diagnostic(stderr: &[u8]) -> &'static str {
    let text = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    for (category, patterns) in [
        ("http_bad_request", &["http 400", "error: 400"][..]),
        ("http_unauthorized", &["http 401", "http 403"][..]),
        ("http_too_large", &["http 413", "error: 413"][..]),
        (
            "http_service_failure",
            &["http 500", "http 502", "http 503", "http 504"][..],
        ),
        ("http2_stream", &["curl 92", "http/2 stream"][..]),
        (
            "truncated_transfer",
            &["curl 18", "transfer closed with"][..],
        ),
        ("receive_failure", &["curl 56"][..]),
        ("tls_handshake", &["curl 35"][..]),
        ("early_eof", &["early eof"][..]),
        ("disconnect", &["unexpected disconnect"][..]),
        ("pack_index", &["index-pack failed"][..]),
        (
            "authentication",
            &[
                "authentication failed",
                "could not read username",
                "could not read password",
                "error: 401",
                "error: 403",
            ][..],
        ),
        ("http_missing", &["error: 404", "repository not found"][..]),
        (
            "http_unavailable",
            &["error: 500", "error: 502", "error: 503", "error: 504"][..],
        ),
        (
            "tls",
            &["ssl certificate", "certificate verify", "tls connection"][..],
        ),
        (
            "network",
            &[
                "could not resolve",
                "failed to connect",
                "connection refused",
                "connection reset",
                "empty reply from server",
            ][..],
        ),
        (
            "helper_missing",
            &[
                "is not a git command",
                "cannot run",
                "unable to find remote helper",
            ][..],
        ),
        ("disk_full", &["no space left on device"][..]),
        (
            "permission",
            &["permission denied", "dubious ownership"][..],
        ),
        (
            "repository",
            &[
                "not a git repository",
                "bad object",
                "invalid object",
                "bad tree",
                "not a valid object name",
                "unable to read tree",
                "corrupt",
            ][..],
        ),
        (
            "reference",
            &[
                "couldn't find remote ref",
                "could not find remote ref",
                "invalid refspec",
                "cannot lock ref",
                "ambiguous argument",
            ][..],
        ),
        ("lock", &["index.lock", "another git process"][..]),
        (
            "transfer",
            &[
                "rpc failed",
                "early eof",
                "unexpected disconnect",
                "index-pack failed",
            ][..],
        ),
    ] {
        if patterns.iter().any(|pattern| text.contains(pattern)) {
            return category;
        }
    }
    "unknown"
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
    wake: bool,
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
    if wake {
        // The keeper wakes a suspended cloud machine only for a deliberate
        // interaction; its HTTP adapter forwards this marker.
        config.push_str("header = \"X-Chimaera-Wake: interaction\"\n");
    }
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

/// The keeper's answer while the account is down (503 `account_unavailable`):
/// transient, never a sign-in problem and never a failed return.
pub(super) fn account_unavailable(response: &Response) -> bool {
    response.status == 503
        && serde_json::from_slice::<serde_json::Value>(&response.body)
            .is_ok_and(|value| value["error"] == "account_unavailable")
}
/// The account's 403 once a plan has ended and the time to bring cloud work
/// home has passed (`{"error":"return_window_ended"}`). It becomes an error of
/// its own so the page can say so plainly, instead of a bare "HTTP 403".
pub(super) const RETURN_WINDOW_ENDED: &str = "return_window_ended";
pub(super) fn return_window_ended(response: &Response) -> bool {
    response.status == 403
        && serde_json::from_slice::<serde_json::Value>(&response.body)
            .is_ok_and(|value| value["error"] == RETURN_WINDOW_ENDED)
}
/// The error a 401 answer becomes: the account no longer accepts this
/// daemon's credential (the lease loop renews it at once, see `engine`).
pub(super) const UNAUTHORIZED: &str = "service request returned HTTP 401";

impl Response {
    pub fn json<T: DeserializeOwned>(self) -> Result<T> {
        ensure!(self.status != 401, UNAUTHORIZED);
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
    request_inner(base, path, method, token, body, false).await
}

/// A deliberate interaction with a cloud machine through the keeper: carries
/// wake intent, so a suspended machine is started to answer it.
pub(super) async fn request_waking(
    base: &str,
    path: &str,
    method: &str,
    token: &str,
    body: Option<&serde_json::Value>,
) -> Result<Response> {
    request_inner(base, path, method, token, body, true).await
}

async fn request_inner(
    base: &str,
    path: &str,
    method: &str,
    token: &str,
    body: Option<&serde_json::Value>,
    wake: bool,
) -> Result<Response> {
    let url = checked_url(base, path)?;
    let (mut command, input) = curl(&url, method, token, body, wake)?;
    command.args(["--write-out", "\n%{http_code}"]);
    let timeout = if path.ends_with("/pro/handoff") {
        command.args(["--max-time", "90"]);
        Duration::from_secs(93)
    } else {
        Duration::from_secs(15)
    };
    let permit = Arc::new(REQUESTS.clone().acquire_owned().await?);
    let mut output = run_reserved(command, input, timeout, JSON_CAP + 4, permit, None).await?;
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
    let selected = mirror_git().await?;
    ensure!(
        selected
            .version
            .is_some_and(|version| version >= (2, 36, 0)),
        "Cloud mirroring needs Git 2.36 or newer to save snapshots safely; update Git and retry"
    );
    let mut command = clean_command(selected.binary);
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
            "core.fsync=committed,reference,pack-metadata",
            "-c",
            "core.fsyncMethod=fsync",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
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
    let transfer = http_transfer(args);
    let compatibility_hint = transfer && !known_fixed(mirror_git().await?.version);
    let mut attempt = 0;
    let output = loop {
        let attempt_command = if transfer && attempt < MIRROR_BUSY_RETRIES {
            replicate(&command)
        } else {
            std::mem::replace(&mut command, Command::new("git"))
        };
        let result = run(attempt_command, input.clone(), timeout, PATH_CAP).await;
        let output = if compatibility_hint {
            result.context(HTTP_GIT_HINT)?
        } else {
            result?
        };
        if !(transfer && output.busy && attempt < MIRROR_BUSY_RETRIES) {
            break output;
        }
        attempt += 1;
        tracing::info!(attempt, "mirror busy; retrying the transfer");
        tokio::time::sleep(busy_delay(attempt)).await;
    };
    if !output.success {
        let operation = match args.first().copied() {
            Some("fetch") => "fetch",
            Some("push") => "push",
            Some("clone") => "clone",
            Some("show") => "show",
            Some("rev-parse") => "rev_parse",
            Some("cat-file") => "cat_file",
            _ => "local_git",
        };
        tracing::warn!(
            operation,
            category = output.diagnostic,
            "mirror Git helper failed"
        );
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

    #[cfg(unix)]
    #[tokio::test]
    async fn file_output_is_bounded_secret_scanned_and_removed_on_failed_or_cancelled_helpers() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-file-helper-{}",
            chimaera_core::generate_token()
        ));
        tokio::fs::create_dir(&root).await.unwrap();
        let output = root.join("blob");
        let mut good = Command::new("sh");
        good.args(["-c", "printf blob"]);
        assert_eq!(
            run_file(good, output.clone(), Duration::from_secs(5), 4)
                .await
                .unwrap(),
            4
        );
        assert_eq!(tokio::fs::read(&output).await.unwrap(), b"blob");
        tokio::fs::remove_file(&output).await.unwrap();
        for (script, cap) in [
            ("printf overflow", 2),
            ("printf data; exit 1", 100),
            (
                "dd if=/dev/zero bs=16382 count=1 2>/dev/null; printf sk-abcdefghijklmnop",
                32 * 1024,
            ),
        ] {
            let mut command = Command::new("sh");
            command.args(["-c", script]);
            assert!(
                run_file(command, output.clone(), Duration::from_secs(5), cap)
                    .await
                    .is_err()
            );
            assert!(!output.exists());
        }
        let missing = Command::new(root.join("missing-helper"));
        assert!(
            run_file(missing, output.clone(), Duration::from_secs(5), 100)
                .await
                .is_err()
        );
        assert!(!output.exists());
        let ready = root.join("ready");
        let late = root.join("late");
        let release = root.join("release");
        let mut command = Command::new("sh");
        // A wall-clock delay could expire before a loaded test runtime observes
        // ready. Release the descendant only after cancellation cleanup instead.
        command.env("HELPER_READY", &ready).env("HELPER_LATE", &late).env("HELPER_RELEASE", &release)
            .args(["-c", "printf partial; (printf ready > \"$HELPER_READY\"; while [ ! -e \"$HELPER_RELEASE\" ]; do sleep 0.05; done; printf escaped > \"$HELPER_LATE\") & wait"]);
        let destination = output.clone();
        let caller = tokio::spawn(async move {
            run_file(command, destination, Duration::from_secs(5), 100).await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !ready.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!late.exists());
        caller.abort();
        let _ = caller.await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while output.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!late.exists());
        tokio::fs::write(&release, b"release").await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!late.exists());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[cfg(unix)]
    async fn delayed_writer(cancel: bool, close_pipes: bool) {
        let root = std::env::temp_dir().join(format!(
            "chimaera-helper-lifetime-{}",
            chimaera_core::generate_token()
        ));
        tokio::fs::create_dir(&root).await.unwrap();
        let ready = root.join("ready");
        let written = root.join("late-write");
        let cache = Arc::new(tokio::sync::Mutex::new(()));
        let guard = Arc::new(cache.clone().lock_owned().await);
        let permits = Arc::new(Semaphore::new(1));
        let permit = Arc::new(permits.clone().acquire_owned().await.unwrap());
        let mut command = clean_command("/bin/sh");
        command
            .env("HELPER_READY", &ready)
            .env("HELPER_WRITE", &written);
        command.args(["-c", if close_pipes {
            "(exec >/dev/null 2>&1; printf ready > \"$HELPER_READY\"; sleep 0.4; printf escaped > \"$HELPER_WRITE\") & exit 0"
        } else {
            "(printf ready > \"$HELPER_READY\"; sleep 0.4; printf escaped > \"$HELPER_WRITE\") & wait"
        }]);
        let workspace = format!("w-{}", chimaera_core::generate_token());
        let owned_workspace = workspace.clone();
        let task = tokio::spawn(async move {
            cache_scope(&owned_workspace, guard, async move {
                run_reserved(
                    command,
                    vec![],
                    if cancel {
                        Duration::from_secs(10)
                    } else {
                        Duration::from_millis(100)
                    },
                    1024,
                    permit,
                    None,
                )
                .await
            })
            .await
        });
        if cancel {
            tokio::time::timeout(Duration::from_secs(3), async {
                while !tokio::fs::try_exists(&ready).await.unwrap() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            task.abort();
            assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        } else if close_pipes {
            assert!(task.await.unwrap().unwrap().success);
        } else {
            assert!(task.await.unwrap().is_err());
        }
        // The next cache operation is permitted only after the entire original
        // process group is gone, including a child that closed its stdio.
        let _next = tokio::time::timeout(Duration::from_secs(6), cache.lock())
            .await
            .unwrap();
        assert_eq!(permits.available_permits(), 1);
        cache_quiescent(&workspace).unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!tokio::fs::try_exists(written).await.unwrap());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn canceled_helper_retains_cache_until_delayed_descendant_is_stopped() {
        delayed_writer(true, false).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timed_out_helper_and_closed_pipe_descendant_cannot_outlive_cache_exclusion() {
        delayed_writer(false, false).await;
        delayed_writer(false, true).await;
    }

    #[test]
    fn missing_object_diagnostics_require_exact_bounded_identity() {
        let output = |text: &str| Output {
            success: false,
            stdout: text.as_bytes().to_vec(),
            diagnostic: "unknown",
            damaged_object: false,
            busy: false,
        };
        assert!(output("missing blob 0123456789012345678901234567890123456789\n").object_damage());
        for text in [
            "missing blob ../private",
            "missing resource 0123456789012345678901234567890123456789",
            "missing blob 0123456789012345678901234567890123456789 extra",
        ] {
            assert!(!output(text).object_damage());
        }
    }

    #[test]
    fn uncertain_cleanup_stays_bound_to_its_workspace() {
        let failed = format!("w-{}", chimaera_core::generate_token());
        let sibling = format!("w-{}", chimaera_core::generate_token());
        uncertain(Some(&failed));
        assert!(cache_quiescent(&failed).is_err());
        assert!(cache_quiescent(&sibling).is_ok());
        // The record is independent of the weak cache-mutex entry and account
        // Configure lifetime; a later lock allocation cannot erase it.
        uncertain(None);
        assert!(cache_quiescent(&failed).is_err());
    }

    #[test]
    fn diagnostics_are_fixed_categories_without_remote_or_credential_text() {
        assert_eq!(helper_diagnostic(b"fatal: Authentication failed for 'https://fixture-user:fixture-secret@mirror.test/repository.git'"), "authentication");
        assert_eq!(
            helper_diagnostic(b"fatal: cannot run git-remote-https: No such file or directory"),
            "helper_missing"
        );
        assert_eq!(
            helper_diagnostic(b"remote: private response fixture-secret"),
            "unknown"
        );
        assert_eq!(helper_diagnostic(b"fatal: unable to access 'https://mirror.test': The requested URL returned error: 503"), "http_unavailable");
    }

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
        let children = Arc::new(Semaphore::new(0));
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

    /// A busy mirror (503 + Retry-After, nothing admitted) is retried with
    /// backoff a bounded number of times; any other failure is not retried.
    #[tokio::test]
    async fn a_busy_mirror_is_retried_a_bounded_number_of_times() {
        use axum::response::IntoResponse;
        use std::sync::atomic::{AtomicUsize, Ordering};
        for (busy_answers, expected) in [
            (2, 3),
            (usize::MAX, 1 + MIRROR_BUSY_RETRIES as usize),
            (0, 1),
        ] {
            let hits = Arc::new(AtomicUsize::new(0));
            let counted = hits.clone();
            let router = axum::Router::new().fallback(move || {
                let counted = counted.clone();
                async move {
                    if counted.fetch_add(1, Ordering::SeqCst) < busy_answers {
                        (
                            axum::http::StatusCode::SERVICE_UNAVAILABLE,
                            [("retry-after", "10")],
                            "busy",
                        )
                            .into_response()
                    } else {
                        axum::http::StatusCode::NOT_FOUND.into_response()
                    }
                }
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/repository.git", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let directory = std::env::temp_dir().join(format!(
                "chimaera-busy-mirror-{}",
                chimaera_core::generate_token()
            ));
            super::super::mirror::initialize(&directory).await.unwrap();
            let result = git_output(
                git(&directory, None).await.unwrap(),
                &["fetch", &url, "+refs/heads/*:refs/remotes/mirror/*"],
                Vec::new(),
            )
            .await;
            assert!(result.is_err());
            assert_eq!(hits.load(Ordering::SeqCst), expected, "{busy_answers}");
            server.abort();
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
}
