//! Remote daemon orchestration over the system `ssh`: discovery, binary
//! install, daemon start, and port-forward tunnels. Shared by the CLI
//! (`chimaera connect`) and the native shell, so both speak the exact same
//! protocol to a host — including inheriting the user's `~/.ssh/config`
//! (ProxyJump, 2FA) by never reimplementing the ssh client.
//!
//! Every ssh/scp invocation here rides one chimaera-owned ControlMaster (see
//! [`ssh_opts`]): the user authenticates once — password or 2FA — and every
//! subsequent command, tunnel, and new window multiplexes that single
//! connection with no further prompts, kept warm by `ControlPersist` so
//! opening new things on the host stays instant. The same options set a
//! trust-on-first-use host-key policy, so a freshly installed app can connect
//! to a host it has never seen without a tty to confirm the key.

pub mod hosts;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context};
use chimaera_core::Manifest;
use tokio::process::{Child, Command};

/// The ssh ControlMaster socket path pattern for chimaera connections. `%C`
/// is ssh's own hash of (localhost, remotehost, port, user): unique per
/// destination and short enough to stay under the ~104-char unix-socket path
/// limit a full hostname would blow past. The parent dir is created on demand
/// (ssh will not create it for the socket).
fn control_path() -> String {
    let dir = chimaera_core::data_dir().join("cm");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("failed to create ssh control dir {}: {e}", dir.display());
    }
    dir.join("%C").to_string_lossy().into_owned()
}

/// The `-o` options shared by every chimaera ssh/scp call to a host.
///
/// *ControlMaster* — `ControlMaster=auto` makes the first connection the
/// master and later ones reuse it; `ControlPersist` keeps it warm after
/// clients disconnect so reconnects and new windows skip re-authentication.
///
/// *Host key* — `StrictHostKeyChecking=accept-new` is the trust-on-first-use
/// policy a windowed app needs: ssh has no tty to answer the "authenticity of
/// host … (yes/no)?" prompt, so a freshly installed app connecting to a host
/// it has never seen would otherwise fail outright. `accept-new` records an
/// unknown key automatically but still *refuses* a changed one — keeping the
/// MITM protection that a blanket `=no` would throw away.
///
/// *Liveness* — `ConnectTimeout` bounds the TCP connect so an unreachable
/// host fails in seconds instead of the OS default (minutes) — a hung
/// connect pins the UI in "connecting". `ServerAliveInterval`/`CountMax`
/// make the ControlMaster and `-N` tunnel children *notice* a dead link
/// (laptop sleep, network change) within ~45s and exit; without them a dead
/// tunnel keeps its local listener open for hours, so every liveness probe
/// lies "up" and reconnect becomes a no-op.
fn ssh_opts() -> [String; 14] {
    [
        "-o".into(),
        "ControlMaster=auto".into(),
        "-o".into(),
        format!("ControlPath={}", control_path()),
        "-o".into(),
        "ControlPersist=10m".into(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
        "-o".into(),
        "ConnectTimeout=15".into(),
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
    ]
}

/// An `ssh` command pre-loaded with the shared options, no host yet. For
/// flag-heavy invocations where the destination must come last
/// (`-O cancel -L …`, `-N -L …`); otherwise prefer [`ssh_cmd`].
fn ssh_base() -> Command {
    let mut c = Command::new("ssh");
    c.args(ssh_opts());
    c
}

/// An `ssh` command pre-loaded with the shared ControlMaster options and
/// pointed at `host`, ready for the remote command to be appended. The common
/// shape (`ssh <opts> host <cmd>`); every plain remote command goes through
/// here so they all share one authenticated connection.
fn ssh_cmd(host: &str) -> Command {
    let mut c = ssh_base();
    c.arg(host);
    c
}

/// An `scp` command pre-loaded with the shared options, so a binary copy
/// reuses the connection the probe already authenticated instead of prompting
/// again.
fn scp_cmd() -> Command {
    let mut c = Command::new("scp");
    c.args(ssh_opts());
    c
}

/// Where the connect flow currently is; consumers surface these however
/// fits (tracing lines in the CLI, progress events in the shell).
#[derive(Clone, Debug)]
pub enum Phase {
    /// Probing the host for a running daemon.
    Probing,
    /// Replacing an outdated remote daemon (graceful stop, then redeploy).
    Updating,
    /// Fetching the matching daemon binary from the GitHub release this build
    /// came from (the end-user path: no repo, no `just dist` stash).
    Downloading { target: String },
    /// Copying a chimaera binary to the host.
    Installing { binary: PathBuf },
    /// Starting the daemon on the host.
    Starting,
    /// Waiting for the local port-forward to come up.
    Tunneling { local_port: u16 },
}

/// Options for [`connect`].
#[derive(Default)]
pub struct ConnectOpts {
    /// Local port for the tunnel (defaults to the remote port if free).
    pub local_port: Option<u16>,
    /// Explicit binary to install on the host if chimaera is missing;
    /// otherwise `~/.chimaera/dist/` is searched for a matching build.
    pub binary: Option<PathBuf>,
    /// Replace an outdated remote daemon even when it has live sessions
    /// (they end with it). The stop is always graceful — SIGTERM, never -9.
    pub update_daemon: bool,
}

/// What [`connect`] should do about a daemon already running on the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Same build (and no force): attach to it as-is.
    Reuse,
    /// Replace it: graceful stop, redeploy, restart. Chosen when the builds
    /// differ and it is provably safe (zero live sessions), or when forced.
    Update,
    /// Builds differ but sessions could die (live count > 0 or unknown):
    /// attach to the old daemon and surface the mismatch to the caller.
    ConnectOutdated,
}

/// Pure policy for daemon reuse vs replacement, shared by the remote
/// connect flow and the app's local-daemon startup. `sessions` `None`
/// means the count could not be determined — treated as busy, never as
/// empty. `force` replaces the daemon regardless of build or session count
/// (the explicit `--update-daemon` / host-row affordance).
pub fn update_decision(
    local_build: &str,
    remote_build: Option<&str>,
    sessions: Option<usize>,
    force: bool,
) -> Decision {
    if chimaera_core::builds_match(local_build, remote_build) && !force {
        return Decision::Reuse;
    }
    if force || sessions == Some(0) {
        return Decision::Update;
    }
    Decision::ConnectOutdated
}

/// A failure in the local-forward phase of [`connect`] (port bind / forward
/// setup), as opposed to auth, probe, or install failures. Distinguished so
/// callers retry ONLY these on a fresh local port — blindly re-running the
/// whole connect on an auth failure re-prompts the user's 2FA.
#[derive(Debug)]
pub struct TunnelPhaseError(pub String);

impl std::fmt::Display for TunnelPhaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TunnelPhaseError {}

/// A live port-forward to a remote daemon. Dropping it kills the ssh child
/// (but a ControlMaster-held forward survives — call [`Tunnel::close`] to
/// cancel it explicitly).
pub struct Tunnel {
    pub host: String,
    pub local_port: u16,
    pub manifest: Manifest,
    /// The forward was registered with an ssh ControlMaster and our child
    /// exited 0; the master holds the port, not `child`.
    pub mux_delegated: bool,
    /// The daemon at the far end is an older build than ours, left running
    /// because live sessions (or an unknown count) made replacing it unsafe.
    /// Callers surface this with their explicit update affordance.
    pub outdated: bool,
    /// The connected daemon's build id (`None` = predates build ids).
    pub remote_build: Option<String>,
    /// Live sessions counted on the remote daemon when the update decision
    /// was made; `None` when unneeded (builds matched) or undeterminable.
    pub live_sessions: Option<usize>,
    child: Child,
}

impl Tunnel {
    /// The UI url for this tunnel. The host alias rides along so the UI can
    /// label the window with the name the user actually calls this machine.
    pub fn url(&self) -> String {
        format!(
            "http://127.0.0.1:{}/#token={}&host={}",
            self.local_port, self.manifest.token, self.host
        )
    }

    /// Wait for the tunnel child to exit (never returns for a healthy
    /// direct forward; returns quickly when delegated to a ControlMaster).
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }

    /// Kill the tunnel child and cancel any master-held forward so local
    /// ports don't leak past the session that opened them. Only the forward
    /// is cancelled — the ControlMaster stays (ControlPersist), so reconnects
    /// and other windows on this host keep their authenticated connection.
    pub async fn close(mut self) {
        self.child.kill().await.ok();
        let _ = ssh_base()
            .args(["-O", "cancel", "-L"])
            .arg(format!(
                "{}:127.0.0.1:{}",
                self.local_port, self.manifest.port
            ))
            .arg(&self.host)
            .output()
            .await;
    }
}

/// Whether an HTTP server answers on `127.0.0.1:port` within 2s. A bare TCP
/// connect is NOT a liveness probe here: after laptop sleep an ssh forward's
/// local listener keeps accepting while the connection behind it is dead, so
/// only a served response proves the daemon end-to-end. Any HTTP status
/// counts — even a 401 had to come from the daemon.
pub async fn http_alive(port: u16) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let attempt = async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .ok()?;
        stream
            .write_all(
                format!(
                    "GET /api/v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .ok()?;
        let mut buf = Vec::with_capacity(16);
        while buf.len() < 5 {
            let mut chunk = [0u8; 16];
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        buf.starts_with(b"HTTP/").then_some(())
    };
    tokio::time::timeout(Duration::from_secs(2), attempt)
        .await
        .ok()
        .flatten()
        .is_some()
}

/// Connect to the daemon on `host`, installing and starting it if needed,
/// and bring up a local port-forward. `progress` fires as phases begin.
pub async fn connect(
    host: &str,
    opts: ConnectOpts,
    progress: impl Fn(Phase),
) -> anyhow::Result<Tunnel> {
    // Normalize whatever the caller has (saved entries predate validation;
    // "ssh cluster" typed verbatim reached ssh as one hostname in the field)
    // so every ssh invocation below sees a real destination.
    let host = &hosts::normalize_alias(host)?;
    progress(Phase::Probing);
    let local_build = chimaera_core::BUILD_ID;
    let mut outdated = false;
    let mut live_sessions = None;
    let manifest = match remote_manifest(host).await? {
        Some(m) if remote_alive(host, m.pid).await? => {
            // Only pay for the session-count round trip when it can change
            // the decision (build mismatch, or an explicit update request).
            let sessions = if opts.update_daemon
                || !chimaera_core::builds_match(local_build, m.build.as_deref())
            {
                remote_sessions_count(host, &m).await?
            } else {
                None
            };
            match update_decision(
                local_build,
                m.build.as_deref(),
                sessions,
                opts.update_daemon,
            ) {
                Decision::Reuse => {
                    tracing::info!("daemon already running on {host} (pid {})", m.pid);
                    m
                }
                Decision::Update => {
                    tracing::info!(
                        "replacing daemon on {host} (build {}, ours {local_build}, {} live sessions)",
                        m.build.as_deref().unwrap_or("pre-build-id"),
                        sessions.map_or("unknown".to_string(), |n| n.to_string()),
                    );
                    progress(Phase::Updating);
                    // Secure the replacement binary BEFORE stopping the
                    // running daemon: a failed download/build must never leave
                    // the host with nothing running (the bug that stranded a
                    // stopped daemon when a dev build 404'd on download).
                    let bin = resolve_local_binary(host, opts.binary.as_deref(), &progress).await?;
                    stop_remote(host, m.pid).await?;
                    deploy_binary(host, &bin, &progress).await?;
                    progress(Phase::Starting);
                    start_remote(host).await?
                }
                Decision::ConnectOutdated => {
                    tracing::info!(
                        "daemon on {host} is an older build ({} vs ours {local_build}) but {} — connecting to it as-is",
                        m.build.as_deref().unwrap_or("pre-build-id"),
                        sessions.map_or("its session count is unknown".to_string(), |n| {
                            format!("has {n} live session{}", if n == 1 { "" } else { "s" })
                        }),
                    );
                    outdated = true;
                    live_sessions = sessions;
                    m
                }
            }
        }
        _ => {
            ensure_remote_binary(host, opts.binary.as_deref(), &progress).await?;
            progress(Phase::Starting);
            start_remote(host).await?
        }
    };

    let local_port = pick_local_port(opts.local_port, manifest.port)?;
    progress(Phase::Tunneling { local_port });
    let mut child = spawn_tunnel(host, local_port, manifest.port)?;
    let mux_delegated = wait_for_port(local_port, &mut child).await?;
    tracing::info!(
        "tunnel up: 127.0.0.1:{local_port} -> {host}:{}",
        manifest.port
    );

    Ok(Tunnel {
        host: host.to_string(),
        local_port,
        remote_build: manifest.build.clone(),
        manifest,
        mux_delegated,
        outdated,
        live_sessions,
        child,
    })
}

/// Fetch and parse the remote manifest, if any.
pub async fn remote_manifest(host: &str) -> anyhow::Result<Option<Manifest>> {
    let output = ssh_cmd(host)
        .arg("cat ~/.chimaera/manifest.json 2>/dev/null")
        .output()
        .await
        .context("failed to run ssh")?;
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(serde_json::from_str(text.trim()).ok())
}

/// Whether `pid` is alive on the host (signal 0 probe over ssh).
pub async fn remote_alive(host: &str, pid: u32) -> anyhow::Result<bool> {
    ssh_check(host, &format!("kill -0 {pid} 2>/dev/null")).await
}

/// Count live sessions on the daemon `manifest` describes by asking the
/// daemon itself: `curl` over ssh against its loopback port, authenticated
/// with the manifest's own token. `Ok(None)` = could not determine (no
/// curl, daemon unreachable, bad payload) — callers must treat unknown as
/// busy, never as zero. `Err` only when ssh itself cannot run.
pub async fn remote_sessions_count(
    host: &str,
    manifest: &Manifest,
) -> anyhow::Result<Option<usize>> {
    // The token rides stdin as a curl config line (`--config -`), never in
    // argv: `ps` on a shared login node must not show other users the
    // daemon token. (`-H @-` would be neater but needs curl >= 7.55;
    // `--config -` works on cluster-vintage curls too.)
    let cmd = format!(
        "curl -fsS -m 5 --config - http://127.0.0.1:{}/api/v1/sessions",
        manifest.port
    );
    let mut child = ssh_cmd(host)
        .arg(cmd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to run ssh")?;
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let line = format!("header = \"Authorization: Bearer {}\"\n", manifest.token);
        stdin.write_all(line.as_bytes()).await.ok();
        // Dropping stdin sends EOF, which ends the config for curl.
    }
    let output = child
        .wait_with_output()
        .await
        .context("failed to run ssh")?;
    if !output.status.success() {
        tracing::debug!(
            "session count on {host} unavailable: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return Ok(None);
    }
    Ok(count_alive_sessions(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

/// Parse a `GET /api/v1/sessions` payload and count `alive: true` entries
/// (the list also carries finished sessions for recents/last-words).
/// `None` for anything that is not the expected JSON array.
pub fn count_alive_sessions(payload: &str) -> Option<usize> {
    let value: serde_json::Value = serde_json::from_str(payload.trim()).ok()?;
    Some(
        value
            .as_array()?
            .iter()
            .filter(|s| {
                s.get("alive")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
            })
            .count(),
    )
}

/// Gracefully stop the daemon on `host`: SIGTERM, then poll for exit for up
/// to ~10s. Never escalates to SIGKILL — a daemon that will not die may be
/// holding sessions that must not be torn out from under their owner, so
/// this errors honestly instead.
pub async fn stop_remote(host: &str, pid: u32) -> anyhow::Result<()> {
    tracing::info!("stopping daemon on {host} (pid {pid})");
    ssh_run(host, &format!("kill -TERM {pid}")).await?;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if !remote_alive(host, pid).await? {
            return Ok(());
        }
    }
    bail!(
        "daemon on {host} (pid {pid}) is still running 10s after SIGTERM — \
         refusing to kill -9 it; something is keeping it busy (open UI tabs \
         hold its sockets). Close them and retry, or stop it by hand."
    )
}

/// `uname -sm` on the host, lowercased: e.g. `("linux", "x86_64")`.
pub async fn remote_target(host: &str) -> anyhow::Result<(String, String)> {
    let output = ssh_cmd(host)
        .arg("uname -sm")
        .output()
        .await
        .context("failed to run ssh")?;
    if !output.status.success() {
        bail!(
            "could not detect the OS/arch of {host}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_lowercase();
    let mut parts = text.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some(os), Some(arch)) => Ok((os.to_string(), arch.to_string())),
        _ => bail!("unexpected `uname -sm` output from {host}: {text:?}"),
    }
}

/// The local stash of deployable builds: `~/.chimaera/dist/`. Populated by
/// `just dist` (or by hand); searched when no explicit binary is given.
pub fn dist_dir() -> PathBuf {
    chimaera_core::data_dir().join("dist")
}

/// The expected dist file name for a remote target: static musl for linux
/// (no glibc roulette on clusters), plain names elsewhere.
pub fn dist_name(os: &str, arch: &str) -> String {
    match os {
        "linux" => format!("chimaera-{arch}-linux-musl"),
        other => format!("chimaera-{arch}-{other}"),
    }
}

/// The Rust target triple for a detected `uname -sm` pair, matching the
/// daemon asset names the release workflow publishes (`chimaera-<triple>`).
/// `None` for a target we don't build (no silent guessing — the caller
/// falls back to the explicit-binary / `just dist` message).
pub fn release_triple(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64" | "arm64") => Some("aarch64-unknown-linux-musl"),
        ("darwin", "arm64" | "aarch64") => Some("aarch64-apple-darwin"),
        _ => None,
    }
}

/// A daemon asset resolved from the GitHub releases API: the release version
/// it belongs to, its download URL, and its published sha256 (hex).
struct ReleaseAsset {
    version: String,
    url: String,
    sha256: String,
}

/// Resolve the daemon asset to download for `triple`. Prefers the release this
/// build came from (`v{VERSION}` — so a real release's daemon shares our build
/// id and connect never loops "updating"); falls back to GitHub's `latest`
/// release when there is no matching one, so a dev build (version `0.0.1`) — or
/// any version without a published release — still gets a working daemon
/// instead of a hard 404.
async fn resolve_release_asset(triple: &str) -> anyhow::Result<ReleaseAsset> {
    let asset_name = format!("chimaera-{triple}");
    let version = chimaera_core::VERSION;
    if let Some(a) = release_asset(&format!("tags/v{version}"), &asset_name).await? {
        return Ok(a);
    }
    tracing::info!("no v{version} release with {asset_name}; falling back to the latest release");
    release_asset("latest", &asset_name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no published release provides {asset_name}"))
}

/// Look up `asset_name` in the release identified by `release_ref`
/// (`tags/vX.Y.Z` or `latest`) via the GitHub API. `Ok(None)` when that release
/// doesn't exist or lacks the asset — so the caller can fall back — and `Err`
/// only on a transport/parse failure.
async fn release_asset(
    release_ref: &str,
    asset_name: &str,
) -> anyhow::Result<Option<ReleaseAsset>> {
    let repo = repo_slug().context("could not derive the repo from the repository URL")?;
    let api = format!("https://api.github.com/repos/{repo}/releases/{release_ref}");
    // No `-f`: a missing release answers 404 with a JSON body we detect below,
    // which we want to treat as "fall back", not as a curl error.
    let out = Command::new("curl")
        .args(["-sSL", "-H", "Accept: application/vnd.github+json", &api])
        .output()
        .await
        .context("failed to run curl (is it installed?)")?;
    if !out.status.success() {
        bail!(
            "release metadata request failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let meta: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("bad release metadata payload")?;
    // A missing release is `{"message": "Not Found", ...}` — no `tag_name`.
    let Some(tag) = meta.get("tag_name").and_then(serde_json::Value::as_str) else {
        return Ok(None);
    };
    let Some(asset) = meta
        .get("assets")
        .and_then(serde_json::Value::as_array)
        .and_then(|assets| {
            assets
                .iter()
                .find(|a| a.get("name").and_then(serde_json::Value::as_str) == Some(asset_name))
        })
    else {
        return Ok(None);
    };
    let url = asset
        .get("browser_download_url")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{asset_name} in {tag} has no download url"))?;
    let sha256 = asset
        .get("digest")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("release {tag} has no checksum for {asset_name}"))?
        .trim_start_matches("sha256:")
        .to_string();
    Ok(Some(ReleaseAsset {
        version: tag.trim_start_matches('v').to_string(),
        url: url.to_string(),
        sha256,
    }))
}

/// Where auto-fetched daemon binaries are cached: `~/.chimaera/dist/cache/`,
/// keyed by target triple *and* version so an app upgrade fetches a fresh
/// daemon instead of redeploying a stale cached one. Kept separate from the
/// `just dist` stash in [`dist_dir`], which a developer owns and overrides
/// with.
fn download_cache_path(triple: &str, version: &str) -> PathBuf {
    dist_dir()
        .join("cache")
        .join(format!("chimaera-{triple}-{version}"))
}

/// Fetch the daemon binary matching `(os, arch)` from GitHub releases (see
/// [`resolve_release_asset`] for version-vs-latest selection), caching it under
/// [`download_cache_path`] keyed by the resolved version. This is the end-user
/// auto-install path — the app ships no repo and no `just dist` stash.
///
/// Downloads with the system `curl` (kept dependency-free, like every other
/// ssh/scp/curl shell-out here) and verifies the bytes against the release's
/// published sha256 before trusting them — we're about to run this on the
/// user's login-node account.
async fn fetch_release_binary(
    os: &str,
    arch: &str,
    progress: &impl Fn(Phase),
) -> anyhow::Result<PathBuf> {
    let triple = release_triple(os, arch)
        .ok_or_else(|| anyhow::anyhow!("no prebuilt daemon is published for {os}/{arch}"))?;
    let asset = resolve_release_asset(triple).await?;
    let cached = download_cache_path(triple, &asset.version);
    if cached.is_file() {
        tracing::info!("using cached daemon {}", cached.display());
        return Ok(cached);
    }
    progress(Phase::Downloading {
        target: triple.to_string(),
    });

    let tmp = cached.with_extension("part");
    if let Some(parent) = cached.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    tracing::info!("downloading daemon {} from {}", asset.version, asset.url);
    let out = Command::new("curl")
        .args(["-fSL", "--retry", "2", "-o"])
        .arg(&tmp)
        .arg(&asset.url)
        .output()
        .await
        .context("failed to run curl (is it installed?)")?;
    if !out.status.success() {
        std::fs::remove_file(&tmp).ok();
        bail!(
            "could not download {}: {}",
            asset.url,
            String::from_utf8_lossy(&out.stderr).trim(),
        );
    }

    let got = sha256_file(&tmp).await?;
    if !got.eq_ignore_ascii_case(&asset.sha256) {
        std::fs::remove_file(&tmp).ok();
        bail!(
            "checksum mismatch on the downloaded daemon (expected {}, got {got})",
            asset.sha256,
        );
    }

    let mut perms = std::fs::metadata(&tmp)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&tmp, perms)?;
    std::fs::rename(&tmp, &cached)
        .with_context(|| format!("failed to finalize {}", cached.display()))?;
    tracing::info!("cached daemon at {}", cached.display());
    Ok(cached)
}

/// `owner/repo` from [`chimaera_core::REPOSITORY`] (`https://github.com/owner/repo`).
fn repo_slug() -> Option<String> {
    chimaera_core::REPOSITORY
        .trim_end_matches('/')
        .strip_prefix("https://github.com/")
        .map(str::to_string)
}

/// Hex sha256 of `file`, via `sha256sum` (linux) or `shasum -a 256` (macOS).
async fn sha256_file(file: &Path) -> anyhow::Result<String> {
    for (bin, args) in [("sha256sum", &[][..]), ("shasum", &["-a", "256"][..])] {
        let out = Command::new(bin).args(args).arg(file).output().await;
        if let Ok(out) = out {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                if let Some(hex) = text.split_whitespace().next() {
                    return Ok(hex.to_string());
                }
            }
        }
    }
    bail!("could not compute a sha256 (neither sha256sum nor shasum is available)")
}

/// Resolve the LOCAL binary to deploy to a host of the target inferred from
/// `host`: an explicit `binary`, else a developer's `just dist` stash, else
/// auto-fetched from our release. Touches only the local machine (bar a
/// read-only `uname` over the shared connection) — callers resolve this
/// *before* stopping a running daemon, so a failed fetch never strands a host
/// with no daemon.
async fn resolve_local_binary(
    host: &str,
    binary: Option<&Path>,
    progress: &impl Fn(Phase),
) -> anyhow::Result<PathBuf> {
    if let Some(p) = binary {
        if !p.is_file() {
            bail!("binary {} does not exist", p.display());
        }
        return Ok(p.to_path_buf());
    }
    let (os, arch) = remote_target(host).await?;
    // Prefer a developer's local `just dist` stash if present, so a repo
    // checkout still overrides with its own build; otherwise auto-fetch the
    // matching daemon from our release (the end-user path — no repo, no stash).
    let candidate = dist_dir().join(dist_name(&os, &arch));
    if candidate.is_file() {
        return Ok(candidate);
    }
    fetch_release_binary(&os, &arch, progress)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "chimaera is not installed on {host} ({os}/{arch}) and could not be \
                 fetched automatically: {e}.\n\
                 Provide one with either:\n\
                 \x20 just dist                 (in the chimaera repo: builds musl \
                 binaries into ~/.chimaera/dist)\n\
                 \x20 chimaera connect {host} --binary /path/to/chimaera-built-for-{host}"
            )
        })
}

/// Copy `path` to `$HOME/.chimaera/bin/chimaera` on the host, staged +
/// renamed so an interrupted copy never leaves a half-written executable and
/// the old inode stays intact for anything still running it.
async fn deploy_binary(host: &str, path: &Path, progress: &impl Fn(Phase)) -> anyhow::Result<()> {
    progress(Phase::Installing {
        binary: path.to_path_buf(),
    });
    tracing::info!("installing {} on {host}", path.display());
    ssh_run(host, "mkdir -p $HOME/.chimaera/bin").await?;
    let output = scp_cmd()
        .arg(path)
        .arg(format!("{host}:.chimaera/bin/chimaera.new"))
        .output()
        .await
        .context("failed to run scp")?;
    if !output.status.success() {
        bail!(
            "scp to {host} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    ssh_run(
        host,
        "chmod +x $HOME/.chimaera/bin/chimaera.new && \
         mv -f $HOME/.chimaera/bin/chimaera.new $HOME/.chimaera/bin/chimaera",
    )
    .await?;
    Ok(())
}

/// Ensure the host has a chimaera binary, installing one only if absent (the
/// fresh-host path — an existing binary is left as-is). Replacing an outdated
/// one is the caller's job: resolve + deploy around a graceful stop, in that
/// order, so a failed fetch never kills a working daemon.
async fn ensure_remote_binary(
    host: &str,
    binary: Option<&Path>,
    progress: &impl Fn(Phase),
) -> anyhow::Result<()> {
    if ssh_check(host, "test -x $HOME/.chimaera/bin/chimaera").await? {
        return Ok(());
    }
    let path = resolve_local_binary(host, binary, progress).await?;
    deploy_binary(host, &path, progress).await
}

/// Start the daemon on the host and poll until its manifest reports alive.
async fn start_remote(host: &str) -> anyhow::Result<Manifest> {
    tracing::info!("starting chimaera daemon on {host}");
    ssh_run(
        host,
        // `;` not `&&` before setsid: with `&&`, the trailing `&` backgrounds the whole
        // list and the daemon runs as the foreground child of a subshell whose
        // stdout/stderr are the ssh channel — sshd then never closes the session and
        // `connect` hangs forever. Found the hard way on a real cluster.
        "mkdir -p $HOME/.chimaera/logs; \
         setsid nohup $HOME/.chimaera/bin/chimaera serve \
         >> $HOME/.chimaera/logs/serve.log 2>&1 < /dev/null & disown",
    )
    .await?;
    for _ in 0..15 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Some(m) = remote_manifest(host).await? {
            if remote_alive(host, m.pid).await? {
                return Ok(m);
            }
        }
    }
    bail!("daemon on {host} did not start within 15s (check ~/.chimaera/logs/serve.log there)");
}

/// Choose the local tunnel port: explicit flag, else the remote port if
/// locally free, else an OS-assigned free port.
fn pick_local_port(requested: Option<u16>, remote_port: u16) -> anyhow::Result<u16> {
    if let Some(port) = requested {
        return Ok(port);
    }
    if std::net::TcpListener::bind(("127.0.0.1", remote_port)).is_ok() {
        return Ok(remote_port);
    }
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .context("failed to find a free local port")?;
    Ok(listener.local_addr()?.port())
}

fn spawn_tunnel(host: &str, local: u16, remote: u16) -> anyhow::Result<Child> {
    ssh_base()
        // Exit non-zero the instant the local bind fails instead of sitting
        // idle: a reconnect that reuses a not-quite-released port then fails
        // in <1s (caught by wait_for_port's early-exit branch) rather than
        // eating the full 15s timeout before the fresh-port retry.
        .args(["-o", "ExitOnForwardFailure=yes"])
        .arg("-N")
        .arg("-L")
        .arg(format!("{local}:127.0.0.1:{remote}"))
        .arg(host)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| TunnelPhaseError(format!("failed to spawn ssh tunnel: {e}")).into())
}

/// Poll the local tunnel port until it accepts connections (15s timeout).
/// Returns true if the forward was delegated to an ssh ControlMaster: the mux
/// client registers the forward with the master and exits 0 immediately, so a
/// zero exit here is success, not failure.
async fn wait_for_port(port: u16, tunnel: &mut Child) -> anyhow::Result<bool> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut mux_delegated = false;
    loop {
        if !mux_delegated {
            if let Some(status) = tunnel.try_wait()? {
                if status.success() {
                    mux_delegated = true;
                } else {
                    return Err(
                        TunnelPhaseError(format!("ssh tunnel exited early: {status}")).into(),
                    );
                }
            }
        }
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return Ok(mux_delegated);
        }
        if tokio::time::Instant::now() > deadline {
            return Err(TunnelPhaseError(format!(
                "tunnel did not come up on 127.0.0.1:{port} within 15s"
            ))
            .into());
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// Run a remote command, treating its exit status as a boolean.
async fn ssh_check(host: &str, cmd: &str) -> anyhow::Result<bool> {
    let output = ssh_cmd(host)
        .arg(cmd)
        .output()
        .await
        .context("failed to run ssh")?;
    Ok(output.status.success())
}

/// Run a remote command, failing loudly if it does not exit 0.
async fn ssh_run(host: &str, cmd: &str) -> anyhow::Result<()> {
    let output = ssh_cmd(host)
        .arg(cmd)
        .output()
        .await
        .context("failed to run ssh")?;
    if !output.status.success() {
        bail!(
            "ssh {host} {cmd:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every ssh/scp call must carry the ControlMaster trio (so one auth
    /// covers the whole session; socket path uses ssh's short `%C` token),
    /// the trust-on-first-use host-key policy that lets a fresh install reach
    /// a never-seen host with no tty, and the liveness bounds that keep a
    /// dead link (laptop sleep) from leaving zombie masters and forwards.
    #[test]
    fn ssh_opts_multiplex_and_accept_new_hosts() {
        let opts = ssh_opts();
        assert_eq!(opts[0], "-o");
        assert_eq!(opts[1], "ControlMaster=auto");
        assert_eq!(opts[2], "-o");
        assert!(opts[3].starts_with("ControlPath="), "{}", opts[3]);
        assert!(opts[3].ends_with("/cm/%C"), "{}", opts[3]);
        assert_eq!(opts[4], "-o");
        assert_eq!(opts[5], "ControlPersist=10m");
        assert_eq!(opts[6], "-o");
        assert_eq!(opts[7], "StrictHostKeyChecking=accept-new");
        assert_eq!(opts[8], "-o");
        assert_eq!(opts[9], "ConnectTimeout=15");
        assert_eq!(opts[10], "-o");
        assert_eq!(opts[11], "ServerAliveInterval=15");
        assert_eq!(opts[12], "-o");
        assert_eq!(opts[13], "ServerAliveCountMax=3");
    }

    /// The fresh-port retry in the app keys off this downcast; if the tunnel
    /// errors stop carrying the marker, every failure would re-run the whole
    /// connect (and re-prompt 2FA).
    #[test]
    fn tunnel_phase_errors_downcast_through_anyhow() {
        let err: anyhow::Error = TunnelPhaseError("bind clash".into()).into();
        assert!(err.downcast_ref::<TunnelPhaseError>().is_some());
        assert_eq!(format!("{err}"), "bind clash");
        let other = anyhow::anyhow!("auth failed");
        assert!(other.downcast_ref::<TunnelPhaseError>().is_none());
    }

    /// The whole point of the HTTP probe: a listener that accepts but never
    /// answers (a dead ssh forward after laptop sleep looks exactly like
    /// this) is DOWN, while anything that answers HTTP is up. A bare TCP
    /// connect can't tell them apart — that regression made reconnect a
    /// silent no-op.
    #[tokio::test]
    async fn http_alive_requires_a_response_not_just_an_accept() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Accepts and holds the socket open, never writing: down.
        let silent = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let silent_port = silent.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((s, _)) = silent.accept().await {
                held.push(s); // keep it open so this mimics a live-but-dead forward
            }
        });
        assert!(
            !http_alive(silent_port).await,
            "accept-only listener must read as down"
        );

        // Answers any bytes with an HTTP status line: up (even a 401 proves
        // the daemon end-to-end).
        let http = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let http_port = http.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = http.accept().await {
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf).await;
                let _ = s
                    .write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n")
                    .await;
            }
        });
        assert!(
            http_alive(http_port).await,
            "an HTTP answer must read as up"
        );

        // Nothing listening at all: down.
        let free = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let free_port = free.local_addr().unwrap().port();
        drop(free);
        assert!(
            !http_alive(free_port).await,
            "closed port must read as down"
        );
    }

    #[test]
    fn dist_names_map_targets() {
        assert_eq!(dist_name("linux", "x86_64"), "chimaera-x86_64-linux-musl");
        assert_eq!(dist_name("linux", "aarch64"), "chimaera-aarch64-linux-musl");
        assert_eq!(dist_name("darwin", "arm64"), "chimaera-arm64-darwin");
    }

    /// The auto-fetch path must map detected targets to the exact asset names
    /// the release workflow publishes, and skip targets we don't build.
    #[test]
    fn release_triples_match_published_assets() {
        assert_eq!(
            release_triple("linux", "x86_64"),
            Some("x86_64-unknown-linux-musl")
        );
        assert_eq!(
            release_triple("linux", "aarch64"),
            Some("aarch64-unknown-linux-musl")
        );
        // Some clusters' uname reports arm64 for 64-bit ARM.
        assert_eq!(
            release_triple("linux", "arm64"),
            Some("aarch64-unknown-linux-musl")
        );
        assert_eq!(
            release_triple("darwin", "arm64"),
            Some("aarch64-apple-darwin")
        );
        // Not published → no guess.
        assert_eq!(release_triple("darwin", "x86_64"), None);
        assert_eq!(release_triple("windows", "x86_64"), None);
    }

    #[test]
    fn repo_slug_drives_the_release_api() {
        assert_eq!(
            repo_slug().as_deref(),
            Some("martinappberg/chimaera"),
            "owner/repo feeds the api.github.com releases url"
        );
    }

    /// The download cache is keyed by version so an app upgrade never
    /// redeploys a stale cached daemon (which would loop the update check).
    #[test]
    fn download_cache_is_versioned() {
        let a = download_cache_path("x86_64-unknown-linux-musl", "0.1.1");
        let b = download_cache_path("x86_64-unknown-linux-musl", "0.1.2");
        assert_ne!(a, b);
        assert!(a.ends_with("chimaera-x86_64-unknown-linux-musl-0.1.1"));
        assert!(a.starts_with(dist_dir()));
    }

    #[test]
    fn local_port_prefers_remote_port_when_free() {
        // Bind a port to force the fallback path.
        let holder = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let held = holder.local_addr().unwrap().port();
        let picked = pick_local_port(None, held).unwrap();
        assert_ne!(picked, held, "held port must not be picked");
        assert_eq!(pick_local_port(Some(4321), held).unwrap(), 4321);
    }

    /// The full reuse/update/attach-outdated policy, with session counts a
    /// caller would have fetched (or failed to fetch — `None` = busy).
    #[test]
    fn update_decision_matrix() {
        const OURS: &str = "ff52221.100";
        // Same source (timestamps differ across targets): reuse.
        assert_eq!(
            update_decision(OURS, Some("ff52221.999"), None, false),
            Decision::Reuse
        );
        assert_eq!(
            update_decision(OURS, Some(OURS), Some(3), false),
            Decision::Reuse
        );
        // Different build + provably idle: safe to replace.
        assert_eq!(
            update_decision(OURS, Some("d4e587f.50"), Some(0), false),
            Decision::Update
        );
        // Missing build id = ancient: same rules as a mismatch.
        assert_eq!(
            update_decision(OURS, None, Some(0), false),
            Decision::Update
        );
        // Live sessions, or a count we couldn't get: never silently kill.
        assert_eq!(
            update_decision(OURS, Some("d4e587f.50"), Some(2), false),
            Decision::ConnectOutdated
        );
        assert_eq!(
            update_decision(OURS, Some("d4e587f.50"), None, false),
            Decision::ConnectOutdated
        );
        assert_eq!(
            update_decision(OURS, None, None, false),
            Decision::ConnectOutdated
        );
        // Force (--update-daemon) replaces regardless of sessions or build.
        assert_eq!(
            update_decision(OURS, Some("d4e587f.50"), Some(7), true),
            Decision::Update
        );
        assert_eq!(update_decision(OURS, None, None, true), Decision::Update);
        assert_eq!(
            update_decision(OURS, Some(OURS), Some(0), true),
            Decision::Update
        );
    }

    /// Session counting only trusts the expected shape and only counts
    /// `alive: true` (the list also carries finished sessions).
    #[test]
    fn count_alive_sessions_parses_payloads() {
        assert_eq!(count_alive_sessions("[]"), Some(0));
        assert_eq!(
            count_alive_sessions(
                r#"[
                    {"id": "a", "alive": true},
                    {"id": "b", "alive": false},
                    {"id": "c", "alive": true},
                    {"id": "d"}
                ]"#
            ),
            Some(2)
        );
        // Not the sessions payload => unknown, never zero.
        assert_eq!(count_alive_sessions(""), None);
        assert_eq!(count_alive_sessions("unauthorized"), None);
        assert_eq!(count_alive_sessions(r#"{"error": "no"}"#), None);
    }
}
