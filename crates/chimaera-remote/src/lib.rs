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

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context};
use chimaera_core::{same_node, Manifest};
use tokio::process::{Child, Command};

/// Per-child context inherited by the native app's SSH_ASKPASS helper. This
/// must be set on every ssh/scp process individually: multiple hosts may
/// connect concurrently, so a process-global "current host" would race and
/// show one host's authentication prompt in another host's windows.
pub const ASKPASS_ALIAS_ENV: &str = "CHIMAERA_ASKPASS_ALIAS";

/// The ssh ControlMaster socket path pattern for chimaera connections. `%C`
/// is ssh's own hash of (localhost, remotehost, port, user): unique per
/// destination and short. The parent dir is created on demand (ssh will not
/// create it for the socket).
///
/// The WHOLE expanded socket path must stay under the ~104-byte unix-socket
/// (`sun_path`) limit. `data_dir()/cm/%C` clears it comfortably for the normal
/// `~/.chimaera` home, but an isolated `CHIMAERA_HOME` under a deep worktree
/// path (the dev app) overshoots even though `%C` keeps the leaf short — ssh
/// then fails every call with "ControlPath too long". So when the preferred
/// path wouldn't fit, anchor the socket in a short `/tmp` dir keyed by a hash of
/// the home: short enough for `sun_path`, yet still distinct per home so a dev
/// app's master never collides with the real app's. A normal home is
/// unaffected — it keeps `data_dir()/cm/%C`.
fn control_path() -> String {
    if let Some(t) = wsl_transport() {
        return wsl_control_path(&t.home);
    }
    let dir = control_dir(&chimaera_core::data_dir());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("failed to create ssh control dir {}: {e}", dir.display());
    }
    dir.join("%C").to_string_lossy().into_owned()
}

/// On Windows every ssh/scp runs INSIDE the WSL2 distro that hosts the local
/// daemon: `connect`'s whole architecture rides on ControlMaster, which
/// Win32-OpenSSH does not implement — Linux OpenSSH in the distro does. The
/// shell wires this once the distro is known; `None` (always, on unix) means
/// spawn the system ssh directly.
#[derive(Clone, Debug)]
pub struct WslTransport {
    /// The distro whose ssh (and daemon) we ride.
    pub distro: String,
    /// The distro user every spawn pins with `-u` (the default user can
    /// change under us — Ubuntu's OOBE flips it on first interactive run).
    pub user: String,
    /// The distro-side `$HOME`, anchoring the ControlMaster sockets — a
    /// socket ssh creates inside the distro cannot live on a Windows path.
    pub home: String,
}

static WSL_TRANSPORT: std::sync::RwLock<Option<WslTransport>> = std::sync::RwLock::new(None);

pub fn set_wsl_transport(t: Option<WslTransport>) {
    *WSL_TRANSPORT.write().unwrap_or_else(|p| p.into_inner()) = t;
}

fn wsl_transport() -> Option<WslTransport> {
    WSL_TRANSPORT
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

/// Whether the WSL transport is wired and remote connects can work at all
/// on this host — the shell refuses a connect when wiring failed rather
/// than silently spawning Win32-OpenSSH (which lacks ControlMaster).
pub fn wsl_transport_ready() -> bool {
    wsl_transport().is_some()
}

/// The ssh/scp process builder, transport-aware. `--exec` bypasses the
/// distro's login shell; env the ssh children need (`SSH_ASKPASS` et al)
/// crosses the boundary via `WSLENV`, which the shell exports. The user is
/// pinned with `-u` so a later change of the distro's default user (Ubuntu
/// OOBE) can never silently re-home ssh's config/keys/sockets mid-flight.
fn transport_command(program: &str) -> Command {
    match wsl_transport() {
        Some(t) => {
            let mut c = Command::new("wsl.exe");
            c.args(["-d", &t.distro, "-u", &t.user, "--exec", program]);
            // wsl.exe's own diagnostics are UTF-16LE without this; every
            // caller decodes stderr as UTF-8 (legacy inbox WSL ignores the
            // var — its errors stay mojibake, acceptably rare).
            c.env("WSL_UTF8", "1");
            #[cfg(windows)]
            c.creation_flags(chimaera_core::CREATE_NO_WINDOW);
            c
        }
        None => Command::new(program),
    }
}

/// A `curl` invocation with the same no-console discipline as every other
/// child this GUI-adjacent crate spawns on Windows.
fn curl_command() -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new("curl");
    #[cfg(windows)]
    c.creation_flags(chimaera_core::CREATE_NO_WINDOW);
    c
}

/// Per-child output ceilings. Every caller expects small control-plane JSON,
/// version text, or diagnostics; downloads write to a file. Keeping the caps
/// here prevents a noisy/malicious endpoint from exhausting the GUI process
/// before the wall-clock timeout fires.
const CHILD_STDOUT_MAX_BYTES: usize = 8 * 1024 * 1024;
const CHILD_STDERR_MAX_BYTES: usize = 1024 * 1024;

async fn read_bounded<R>(reader: R, cap: usize, stream: &str) -> anyhow::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::with_capacity(cap.min(64 * 1024));
    reader
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .with_context(|| format!("failed to read child {stream}"))?;
    if bytes.len() > cap {
        bail!("child {stream} exceeded the {cap}-byte limit");
    }
    Ok(bytes)
}

/// Collect an already-spawned child's piped output with byte and time bounds.
/// On overflow/timeout the child is killed and reaped; dropping a read future
/// closes its pipe, so a producer cannot remain wedged behind backpressure.
async fn collect_child_bounded(
    mut child: Child,
    secs: u64,
    what: &str,
) -> anyhow::Result<std::process::Output> {
    let stdout = child.stdout.take().context("child stdout was not piped")?;
    let stderr = child.stderr.take().context("child stderr was not piped")?;
    let collect = async {
        let (stdout, stderr) = tokio::try_join!(
            read_bounded(stdout, CHILD_STDOUT_MAX_BYTES, "stdout"),
            read_bounded(stderr, CHILD_STDERR_MAX_BYTES, "stderr"),
        )?;
        let status = child.wait().await.context("failed to wait for child")?;
        Ok::<_, anyhow::Error>(std::process::Output {
            status,
            stdout,
            stderr,
        })
    };

    match tokio::time::timeout(Duration::from_secs(secs), collect).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(err)) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(err).with_context(|| format!("failed to collect {what}"))
        }
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            bail!("{what} timed out after {secs}s")
        }
    }
}

/// Collect a command's output with hard byte and time bounds. Under the WSL
/// transport a wedged wsl.exe would otherwise hang forever — ssh's own
/// `ConnectTimeout` only bounds the TCP connect *inside* the distro.
async fn output_bounded(
    cmd: &mut Command,
    secs: u64,
    what: &str,
) -> anyhow::Result<std::process::Output> {
    cmd.kill_on_drop(true);
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = cmd
        .spawn()
        .with_context(|| format!("failed to run {what}"))?;
    collect_child_bounded(child, secs, what).await
}

/// ssh runs inside the distro, so its ControlMaster socket must live on a
/// distro-side path (the shell `mkdir -p`s the dir when wiring the
/// transport). Deliberately the same `~/.chimaera/cm` an in-distro `connect`
/// would use: the app and the distro share one authenticated master per
/// host. Pure so the shape is testable without touching the global.
fn wsl_control_path(home: &str) -> String {
    format!("{home}/.chimaera/cm/%C")
}

/// How a local file path is spelled for a process inside WSL (`C:\x\y` →
/// `/mnt/c/x/y`) — scp's "local" side runs in the distro under the transport.
pub fn local_path_for_scp(path: &std::path::Path) -> String {
    if wsl_transport().is_none() {
        return path.to_string_lossy().into_owned();
    }
    windows_path_as_wsl(&path.to_string_lossy())
}

/// How a Windows path is spelled for a process inside WSL: drive-letter
/// absolutes (including `\\?\`-verbatim ones, which `current_exe`/
/// canonicalize can produce) become `/mnt/<drive>/…`; UNC and other shapes
/// pass through untouched — callers must treat an un-translated result as
/// "not reachable from the distro", not silently use it. Shared by scp's
/// local side AND the shell's askpass-wrapper exe path, so the two can't
/// drift.
pub fn windows_path_as_wsl(s: &str) -> String {
    let s = s.strip_prefix(r"\\?\").unwrap_or(s).replace('\\', "/");
    if s.len() >= 3 && s.as_bytes()[1] == b':' && s.as_bytes()[2] == b'/' {
        format!("/mnt/{}{}", s[..1].to_ascii_lowercase(), &s[2..])
    } else {
        s
    }
}

/// The ControlMaster socket DIRECTORY for a given state dir: `<data_dir>/cm`
/// normally, or a short `/tmp/chimaera-<home-hash>/cm` when that would push the
/// expanded socket path past the `sun_path` limit (a deep isolated
/// `CHIMAERA_HOME`). Pure so the length invariant can be tested without env.
fn control_dir(data_dir: &std::path::Path) -> std::path::PathBuf {
    /// `%C` expands to a 40-hex-char hash. OpenSSH appends a temporary
    /// `.XXXXXXXXXXXX...` suffix while creating the mux listener, and macOS
    /// applies the same `sun_path` limit to that transient path.
    const C_LEAF_WITH_OPENSSH_TMP_SUFFIX: usize = 40 + 1 + 16;
    /// Headroom under the ~104-byte `sun_path` cap.
    const SUN_PATH_SAFE: usize = 100;

    let preferred = data_dir.join("cm");
    if preferred.as_os_str().len() + 1 + C_LEAF_WITH_OPENSSH_TMP_SUFFIX <= SUN_PATH_SAFE {
        preferred
    } else {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        data_dir.hash(&mut h);
        // Keep the `/cm` tail so the socket shape (`…/cm/%C`) is stable per home.
        std::path::PathBuf::from(format!("/tmp/chimaera-{:08x}", h.finish() as u32)).join("cm")
    }
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
fn ssh_opts() -> [String; 16] {
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
        // Compression: negotiated once at master creation and inherited by
        // every mux channel. Terminal streams and escape-sequence snapshots
        // compress enormously; on the WAN links this product targets (HPC
        // login nodes) that is bandwidth the tunnel doesn't spend, and the
        // CPU cost measured negligible (~2% of one core encrypting a
        // 75 Mbit/s flood, before compression shrinks it). Applies only to
        // chimaera's own masters — the user's ssh config is untouched.
        "-o".into(),
        "Compression=yes".into(),
    ]
}

/// An `ssh` command pre-loaded with the shared options, no host yet. For
/// flag-heavy invocations where the destination must come last
/// (`-O cancel -L …`, `-N -L …`); otherwise prefer [`ssh_cmd`]. Follows
/// `host`'s current [`Route`].
fn ssh_base(host: &str) -> Command {
    ssh_base_via(host, &route_of(host))
}

/// [`ssh_base`] along an explicit route (a tunnel tearing down the forward it
/// registered, a wedge check of one particular master).
fn ssh_base_via(host: &str, route: &Route) -> Command {
    let mut c = transport_command("ssh");
    c.env(ASKPASS_ALIAS_ENV, host);
    c.args(route_opts(host, route));
    c.args(ssh_opts());
    c
}

/// An `ssh` command pre-loaded with the shared ControlMaster options and
/// pointed at `host`, ready for the remote command to be appended. The common
/// shape (`ssh <opts> host <cmd>`); every plain remote command goes through
/// here so they all share one authenticated connection.
fn ssh_cmd(host: &str) -> Command {
    let mut c = ssh_base(host);
    c.arg(host);
    c
}

/// An `scp` command pre-loaded with the shared options, so a binary copy
/// reuses the connection the probe already authenticated instead of prompting
/// again.
fn scp_cmd(host: &str) -> Command {
    let mut c = transport_command("scp");
    c.env(ASKPASS_ALIAS_ENV, host);
    c.args(route_opts(host, &route_of(host)));
    c.args(ssh_opts());
    c
}

// --- Round-robin login nodes --------------------------------------------------
//
// An HPC alias often names a POOL of login nodes (one DNS name, rotated per
// lookup), and every login node mounts the same `$HOME`. The daemon runs on
// ONE of them: its manifest on the shared home is visible from every node, but
// its pid and loopback port mean something only on the node that wrote it. A
// ControlMaster keeps every command on the node it landed on — until a new
// master dials (after sleep, a `ControlPersist` expiry, an app relaunch) and
// lands on another node, where `kill -0 <pid>` answers for an unrelated process
// table. So the probe reports which node it ran on, a manifest written
// elsewhere is never judged from the wrong node, and every later ssh call for
// the alias is routed to the daemon's node.

/// Which node an ssh/scp call for a host alias lands on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Route {
    /// Wherever the alias's own ssh config sends a new connection.
    #[default]
    Alias,
    /// One named node, dialed with the alias's own config and only its
    /// `HostName` replaced: user, keys, ProxyJump and 2FA carry over, and the
    /// node gets its own ControlMaster (`%C` hashes the host name). One
    /// authentication, no dependency on another connection — but the name is
    /// resolved and dialed from THIS machine.
    Node(String),
    /// One named node reached through the alias's own ControlMaster (a `-W`
    /// first leg): the name is resolved and dialed from inside the cluster,
    /// for names this machine can't resolve or login nodes it can't reach.
    NodeViaAlias(String),
}

impl Route {
    /// The node this route pins, if any.
    pub fn node(&self) -> Option<&str> {
        match self {
            Route::Alias => None,
            Route::Node(node) | Route::NodeViaAlias(node) => Some(node),
        }
    }
}

/// The learned route per alias, process-wide: a connect records where the
/// daemon lives and every later call for that alias — the tunnel, stops,
/// session counts, compute tunnels, wedge checks — follows it, the way they
/// all share one ControlMaster. Absent = [`Route::Alias`].
static ROUTES: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, Route>>> =
    std::sync::LazyLock::new(Default::default);

/// The route every ssh/scp call for `host` currently takes.
pub fn route_of(host: &str) -> Route {
    ROUTES
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(host)
        .cloned()
        .unwrap_or_default()
}

fn set_route(host: &str, route: Route) {
    let mut routes = ROUTES.lock().unwrap_or_else(|p| p.into_inner());
    match route {
        Route::Alias => routes.remove(host),
        route => routes.insert(host.to_string(), route),
    };
}

/// The `-o` options that send a call for `host` along `route`. Placed before
/// [`ssh_opts`] and the destination: OpenSSH keeps the first value it sees,
/// so these override the alias's own `HostName` (and, for the via-alias
/// route, its `ProxyJump`/`ProxyCommand`) from `~/.ssh/config`.
fn route_opts(host: &str, route: &Route) -> Vec<String> {
    match route {
        Route::Alias => Vec::new(),
        Route::Node(node) => vec!["-o".into(), format!("HostName={node}")],
        Route::NodeViaAlias(node) => vec![
            "-o".into(),
            format!("HostName={node}"),
            "-o".into(),
            format!("ProxyCommand={}", master_proxy_command(host, None)),
        ],
    }
}

/// Whether a node name from a manifest is safe to put in ssh's argv and in a
/// `ProxyCommand` (whose `%h` a local shell expands): letters, digits, `-`,
/// `_`, `.`, no leading `-`/`.`. The manifest is data on a remote disk, never
/// trusted syntax.
fn valid_node_name(node: &str) -> bool {
    !node.is_empty()
        && node.len() <= 253
        && !node.starts_with(['-', '.'])
        && node
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The routes to try, in order, for reaching `node`. A dotted name is the
/// cluster's own DNS name — dial it directly first (one prompt, no second
/// connection to keep up). A bare name would resolve through THIS machine's
/// search domains, which can reach an unrelated host (and send it the
/// password the prompt asks for), so it only ever travels inside the cluster.
fn routes_to(node: &str) -> Vec<Route> {
    if node.contains('.') {
        vec![
            Route::Node(node.to_string()),
            Route::NodeViaAlias(node.to_string()),
        ]
    } else {
        vec![Route::NodeViaAlias(node.to_string())]
    }
}

/// Where the connect flow currently is; consumers surface these however
/// fits (tracing lines in the CLI, progress events in the shell).
#[derive(Clone, Debug)]
pub enum Phase {
    /// Probing the host for a running daemon.
    Probing,
    /// The daemon runs on another node of the alias's login-node pool than
    /// the one this connection landed on; reaching `node` (which may ask the
    /// user to authenticate to it).
    Routing { node: String },
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

/// Which per-user state root on the HOST a connect targets. Every remote
/// side effect — the manifest probed, the binary installed, the daemon
/// started (and that daemon's own state, via `CHIMAERA_HOME`), the
/// reuse/update decision — derives from this one value, so the two roots are
/// fully disjoint: a dev daemon runs NEXT TO the real one and a dev connect
/// can never stop, replace, or even read the real daemon.
///
/// `Real` is the shared `~/.chimaera`, where the home IS the data dir
/// (manifest at `~/.chimaera/manifest.json`). `Dev` runs the daemon under
/// `CHIMAERA_HOME=~/.chimaera-dev`, which relocates its data dir to
/// `<home>/data` — hence the asymmetric manifest/log paths. The dev home
/// stays short and `$HOME`-anchored deliberately: the remote daemon's own
/// runtime dir (`<home>/run`) is bound by the same ~104-byte `sun_path`
/// limit as our local sockets (see [`control_dir`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RemoteHome {
    /// The end-user daemon at `~/.chimaera` (release binaries).
    #[default]
    Real,
    /// The isolated dev daemon at `~/.chimaera-dev` (locally built binaries).
    Dev,
}

impl RemoteHome {
    /// Which home THIS build targets — the build's property, never a
    /// per-host or per-connect choice: a dev build (the unstamped `0.0.1`
    /// sentinel) always talks to `~/.chimaera-dev` on both ends, a release
    /// always to `~/.chimaera`. No toggle exists, so a dev tunnel can never
    /// heal into the real daemon (or vice versa) across reconnects.
    pub fn current() -> Self {
        if chimaera_core::is_dev_build() {
            RemoteHome::Dev
        } else {
            RemoteHome::Real
        }
    }

    /// The state root as a `$HOME`-anchored fragment for remote shell
    /// commands (expanded by the remote shell, never locally).
    fn dir(self) -> &'static str {
        match self {
            RemoteHome::Real => "$HOME/.chimaera",
            RemoteHome::Dev => "$HOME/.chimaera-dev",
        }
    }

    /// The daemon's manifest path. Real: the home is the data dir. Dev:
    /// `CHIMAERA_HOME` relocates data one level down, to `<home>/data`.
    fn manifest_path(self) -> String {
        match self {
            RemoteHome::Real => format!("{}/manifest.json", self.dir()),
            RemoteHome::Dev => format!("{}/data/manifest.json", self.dir()),
        }
    }

    /// The directory the started daemon's stdout/stderr log lives in (same
    /// data-dir split as [`Self::manifest_path`]).
    fn log_dir(self) -> String {
        match self {
            RemoteHome::Real => format!("{}/logs", self.dir()),
            RemoteHome::Dev => format!("{}/data/logs", self.dir()),
        }
    }

    fn log_path(self) -> String {
        format!("{}/serve.log", self.log_dir())
    }

    fn bin_dir(self) -> String {
        format!("{}/bin", self.dir())
    }

    fn bin_path(self) -> String {
        format!("{}/chimaera", self.bin_dir())
    }

    /// The staged-upload scp destination, relative to the remote `$HOME`
    /// (scp has no remote shell to expand `$HOME` in).
    fn scp_staged_bin(self) -> &'static str {
        match self {
            RemoteHome::Real => ".chimaera/bin/chimaera.new",
            RemoteHome::Dev => ".chimaera-dev/bin/chimaera.new",
        }
    }

    /// The env assignment that scopes the started daemon (and all the state
    /// it writes) to this home — empty for the real home. An ENV PREFIX, not
    /// a flag: `chimaera serve` is a load-bearing CLI string and must stay
    /// exactly that.
    fn serve_env(self) -> &'static str {
        match self {
            RemoteHome::Real => "",
            RemoteHome::Dev => "CHIMAERA_HOME=$HOME/.chimaera-dev ",
        }
    }
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
    /// How the forward reaches the daemon's node: [`Route::Alias`] unless the
    /// alias is a login-node pool and the daemon runs on a node other than
    /// the one a new connection lands on.
    pub route: Route,
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
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
        cancel_master_forward(
            &self.host,
            &self.route,
            &format!("{}:127.0.0.1:{}", self.local_port, self.manifest.port),
        )
        .await;
    }
}

/// Best-effort `ssh -O cancel` of a `-L` forward the host's ControlMaster
/// holds. A forward registered by a mux client belongs to the MASTER, not
/// the client — killing (or outliving) the client leaves the local listener
/// bound until the master expires, so every path that abandons such a
/// forward must cancel it by the exact spec it was opened with, on the
/// master (the `route`) it was registered with.
async fn cancel_master_forward(host: &str, route: &Route, spec: &str) {
    if bounded_mux_ssh(host, route, &["-O", "cancel", "-L", spec], &[], 10)
        .await
        .is_none()
    {
        tracing::warn!("ssh -O cancel -L {spec} to {host} did not finish within 10s");
    }
}

/// Detect and `-O exit` a ControlMaster to `host` whose TCP link is dead:
/// after laptop sleep the master PROCESS usually survives its connection,
/// and until `ServerAliveInterval × CountMax` (~45 s) expires every mux
/// client — the reconnect's probe included — queues on it. Returns whether
/// a master was terminated.
///
/// `-O check` / `-O exit` are answered by the master's own local event loop
/// (no packet crosses the network), so they are safe on a dead link; only
/// the `host true` session open traverses it, which is what makes it the
/// test — and the only step that may declare a wedge: an unanswered check
/// is "could not tell", not a verdict. The 15 s bound is one
/// `ServerAliveInterval` and deliberately not tighter: a merely LOADED
/// login node (an NFS-backed module init under sshd; load 20 on 64 cores
/// seen live) takes seconds to run `true`, and that same load is what
/// confirms a tunnel down — a false positive here kills a healthy master,
/// forcing the Duo re-auth the master exists to avoid and dropping that
/// alias's compute forwards.
/// `session_bound_secs` is how long the session-open test may take before
/// the master is declared wedged: 15 s when the health monitor already
/// confirmed the tunnel down (that same node load produced the misses), but
/// longer when there is no verdict at all — a launch-time restore or a click
/// on a host whose warm master merely sits on a loaded node must not lose
/// its master (and its Duo session) to a slow `true`.
///
/// A host routed to its daemon's node ([`Route`]) has two masters — the
/// node's, and the alias's own (the via-alias route's first leg, or the one
/// the connect first landed through) — and both are checked, the node's first
/// so its `-W` leg is gone before the master it rides.
pub async fn clear_wedged_master(host: &str, session_bound_secs: u64) -> bool {
    let route = route_of(host);
    let mut cleared = clear_wedged_master_via(host, &route, session_bound_secs).await;
    if route != Route::Alias {
        cleared |= clear_wedged_master_via(host, &Route::Alias, session_bound_secs).await;
    }
    cleared
}

async fn clear_wedged_master_via(host: &str, route: &Route, session_bound_secs: u64) -> bool {
    match bounded_mux_ssh(host, route, &["-O", "check"], &[], 10).await {
        // No master (or ssh cannot even run): nothing to clear.
        Some(false) => return false,
        Some(true) => {}
        None => {
            tracing::warn!(
                "could not tell whether the ControlMaster to {host} is wedged: `-O check` did \
                 not answer within 10s; leaving it alone"
            );
            return false;
        }
    }
    // A master that answers `-O check` but cannot open a session is broken
    // either way — a stall (dead link) or a fast failure ("read from master
    // failed" from one mid-teardown) — and the flight would otherwise dial
    // it under 240 s bounds.
    if bounded_mux_ssh(host, route, &[], &["true"], session_bound_secs).await == Some(true) {
        return false;
    }
    match bounded_mux_ssh(host, route, &["-O", "exit"], &[], 5).await {
        Some(_) => tracing::info!(
            "ControlMaster to {host} was wedged after a link loss; terminated so the reconnect dials fresh"
        ),
        None => tracing::warn!(
            "ControlMaster to {host} was wedged after a link loss and did not answer `-O exit` \
             within 5s; the reconnect may have to wait for ssh's own keepalive timeout"
        ),
    }
    true
}

/// A non-interactive ssh at `host`'s ControlMaster along `route`: `BatchMode`
/// (no prompt, ever) and a tight `ConnectTimeout` placed BEFORE [`ssh_opts`] —
/// OpenSSH takes the first value it sees for most options, so one appended
/// after the shared set would look effective and be silently ignored. Every
/// mux control request and teardown probe starts here.
fn mux_prologue(host: &str, route: &Route) -> Command {
    let mut command = transport_command("ssh");
    command
        .env(ASKPASS_ALIAS_ENV, host)
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=5"])
        .args(route_opts(host, route))
        .args(ssh_opts());
    command
}

/// `ssh <prologue> <opts> host <remote>` under a hard wall-clock bound:
/// `Some(exit-success)` when it finished, `None` when it was still running
/// at the deadline (then killed). A spawn failure counts as `Some(false)`.
async fn bounded_mux_ssh(
    host: &str,
    route: &Route,
    opts: &[&str],
    remote: &[&str],
    secs: u64,
) -> Option<bool> {
    let label = format!("ssh {} {host} {}", opts.join(" "), remote.join(" "));
    let what = label.trim();
    let mut command = mux_prologue(host, route);
    command
        .args(opts)
        .arg(host)
        .args(remote)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            tracing::debug!("could not spawn {what}: {err}");
            return Some(false);
        }
    };
    // The outer deadline decides "stalled" and drops the child (killed via
    // kill_on_drop); the inner bound only guards the pipe reads.
    match tokio::time::timeout(
        Duration::from_secs(secs),
        collect_child_bounded(child, secs + 5, what),
    )
    .await
    {
        Ok(Ok(output)) => Some(output.status.success()),
        Ok(Err(_)) => Some(false),
        Err(_) => None,
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

/// [`http_alive`] with identity: a bearer-authed request must come back 200.
/// Liveness alone is not enough on a multi-hop tunnel — a relay port on a
/// shared login node can be squatted by a stale relay or a foreign process,
/// and "something answered HTTP" would bless the wrong endpoint (found live:
/// a health probe passed through a previous connect's leaked relay while the
/// new tunnel's own forward was already dying). Only the intended daemon
/// holding this endpoint's token can answer 200.
pub async fn http_alive_authed(port: u16, token: &str) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let attempt = async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .ok()?;
        stream
            .write_all(
                format!(
                    "GET /api/v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
                     Authorization: Bearer {token}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .ok()?;
        let mut buf = Vec::with_capacity(16);
        while buf.len() < 12 {
            let mut chunk = [0u8; 16];
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        // "HTTP/1.1 200" — status position is fixed by the HTTP/1.x grammar.
        (buf.starts_with(b"HTTP/") && buf.get(9..12) == Some(b"200")).then_some(())
    };
    tokio::time::timeout(Duration::from_secs(2), attempt)
        .await
        .ok()
        .flatten()
        .is_some()
}

/// The side-effecting host operations the [`connect`] decision phase drives,
/// behind a trait so [`resolve_daemon`]'s policy is unit-testable with a fake
/// (this crate can't be live-verified — no remote host in CI). The production
/// impl ([`SshOps`]) delegates each method VERBATIM to the free function of the
/// same name, so the seam can never drift from real behavior. The three
/// binary/deploy methods take `progress` as `&impl Fn(Phase)` (NOT `&dyn`): a
/// bare `dyn Fn` erases the closure's auto-traits, which would make the whole
/// `connect` future `!Send` and break the Tauri app's `spawn` of it — keeping
/// the concrete closure type lets `Send` flow through exactly as it did before
/// this seam existed.
///
/// Every op runs along `host`'s current [`Route`]; [`locate`] is the only
/// place that changes it.
trait RemoteOps {
    fn route(&self, host: &str) -> Route;
    fn set_route(&self, host: &str, route: Route);
    async fn remote_probe(&self, host: &str) -> anyhow::Result<ProbeRun>;
    async fn remote_sessions_count(
        &self,
        host: &str,
        manifest: &Manifest,
    ) -> anyhow::Result<Option<usize>>;
    async fn resolve_local_binary(
        &self,
        host: &str,
        binary: Option<&Path>,
        progress: &impl Fn(Phase),
    ) -> anyhow::Result<PathBuf>;
    async fn stop_remote(&self, host: &str, pid: u32) -> anyhow::Result<()>;
    async fn deploy_binary(
        &self,
        host: &str,
        path: &Path,
        progress: &impl Fn(Phase),
    ) -> anyhow::Result<()>;
    async fn start_remote(&self, host: &str) -> anyhow::Result<Manifest>;
    async fn ensure_remote_binary(
        &self,
        host: &str,
        binary: Option<&Path>,
        progress: &impl Fn(Phase),
    ) -> anyhow::Result<()>;
}

/// The production [`RemoteOps`]: every method is a one-line delegation to the
/// existing free function, so behavior is preserved by construction. Carries
/// the [`RemoteHome`] so the whole decision phase is scoped to one root — the
/// policy in [`resolve_daemon`] never needs to know which.
struct SshOps {
    home: RemoteHome,
}

impl RemoteOps for SshOps {
    fn route(&self, host: &str) -> Route {
        route_of(host)
    }
    fn set_route(&self, host: &str, route: Route) {
        set_route(host, route)
    }
    async fn remote_probe(&self, host: &str) -> anyhow::Result<ProbeRun> {
        probe_run(host, self.home).await
    }
    async fn remote_sessions_count(
        &self,
        host: &str,
        manifest: &Manifest,
    ) -> anyhow::Result<Option<usize>> {
        remote_sessions_count(host, manifest).await
    }
    async fn resolve_local_binary(
        &self,
        host: &str,
        binary: Option<&Path>,
        progress: &impl Fn(Phase),
    ) -> anyhow::Result<PathBuf> {
        resolve_local_binary(host, binary, self.home, &progress).await
    }
    async fn stop_remote(&self, host: &str, pid: u32) -> anyhow::Result<()> {
        stop_remote(host, pid).await
    }
    async fn deploy_binary(
        &self,
        host: &str,
        path: &Path,
        progress: &impl Fn(Phase),
    ) -> anyhow::Result<()> {
        deploy_binary(host, path, self.home, &progress).await
    }
    async fn start_remote(&self, host: &str) -> anyhow::Result<Manifest> {
        start_remote(host, self.home).await
    }
    async fn ensure_remote_binary(
        &self,
        host: &str,
        binary: Option<&Path>,
        progress: &impl Fn(Phase),
    ) -> anyhow::Result<()> {
        ensure_remote_binary(host, binary, self.home, &progress).await
    }
}

/// Find `host`'s daemon and leave `host`'s route pointing at the node it runs
/// on — or at the node a fresh start belongs on. `Ok(None)` = nothing to
/// attach to (no manifest, or its node provably gone); `Ok(Some((manifest,
/// alive)))` carries a verdict taken ON the manifest's node, so `alive ==
/// false` means provably dead there. A manifest another node wrote whose node
/// can't be reached is an error — never "dead": starting a second daemon over
/// the same shared state would resume every session next to the running one.
async fn locate(
    ops: &impl RemoteOps,
    host: &str,
    progress: &impl Fn(Phase),
) -> anyhow::Result<Option<(Manifest, bool)>> {
    // A route an earlier connect learned goes first: it lands straight on the
    // daemon's node (one dial, one prompt) instead of wherever the pool sends
    // a new master. Anything but a verdict from that node starts over.
    let learned = ops.route(host);
    if learned != Route::Alias {
        let why = match ops.remote_probe(host).await {
            // Stopped since (a graceful stop removes the manifest): a fresh
            // start there keeps the alias on the node it was routed to.
            Ok(ProbeRun::Ran(None)) => return Ok(None),
            Ok(ProbeRun::Ran(Some(p))) if p.here() => return Ok(Some((p.manifest, p.alive))),
            Ok(ProbeRun::Ran(Some(p))) => format!("registered on {} now", p.manifest.hostname),
            Ok(ProbeRun::Failed(f)) => f.to_string(),
            Err(e) => format!("{e:#}"),
        };
        tracing::info!(
            "{host}: no verdict from {} ({why}); probing afresh",
            learned.node().unwrap_or_default()
        );
        ops.set_route(host, Route::Alias);
    }

    let landed = match ops.remote_probe(host).await? {
        // An unreachable host reads as nothing running, as it always has:
        // the start path then surfaces ssh's own error.
        ProbeRun::Failed(_) | ProbeRun::Ran(None) => return Ok(None),
        ProbeRun::Ran(Some(p)) if p.here() => return Ok(Some((p.manifest, p.alive))),
        ProbeRun::Ran(Some(p)) => p,
    };
    let node = landed.manifest.hostname.clone();
    if landed.manifest_node_resolves == Some(false) {
        tracing::warn!(
            "{host}: the daemon registered on {node} (pid {}) is gone with its node — the name no \
             longer resolves on {}; starting a fresh daemon there",
            landed.manifest.pid,
            landed.node
        );
        return Ok(None);
    }
    if !valid_node_name(&node) {
        bail!(unreachable_node_error(
            host,
            &landed,
            "its name is not a plain host name"
        ));
    }
    tracing::info!(
        "{host}: this connection landed on {} but the daemon is registered on {node}; routing there",
        landed.node
    );
    progress(Phase::Routing { node: node.clone() });
    let mut why = String::new();
    for route in routes_to(&node) {
        ops.set_route(host, route.clone());
        match ops.remote_probe(host).await {
            // The verdict now comes from the node that wrote the manifest.
            Ok(ProbeRun::Ran(Some(p))) if p.here() && same_node(&p.node, &node) => {
                tracing::info!("{host}: reached {node} ({route:?})");
                return Ok(Some((p.manifest, p.alive)));
            }
            // The name leads back to the node we landed on: a renamed host,
            // so the first probe's verdict was local after all.
            Ok(ProbeRun::Ran(Some(p))) if same_node(&p.node, &landed.node) => {
                ops.set_route(host, Route::Alias);
                return Ok(Some((landed.manifest, landed.alive)));
            }
            // Gone between the probes — that node's daemon just stopped.
            Ok(ProbeRun::Ran(None)) => return Ok(None),
            Ok(ProbeRun::Ran(Some(p))) => {
                why = if same_node(&p.node, &node) {
                    format!(
                        "the manifest changed while connecting (now {})",
                        p.manifest.hostname
                    )
                } else {
                    format!("dialing {node} reached a machine calling itself {}", p.node)
                };
            }
            Ok(ProbeRun::Failed(f)) => {
                let network = f.network_level();
                why = f.to_string();
                // An auth failure or a cancelled prompt must not raise a
                // second prompt along another route.
                if !network {
                    break;
                }
            }
            Err(e) => {
                why = format!("{e:#}");
                break;
            }
        }
    }
    ops.set_route(host, Route::Alias);
    bail!(unreachable_node_error(host, &landed, &why))
}

/// The honest failure for a daemon registered on a node this connect could
/// not reach: what is where, why nothing was started, and the way out.
fn unreachable_node_error(host: &str, landed: &Probe, why: &str) -> String {
    let m = &landed.manifest;
    format!(
        "{host}'s daemon runs on login node {} (pid {}), but this connection landed on {} and \
         could not reach {}: {why}. Nothing was started: a second daemon would resume the same \
         sessions next to the running one. Reconnect once {} is reachable — or, if that node is \
         gone for good, remove the daemon's manifest.json on {host} and reconnect.",
        m.hostname, m.pid, landed.node, m.hostname, m.hostname
    )
}

/// The DECISION phase of [`connect`]: probe the host's daemon and decide
/// whether to reuse, replace, attach-outdated, or fresh-start it — returning
/// `(manifest, outdated, live_sessions)` for the tunnel-attach phase to forward
/// against. Split out behind [`RemoteOps`] so the policy is exercisable without
/// ssh, including the resolve-binary-BEFORE-stop ordering in the Update arm
/// (the past bug: a failed download once stranded a stopped daemon).
async fn resolve_daemon(
    ops: &impl RemoteOps,
    host: &str,
    opts: &ConnectOpts,
    progress: &impl Fn(Phase),
) -> anyhow::Result<(Manifest, bool, Option<usize>)> {
    progress(Phase::Probing);
    let local_build = chimaera_core::BUILD_ID;
    let mut outdated = false;
    let mut live_sessions = None;
    // One remote exec answers "is there a manifest", "is its pid alive", and
    // "was it written on this node" (`probe_run`): every ssh exec through the
    // ControlMaster costs a channel-open RTT plus a fork on a loaded login
    // node. `locate` adds execs only for a manifest another node wrote, and
    // leaves every op below routed to the daemon's node.
    let manifest = match locate(ops, host, progress).await? {
        Some((m, true)) => {
            // Only pay for the session-count round trip when it can change
            // the decision (build mismatch, or an explicit update request).
            let sessions = if opts.update_daemon
                || !chimaera_core::builds_match(local_build, m.build.as_deref())
            {
                ops.remote_sessions_count(host, &m).await?
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
                    let bin = ops
                        .resolve_local_binary(host, opts.binary.as_deref(), progress)
                        .await?;
                    ops.stop_remote(host, m.pid).await?;
                    ops.deploy_binary(host, &bin, progress).await?;
                    progress(Phase::Starting);
                    let started = ops.start_remote(host).await?;
                    // A stop whose exit went unconfirmed (a link blip during
                    // the wait) leaves the old daemon serving: the new one
                    // refuses to start beside it and the wait hands back the
                    // OLD manifest. That is a working daemon, so connect to
                    // it — but as outdated, never as a completed update.
                    if started.pid == m.pid
                        || !chimaera_core::builds_match(local_build, started.build.as_deref())
                    {
                        tracing::warn!(
                            "daemon on {host} was not replaced — the previous build (pid {}) is still serving; connecting to it as outdated",
                            started.pid
                        );
                        outdated = true;
                        live_sessions = sessions;
                    }
                    started
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
            ops.ensure_remote_binary(host, opts.binary.as_deref(), progress)
                .await?;
            progress(Phase::Starting);
            ops.start_remote(host).await?
        }
    };
    Ok((manifest, outdated, live_sessions))
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
    // Dev-ness is the build's property (see `RemoteHome::current`): a dev
    // build ALWAYS targets `~/.chimaera-dev`, a release always `~/.chimaera`.
    // There is no per-connect override, so a release client can never touch
    // a dev home and a dev client can never stop or replace the real daemon.
    let ops = SshOps {
        home: RemoteHome::current(),
    };
    let (manifest, outdated, live_sessions) = resolve_daemon(&ops, host, &opts, &progress).await?;
    // Where `locate` left the alias: the forward must end on the daemon's
    // node, the only one whose loopback it listens on.
    let route = route_of(host);

    let mut local_port = pick_local_port(opts.local_port, manifest.port)?;
    progress(Phase::Tunneling { local_port });
    let tunnel = open_proven_tunnel(host, local_port, manifest.port, &manifest.token).await;
    let (child, mux_delegated) = match tunnel {
        Ok(tunnel) => tunnel,
        // The availability probe binds on OUR side, but under the WSL
        // transport ssh's -L binds inside the distro — a different port
        // namespace — so the first pick can collide with an in-distro
        // listener the probe cannot see. A stale mux forward can also accept
        // locally without reaching this daemon. One retry on a fresh
        // OS-assigned port covers both; an EXPLICITLY requested port stays a
        // hard error.
        Err(e) if opts.local_port.is_none() => {
            tracing::warn!("tunnel on 127.0.0.1:{local_port} failed ({e:#}); retrying fresh");
            local_port = ephemeral_port()?;
            progress(Phase::Tunneling { local_port });
            open_proven_tunnel(host, local_port, manifest.port, &manifest.token).await?
        }
        Err(e) => return Err(e),
    };
    match route.node() {
        Some(node) => tracing::info!(
            "tunnel up: 127.0.0.1:{local_port} -> {host} (login node {node}):{}",
            manifest.port
        ),
        None => tracing::info!(
            "tunnel up: 127.0.0.1:{local_port} -> {host}:{}",
            manifest.port
        ),
    }

    Ok(Tunnel {
        host: host.to_string(),
        local_port,
        remote_build: manifest.build.clone(),
        manifest,
        mux_delegated,
        outdated,
        live_sessions,
        route,
        child,
    })
}

/// Find `host`'s daemon under `home` and route every later ssh call for
/// `host` to the node it runs on — what a caller that talks to the daemon
/// over ssh (curl against its loopback port) must do first. `Ok(None)` =
/// nothing registered; otherwise the manifest and a liveness verdict taken on
/// its own node. May ask to authenticate to that node. The route is keyed by
/// `host` exactly as given, so later calls must pass the same string.
pub async fn locate_daemon(
    host: &str,
    home: RemoteHome,
) -> anyhow::Result<Option<(Manifest, bool)>> {
    locate(&SshOps { home }, host, &|_| {}).await
}

/// Frame the manifest in [`remote_probe`] / [`start_remote`] output on BOTH
/// sides: an echoing `~/.bashrc` under non-interactive ssh (common on HPC)
/// puts noise before AND after a script's output, so the client parses
/// strictly between these. Never substrings of serde JSON.
const MANIFEST_BEGIN: &str = "---chimaera-manifest-begin---";
const MANIFEST_END: &str = "---chimaera-manifest-end---";

/// POSIX-sh fragment printing the pid recorded in the manifest at `$f` (a
/// serde-pretty JSON file; `sed` is on every remote, `jq` is not). It takes
/// the FIRST `"pid"` line of pretty output and the LAST on a compact line,
/// so `chimaera_core::Manifest` must stay FLAT — a nested pid would win.
const SH_MANIFEST_PID: &str =
    r#"sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$f" | head -n 1"#;

/// POSIX-sh predicate: read the manifest's pid into `$p` and `kill -0` it.
/// Shared by the probe and the start-wait so the two can never disagree on
/// what "alive" means (a fn, not a const: it interpolates the sed above).
fn sh_manifest_alive() -> String {
    format!("p=$({SH_MANIFEST_PID}); [ -n \"$p\" ] && kill -0 \"$p\" 2>/dev/null")
}

/// Exit code of a remote wait loop whose host has no usable `sleep`
/// (neither `sleep 0.5` nor `sleep 1`). Without it the loop would spin
/// through its tick budget in milliseconds — measured at 24 ms — and
/// fabricate a "did not start" / "still running" verdict.
const NO_SLEEP_EXIT: i32 = 5;

/// Probe ONCE whether `sleep` accepts fractions (GNU and BSD do; a minimal
/// busybox may not): `$frac` = 0 means half-second ticks, otherwise whole
/// seconds counted double so a tick-bounded loop keeps its deadline. The
/// probe's own half second is the loop's first wait, hence `i=1`.
const SH_SLEEP_PROBE: &str = "sleep 0.5 2>/dev/null; frac=$?; i=0; [ \"$frac\" -eq 0 ] && i=1;";

/// One tick of a remote wait loop after [`SH_SLEEP_PROBE`]: half a second,
/// or a whole second counted twice; a `sleep` that fails outright exits
/// [`NO_SLEEP_EXIT`] instead of spinning.
fn sh_tick() -> String {
    format!(
        "if [ \"$frac\" -eq 0 ]; then sleep 0.5 || exit {NO_SLEEP_EXIT}; i=$((i+1)); \
         else sleep 1 2>/dev/null || exit {NO_SLEEP_EXIT}; i=$((i+2)); fi;"
    )
}

/// Wrap a POSIX-sh script for `ssh host <cmd>`: sshd hands the command to
/// the user's LOGIN shell, which on HPC accounts may be tcsh or fish, so the
/// script rides inside `sh -c '…'`. Two escapes make the single-quoted body
/// read identically under sh, bash, zsh, dash, fish, and csh: an inner `'`
/// becomes `'\''`, and — csh expands `!` even inside single quotes — an
/// inner `!` becomes `'\!'` (the backslash sits outside the quotes, where
/// every shell honors it and the POSIX ones drop it). Quotes first: the
/// bang escape introduces quotes of its own.
fn sh_wrap(script: &str) -> String {
    let body = script.replace('\'', r"'\''").replace('!', r"'\!'");
    format!("sh -c '{body}'")
}

/// POSIX-sh fragment printing the node name recorded in the manifest at `$f`
/// — the same flat-JSON `sed` discipline as [`SH_MANIFEST_PID`].
const SH_MANIFEST_HOSTNAME: &str =
    r#"sed -n 's/.*"hostname"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$f" | head -n 1"#;

/// POSIX-sh fragment setting `$d` to whether node name `$h` exists in the
/// name service of the node the script runs on: `found`, `gone` (the resolver
/// answered "no such name", `EAI_NONAME`), or `unknown` (no perl, or no clear
/// answer). Not `getent`: it exits the same for "no such name" and "the DNS
/// server is down", and an outage must never read as "that node is gone".
/// The verdict rides stdout, not an exit code — perl dying at compile time
/// exits with whatever `errno` held.
const SH_NODE_RESOLVES: &str = r#"d=unknown; if command -v perl >/dev/null 2>&1; then r=$(perl -MSocket=:addrinfo -e 'my ($e) = getaddrinfo($ARGV[0], "22"); print STDOUT ($e ? ($e == EAI_NONAME() ? "gone" : "unknown") : "found")' "$h" 2>/dev/null); case "$r" in found|gone) d=$r;; esac; fi;"#;

/// What one probe exec found, seen from the node it ran on.
#[derive(Clone, Debug)]
pub struct Probe {
    pub manifest: Manifest,
    /// The node the probe ran on (`uname -n`); empty if the host didn't say.
    pub node: String,
    /// `kill -0` of the manifest's pid on `node` — the daemon's liveness only
    /// when [`Probe::here`]; anywhere else it tested an unrelated process
    /// table.
    pub alive: bool,
    /// For a manifest another node wrote: whether that node's name still
    /// resolves on `node`. `Some(false)` is the cluster's name service saying
    /// the node no longer exists; `None` = not asked, or no clear answer.
    pub manifest_node_resolves: Option<bool>,
}

impl Probe {
    /// Whether the manifest was written on the node the probe ran on — the
    /// only node where its pid and loopback port mean anything. A host that
    /// doesn't report its node name is taken at its word, as before nodes
    /// were compared.
    pub fn here(&self) -> bool {
        self.node.is_empty() || same_node(&self.node, &self.manifest.hostname)
    }
}

/// One probe exec over a host's current [`Route`].
#[derive(Debug)]
enum ProbeRun {
    /// The script ran; `None` = no readable manifest.
    Ran(Option<Probe>),
    /// ssh (or the remote shell) failed before the script could answer.
    Failed(ProbeFailure),
}

#[derive(Debug)]
struct ProbeFailure {
    /// The exit status, as ssh's caller would print it.
    status: String,
    stderr: String,
}

impl ProbeFailure {
    /// Whether the dial never reached an sshd that could have authenticated
    /// us — name resolution, TCP connect, or the banner exchange failed — the
    /// one case where another route to the same node is worth a try. An auth
    /// failure or a cancelled prompt is not: retrying would prompt again.
    /// OpenSSH's own client messages, stable across releases.
    fn network_level(&self) -> bool {
        [
            "Could not resolve hostname",
            "connect to host",
            "kex_exchange_identification",
            "banner exchange",
        ]
        .iter()
        .any(|marker| self.stderr.contains(marker))
    }
}

impl std::fmt::Display for ProbeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // ssh's own complaint is its last line; a chatty login banner
        // printed to stderr sits before it.
        match self.stderr.lines().rev().find(|l| !l.trim().is_empty()) {
            Some(line) => write!(f, "{}", line.trim()),
            None => write!(f, "ssh exited {}", self.status),
        }
    }
}

/// Fetch the manifest under `home`, whether its recorded pid answers
/// `kill -0`, and which node answered, in ONE remote exec — the decision
/// phase's whole probe. A manifest + a `kill -0` exec cost two serial round
/// trips, and every exec through the ControlMaster is a channel-open RTT plus
/// a remote fork (~300-500 ms on a loaded login node at WAN latency). `None`
/// = no readable manifest (or ssh itself failed — "nothing running", as the
/// connect flow has always read an unreachable host).
pub async fn remote_probe(host: &str, home: RemoteHome) -> anyhow::Result<Option<Probe>> {
    match probe_run(host, home).await? {
        ProbeRun::Ran(probe) => Ok(probe),
        ProbeRun::Failed(_) => Ok(None),
    }
}

async fn probe_run(host: &str, home: RemoteHome) -> anyhow::Result<ProbeRun> {
    let cmd = sh_wrap(&probe_script(&home.manifest_path()));
    // SSH_ONESHOT_SECS, not shorter: the first call to a host raises the
    // ControlMaster and may sit in an askpass password/Duo prompt.
    let output = output_bounded(ssh_cmd(host).arg(cmd), SSH_ONESHOT_SECS, "ssh").await?;
    if !output.status.success() {
        return Ok(ProbeRun::Failed(ProbeFailure {
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }));
    }
    let probe = parse_probe_output(&String::from_utf8_lossy(&output.stdout))
        .with_context(|| format!("probing the daemon on {host}"))?;
    Ok(ProbeRun::Ran(probe))
}

/// The POSIX-sh script [`remote_probe`] runs on the host: the manifest at
/// `manifest_path` (if readable) framed between [`MANIFEST_BEGIN`] and
/// [`MANIFEST_END`], then a trailer: the pid it tested, the node it ran on,
/// the node the manifest names and — only when those two differ — whether
/// that name still resolves here, and an `alive`/`dead` verdict. `$HOME` in
/// the path is expanded by the remote shell (as an assignment RHS it is never
/// word-split). A missing or unreadable manifest prints nothing and exits 0.
/// Pure so the tests can run it under real shells.
fn probe_script(manifest_path: &str) -> String {
    format!(
        "f={manifest_path}; [ -r \"$f\" ] || exit 0; if {alive}; then a=alive; else a=dead; fi; \
         n=$(uname -n 2>/dev/null); h=$({hostname}); d=same; \
         if [ \"$h\" != \"$n\" ]; then {resolves} fi; \
         printf '\\n{begin}\\n'; cat \"$f\"; \
         printf '\\n{end}\\npid=%s\\nnode=%s\\nhost=%s\\ndns=%s\\n%s\\n' \"$p\" \"$n\" \"$h\" \"$d\" \"$a\"",
        alive = sh_manifest_alive(),
        hostname = SH_MANIFEST_HOSTNAME,
        resolves = SH_NODE_RESOLVES,
        begin = MANIFEST_BEGIN,
        end = MANIFEST_END,
    )
}

/// The manifest JSON framed between the markers in a remote script's
/// stdout, plus the trailer the script printed after the end marker.
/// `Ok(None)` = no begin marker (the script printed nothing: no manifest);
/// a begin without an end is a cut-off transcript → `Err`.
fn framed_manifest(stdout: &str) -> anyhow::Result<Option<(&str, &str)>> {
    let Some((_, rest)) = stdout.split_once(MANIFEST_BEGIN) else {
        return Ok(None);
    };
    let (json, trailer) = rest
        .split_once(MANIFEST_END)
        .context("remote output was cut off before the manifest end marker")?;
    Ok(Some((json.trim(), trailer)))
}

/// The `pid=<n>` trailer line: the pid the remote `sed` extracted and
/// tested. Cross-checked against the parsed manifest so a silent sed/serde
/// disagreement is an error, never "dead" — which would start a second
/// daemon next to a live one.
fn trailer_pid(trailer: &str) -> Option<u32> {
    trailer
        .lines()
        .find_map(|l| l.trim().strip_prefix("pid="))
        .and_then(|p| p.trim().parse().ok())
}

/// A `key=value` trailer line's value.
fn trailer_field<'a>(trailer: &'a str, key: &str) -> Option<&'a str> {
    trailer
        .lines()
        .find_map(|l| l.trim().strip_prefix(key)?.strip_prefix('='))
        .map(str::trim)
}

fn trailer_verdict(trailer: &str) -> Option<bool> {
    trailer.lines().find_map(|l| match l.trim() {
        "alive" => Some(true),
        "dead" => Some(false),
        _ => None,
    })
}

fn check_trailer_pid(manifest: &Manifest, trailer: &str, what: &str) -> anyhow::Result<()> {
    match trailer_pid(trailer) {
        Some(p) if p == manifest.pid => Ok(()),
        p => bail!(
            "{what}: the remote tested pid {} but the manifest says {}",
            p.map_or("<none>".to_string(), |p| p.to_string()),
            manifest.pid
        ),
    }
}

/// [`remote_probe`]'s stdout → a [`Probe`]. No framed manifest = `None`; an
/// unparsable one counts as none (a corrupt file: a fresh start) — but never
/// when the remote found a LIVE pid in it, or when the remote read it as
/// another node's record (whose daemon can't be judged from here); a pid
/// mismatch or a missing verdict is an `Err`. The trailer's node fields are
/// optional so a host that can't name itself degrades to the pre-node probe.
fn parse_probe_output(stdout: &str) -> anyhow::Result<Option<Probe>> {
    let Some((json, trailer)) = framed_manifest(stdout)? else {
        return Ok(None);
    };
    let node = trailer_field(trailer, "node")
        .unwrap_or_default()
        .to_string();
    let manifest = match serde_json::from_str::<Manifest>(json) {
        Ok(manifest) => manifest,
        Err(err) => {
            if trailer_pid(trailer).is_some() && trailer_verdict(trailer) == Some(true) {
                bail!("the manifest is unparsable ({err}) yet its pid is alive; refusing to start a second daemon");
            }
            let written_on = trailer_field(trailer, "host").unwrap_or_default();
            if !written_on.is_empty() && !node.is_empty() && !same_node(written_on, &node) {
                bail!("the manifest is unparsable ({err}) and names node {written_on}, not {node}; refusing to start a second daemon");
            }
            return Ok(None);
        }
    };
    check_trailer_pid(&manifest, trailer, "probe")?;
    let alive = trailer_verdict(trailer).context("probe output carried no alive/dead verdict")?;
    let manifest_node_resolves = match trailer_field(trailer, "dns") {
        Some("found") => Some(true),
        Some("gone") => Some(false),
        _ => None,
    };
    // The resolver was asked about the name the remote `sed` read: the same
    // cross-check as the pid, so a sed/serde disagreement can never turn into
    // "gone" for the wrong name — which would start a second daemon.
    if manifest_node_resolves.is_some() {
        let resolved = trailer_field(trailer, "host").unwrap_or_default();
        anyhow::ensure!(
            resolved == manifest.hostname,
            "probe: the remote resolved node {resolved:?} but the manifest names {:?}",
            manifest.hostname
        );
    }
    Ok(Some(Probe {
        manifest,
        node,
        alive,
        manifest_node_resolves,
    }))
}

/// [`start_remote`]'s wait output → the manifest of the daemon that came up.
fn parse_start_wait_output(stdout: &str) -> anyhow::Result<Manifest> {
    let (json, trailer) =
        framed_manifest(stdout)?.context("the wait exited 0 without printing a manifest")?;
    let manifest: Manifest = serde_json::from_str(json).context("unparsable manifest")?;
    check_trailer_pid(&manifest, trailer, "start wait")?;
    Ok(manifest)
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
    let mut command = ssh_cmd(host);
    command
        .arg(cmd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().context("failed to run ssh")?;
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let line = format!("header = \"Authorization: Bearer {}\"\n", manifest.token);
        stdin.write_all(line.as_bytes()).await.ok();
        // Dropping stdin sends EOF, which ends the config for curl.
    }
    let output = collect_child_bounded(child, SSH_ONESHOT_SECS, "ssh session count").await?;
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

/// Gracefully stop the daemon on `host`: SIGTERM, then ONE remote exec waits
/// up to ~10 s for it to go. Never escalates to SIGKILL — a daemon that will
/// not die may be holding sessions that must not be torn out from under
/// their owner, so that errors honestly. Three outcomes: gone (`Ok`), still
/// running after the deadline (`Err`), or the wait's ssh ended early (a
/// dropped link) — then one bounded `kill -0` re-check decides, and if even
/// that cannot run, `Ok` with a warning: this sits between "stop" and
/// "deploy" in the update path, where an `Err` would strand the host with a
/// stopped daemon and the old binary (deploy stages `.new` + `mv -f`; a
/// daemon that was in fact still running makes the restart's wait return
/// its manifest).
pub async fn stop_remote(host: &str, pid: u32) -> anyhow::Result<()> {
    tracing::info!("stopping daemon on {host} (pid {pid})");
    // The exit code is the wire: 0 = gone, STOP_STILL_ALIVE_EXIT,
    // STOP_SIGNAL_FAILED_EXIT (stderr says why), NO_SLEEP_EXIT; anything
    // else is ssh's own (255 on a dropped link) = unconfirmed.
    let cmd = sh_wrap(&stop_script(pid, STOP_WAIT_TICKS));
    let output = output_bounded(ssh_cmd(host).arg(cmd), SSH_ONESHOT_SECS, "ssh").await?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    match output.status.code() {
        Some(STOP_STILL_ALIVE_EXIT) => bail!(still_running_error(host, pid)),
        Some(STOP_SIGNAL_FAILED_EXIT) => {
            bail!("ssh {host} \"kill -TERM {pid}\" failed: {}", stderr.trim())
        }
        Some(NO_SLEEP_EXIT) => bail!(
            "{host} has no usable `sleep`; cannot wait for the daemon to stop: {}",
            stderr.trim()
        ),
        _ => {}
    }
    // SIGTERM was dispatched, then ssh itself ended (255: the link dropped
    // mid-wait). One non-interactive re-check: `kill -0` exits 1 once the
    // pid is gone.
    let mut check = mux_prologue(host, &route_of(host));
    check
        .arg(host)
        .arg(sh_wrap(&format!("kill -0 {pid} 2>/dev/null")));
    match output_bounded(&mut check, 30, "ssh kill -0").await {
        Ok(out) if out.status.success() => bail!(still_running_error(host, pid)),
        Ok(out) if out.status.code() == Some(1) => Ok(()),
        other => {
            let recheck = match other {
                Ok(out) => format!(
                    "{}: {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                Err(err) => format!("{err:#}"),
            };
            tracing::warn!(
                "stop sent to the daemon on {host} (pid {pid}) but its exit could not be \
                 confirmed (wait ended {}: {}; re-check: {recheck}) — proceeding as stopped",
                output.status,
                stderr.trim()
            );
            Ok(())
        }
    }
}

fn still_running_error(host: &str, pid: u32) -> String {
    format!(
        "daemon on {host} (pid {pid}) is still running 10s after SIGTERM — \
         refusing to kill -9 it; something is keeping it busy (open UI tabs \
         hold its sockets). Close them and retry, or stop it by hand."
    )
}

/// Remote exit codes of [`stop_remote`]'s one-shot script: small, and
/// distinct from what the shell (1, 2, 126, 127) or ssh itself (255) emits.
const STOP_SIGNAL_FAILED_EXIT: i32 = 3;
const STOP_STILL_ALIVE_EXIT: i32 = 4;
/// How many half-second ticks [`stop_remote`] waits after SIGTERM (10 s).
const STOP_WAIT_TICKS: u32 = 20;

/// The POSIX-sh script [`stop_remote`] runs: SIGTERM `pid`, then up to
/// `ticks` half-second waits for it to go. Pure so tests can run it under
/// real shells with a short deadline.
fn stop_script(pid: u32, ticks: u32) -> String {
    format!(
        "kill -TERM {pid} || exit {failed}; {probe} while :; do \
         kill -0 {pid} 2>/dev/null || exit 0; [ $i -lt {ticks} ] || exit {alive}; {tick} done",
        failed = STOP_SIGNAL_FAILED_EXIT,
        probe = SH_SLEEP_PROBE,
        alive = STOP_STILL_ALIVE_EXIT,
        tick = sh_tick(),
    )
}

/// `uname -sm` on the host, lowercased: e.g. `("linux", "x86_64")`.
pub async fn remote_target(host: &str) -> anyhow::Result<(String, String)> {
    let output = output_bounded(ssh_cmd(host).arg("uname -sm"), SSH_ONESHOT_SECS, "ssh").await?;
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
    let mut cmd = curl_command();
    cmd.args(["-sSL", "--max-time", "30"]).args([
        "-H",
        "Accept: application/vnd.github+json",
        &api,
    ]);
    // Unauthenticated api.github.com is rate-limited PER IP — CI runners
    // share pool IPs and hit it constantly. Ride a token when the
    // environment has one (GITHUB_TOKEN in workflows); end-user machines do
    // fine on the anonymous quota.
    if let Ok(token) = std::env::var("GITHUB_TOKEN").or_else(|_| std::env::var("GH_TOKEN")) {
        if !token.is_empty() {
            cmd.args(["-H", &format!("Authorization: Bearer {token}")]);
        }
    }
    let out = output_bounded(&mut cmd, 45, "curl (release metadata)")
        .await
        .context("is curl installed?")?;
    if !out.status.success() {
        bail!(
            "release metadata request failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let meta: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("bad release metadata payload")?;
    // A missing release is `{"message": "Not Found", ...}` — no `tag_name`.
    // Anything ELSE without a tag (rate limit, auth failure) is a transient
    // API error and must surface as one: falling back would misreport it as
    // "no release provides <asset>" and strand provisioning with a lie.
    let Some(tag) = meta.get("tag_name").and_then(serde_json::Value::as_str) else {
        let message = meta
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unrecognized payload");
        if message == "Not Found" {
            return Ok(None);
        }
        bail!("GitHub API refused the release lookup: {message}");
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
/// auto-install path — the app ships no repo and no `just dist` stash. Public
/// for the Windows shell, which provisions a WSL distro exactly the way
/// `connect` provisions a remote host: same assets, same cache, same verify.
///
/// Downloads with the system `curl` (kept dependency-free, like every other
/// ssh/scp/curl shell-out here) and verifies the bytes against the release's
/// published sha256 before trusting them — we're about to run this on the
/// user's login-node account.
pub async fn fetch_release_binary(
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
    // --max-time bounds the download so a stalled connection can't pin a
    // caller (the Windows shell provisions BEFORE its first window opens).
    let out = output_bounded(
        curl_command()
            .args(["-fSL", "--retry", "2", "--max-time", "300", "-o"])
            .arg(&tmp)
            .arg(&asset.url),
        330,
        "curl (daemon download)",
    )
    .await
    .context("is curl installed?")?;
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

    // The exec bit only matters where the cached binary runs in place (unix);
    // on Windows the cache is transfer-only — the mode is set at install time
    // inside the distro/host.
    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&tmp)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tmp, perms)?;
    }
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

/// Hex sha256 of `file`, computed in-process — a Windows host has neither
/// `sha256sum` nor `shasum`, and the verify step must never depend on
/// platform hash tools. Streamed in fixed chunks: this crate ships inside
/// the daemon binary run on RSS-policed login nodes, so the hash must never
/// buffer a whole (growing) release asset.
async fn sha256_file(file: &Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;
    let mut f = tokio::fs::File::open(file)
        .await
        .with_context(|| format!("failed to open {} for checksum", file.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Resolve the LOCAL binary to deploy to a host of the target inferred from
/// `host`: an explicit `binary`, else a developer's `just dist` stash, else
/// auto-fetched from our release. Touches only the local machine (bar a
/// read-only `uname` over the shared connection) — callers resolve this
/// *before* stopping a running daemon, so a failed fetch never strands a host
/// with no daemon.
///
/// A DEV connect deploys YOUR build, so its source policy differs twice:
/// no release fallback (a downloaded release masquerading as the dev daemon
/// would silently test the wrong code — fail loudly instead), and the real
/// `~/.chimaera/dist` stash is searched as well, because `just dist` writes
/// there while an isolated app's `dist_dir()` (under `CHIMAERA_HOME`) no
/// longer points at it.
async fn resolve_local_binary(
    host: &str,
    binary: Option<&Path>,
    home: RemoteHome,
    progress: &impl Fn(Phase),
) -> anyhow::Result<PathBuf> {
    if let Some(p) = binary {
        if !p.is_file() {
            bail!("binary {} does not exist", p.display());
        }
        return Ok(p.to_path_buf());
    }
    let (os, arch) = remote_target(host).await?;
    let name = dist_name(&os, &arch);
    if home == RemoteHome::Dev {
        // A dev connect deploys YOUR build: the (possibly isolated) dist dir
        // first, then the real `~/.chimaera/dist` stash `just dist` writes to.
        let candidate = dist_dir().join(&name);
        if candidate.is_file() {
            return Ok(candidate);
        }
        if let Some(stash) = dirs::home_dir().map(|h| h.join(".chimaera").join("dist").join(&name))
        {
            if stash.is_file() {
                return Ok(stash);
            }
        }
        bail!(
            "no locally built daemon for {host} ({os}/{arch}) — a dev connect \
             deploys YOUR build, never a release download.\n\
             Build one with either:\n\
             \x20 just dist                 (in the chimaera repo: builds musl \
             binaries into ~/.chimaera/dist)\n\
             \x20 chimaera connect {host} --binary /path/to/chimaera-built-for-{host}"
        );
    }
    // The REAL home runs release binaries ONLY — the `just dist` stash is
    // never a source here. A stash build carries the unstamped 0.0.1 sentinel,
    // and a dev binary started in the real home relocates its state to
    // `~/.chimaera-dev` (dev is dev, on both ends): the connect polling
    // `~/.chimaera/manifest.json` never sees it come up, and every retry
    // piles another daemon onto the host. Testing your own build against a
    // host is what the dev connect is for; `--binary` stays as the explicit
    // override.
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

/// Copy `path` to `<home>/bin/chimaera` on the host, staged + renamed so an
/// interrupted copy never leaves a half-written executable and the old inode
/// stays intact for anything still running it.
async fn deploy_binary(
    host: &str,
    path: &Path,
    home: RemoteHome,
    progress: &impl Fn(Phase),
) -> anyhow::Result<()> {
    progress(Phase::Installing {
        binary: path.to_path_buf(),
    });
    tracing::info!("installing {} on {host} ({})", path.display(), home.dir());
    ssh_run(host, &format!("mkdir -p {}", home.bin_dir())).await?;
    let output = output_bounded(
        scp_cmd(host)
            // Under the WSL transport scp itself runs in the distro, so the
            // "local" side must be spelled as a /mnt path.
            .arg(local_path_for_scp(path))
            .arg(format!("{host}:{}", home.scp_staged_bin())),
        600,
        "scp",
    )
    .await?;
    if !output.status.success() {
        bail!(
            "scp to {host} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    ssh_run(
        host,
        &format!(
            "chmod +x {bin}.new && mv -f {bin}.new {bin}",
            bin = home.bin_path()
        ),
    )
    .await?;
    Ok(())
}

/// Ensure the host has a chimaera binary, installing one only if absent (the
/// fresh-host path — an existing binary is left as-is). Replacing an outdated
/// one is the caller's job: resolve + deploy around a graceful stop, in that
/// order, so a failed fetch never kills a working daemon.
///
/// The DEV home always deploys, even over an existing binary: its whole point
/// is running THIS build, and with no dev daemon alive there is nothing a
/// redeploy could disturb — while a stale binary from last week would
/// otherwise silently start in place of the code under test.
async fn ensure_remote_binary(
    host: &str,
    binary: Option<&Path>,
    home: RemoteHome,
    progress: &impl Fn(Phase),
) -> anyhow::Result<()> {
    if home == RemoteHome::Real {
        // Reuse the existing binary only if it can actually SERVE this home:
        // executability is not enough. A dev (0.0.1-sentinel) binary stranded
        // in the real home — deployed there by a pre-fix release that trusted
        // the dist stash — starts fine but relocates its state to
        // `~/.chimaera-dev`, so the manifest this connect polls never
        // appears. Probe the version and replace anything that is not a
        // stamped release.
        match remote_binary_version(host, home).await? {
            Some(v) if !chimaera_core::version_is_dev(&v) => return Ok(()),
            Some(v) => tracing::info!(
                "replacing the dev build ({v}) stranded at {} — it cannot serve the real home",
                home.bin_path()
            ),
            None => {}
        }
    }
    let path = resolve_local_binary(host, binary, home, progress).await?;
    deploy_binary(host, &path, home, progress).await
}

/// The version the binary installed in `home` reports (`chimaera X.Y.Z` →
/// `X.Y.Z`), or `None` when it is missing, not executable, or prints
/// something unrecognizable — all "not usable, redeploy" to the caller.
async fn remote_binary_version(host: &str, home: RemoteHome) -> anyhow::Result<Option<String>> {
    let output = output_bounded(
        ssh_cmd(host).arg(sh_wrap(&format!(
            "{} --version 2>/dev/null",
            home.bin_path()
        ))),
        SSH_ONESHOT_SECS,
        "ssh",
    )
    .await?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(parse_cli_version(&String::from_utf8_lossy(&output.stdout)))
}

/// Parse clap's `--version` line (`chimaera X.Y.Z`) to `X.Y.Z`. The name
/// check guards against stray shell noise on stdout being read as a version.
fn parse_cli_version(text: &str) -> Option<String> {
    let mut words = text
        .lines()
        .find(|l| !l.trim().is_empty())?
        .split_whitespace();
    if words.next()? != "chimaera" {
        return None;
    }
    words.next().map(str::to_string)
}

/// Start the daemon on the host, then wait — in ONE remote exec — until its
/// manifest reports a live pid.
async fn start_remote(host: &str, home: RemoteHome) -> anyhow::Result<Manifest> {
    tracing::info!("starting chimaera daemon on {host} ({})", home.dir());
    ssh_run(
        host,
        // Detach the daemon so it outlives this one-shot ssh command. Primary
        // path: `serve --daemonize` forks + `setsid(2)`s IN-PROCESS, so it needs
        // no util-linux `setsid`/`nohup` (absent on macOS/BSD and minimal Linux
        // containers) — that is what lets `connect` start a daemon on ANY POSIX
        // remote. Its parent exits 0 the instant the child owns its new session,
        // so `&& exit` ends the ssh command promptly and `connect` moves on to
        // the wait exec below.
        //
        // Fallback, reached only when `--daemonize` is rejected (a pre-flag
        // remote binary — an older release, which by definition sits on a Linux
        // host already provisioned with one): the proven `setsid nohup … &
        // disown`. `;` not `&&` after `mkdir`: with `&&` the trailing `&`
        // backgrounds the whole list and the daemon becomes the foreground child
        // of a subshell still holding the ssh channel's stdio, so sshd never
        // closes the session and `connect` hangs. Found the hard way on a real
        // cluster.
        //
        // The `>> {log}` target MUST stay a regular file: the daemonize child
        // (chimaera/src/daemonize.rs) keeps only regular-file stdio and
        // re-points everything else at /dev/null, so redirecting to a
        // fifo/pipe — or dropping the redirect — silently sends the remote
        // daemon's logs to /dev/null.
        //
        // Sent BARE (not through `sh_wrap`): pre-existing, and it still assumes
        // a POSIX login shell — folding it into the wait exec is the follow-up.
        &format!(
            "mkdir -p {log_dir}; \
             {env}{bin} serve --daemonize >> {log} 2>&1 < /dev/null && exit; \
             {env}setsid nohup {bin} serve >> {log} 2>&1 < /dev/null & disown",
            log_dir = home.log_dir(),
            env = home.serve_env(),
            bin = home.bin_path(),
            log = home.log_path(),
        ),
    )
    .await?;
    // Wait for the manifest AND a live pid in ONE remote loop instead of up
    // to 15 × 2 client-side execs (each a channel-open RTT + remote fork):
    // 30 half-second ticks = the same 15 s deadline. The manifest is written
    // atomically (tmp + rename), so a readable file is a complete one. The
    // loop prints the manifest (framed) and exits 0 the moment the daemon is
    // up, 1 at the deadline. The old per-exec poll shrugged off one transient
    // ssh failure; a dropped channel (255) retries the wait once.
    let wait = sh_wrap(&start_wait_script(&home.manifest_path(), START_WAIT_TICKS));
    let mut output = output_bounded(ssh_cmd(host).arg(&wait), SSH_ONESHOT_SECS, "ssh").await?;
    if output.status.code() == Some(255) {
        tracing::warn!(
            "the start wait on {host} lost its ssh channel ({}); retrying once",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        output = output_bounded(ssh_cmd(host).arg(&wait), SSH_ONESHOT_SECS, "ssh").await?;
    }
    if output.status.success() {
        return parse_start_wait_output(&String::from_utf8_lossy(&output.stdout)).with_context(
            || format!("daemon on {host} started but its manifest could not be read"),
        );
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    match output.status.code() {
        Some(1) => bail!(
            "daemon on {host} did not start within 15s (check {} there)",
            home.log_path()
        ),
        Some(NO_SLEEP_EXIT) => bail!(
            "{host} has no usable `sleep`; cannot wait for the daemon to start: {}",
            stderr.trim()
        ),
        _ => bail!(
            "waiting for the daemon on {host} failed before its deadline ({}): {}",
            output.status,
            stderr.trim()
        ),
    }
}

/// How many half-second ticks [`start_remote`] waits for the manifest (15 s).
const START_WAIT_TICKS: u32 = 30;

/// The POSIX-sh script [`start_remote`] runs after the start line: up to
/// `ticks` half-second waits for a readable manifest at `manifest_path`
/// written on THIS node whose pid answers `kill -0`, then print it framed
/// (with the tested pid) and exit 0; exit 1 at the deadline. The node check
/// matters on a home shared across nodes: until the new daemon writes its own
/// record the file may still hold another node's, whose pid can be alive
/// here as an unrelated process. Pure so tests can run it under real shells.
fn start_wait_script(manifest_path: &str, ticks: u32) -> String {
    format!(
        "f={manifest_path}; n=$(uname -n 2>/dev/null); {probe} while :; do \
         if [ -r \"$f\" ]; then h=$({hostname}); \
         if {{ [ -z \"$n\" ] || [ \"$h\" = \"$n\" ]; }} && {alive}; then printf '\\n{begin}\\n'; cat \"$f\"; \
         printf '\\n{end}\\npid=%s\\n' \"$p\"; exit 0; fi; fi; \
         [ $i -lt {ticks} ] || exit 1; {tick} done",
        probe = SH_SLEEP_PROBE,
        hostname = SH_MANIFEST_HOSTNAME,
        alive = sh_manifest_alive(),
        begin = MANIFEST_BEGIN,
        end = MANIFEST_END,
        tick = sh_tick(),
    )
}

/// Choose the local tunnel port: explicit flag, else the remote port if
/// locally free, else an OS-assigned free port.
fn pick_local_port(requested: Option<u16>, remote_port: u16) -> anyhow::Result<u16> {
    if let Some(port) = requested {
        return Ok(port);
    }
    // Under the WSL transport the -L bind happens inside the distro, where
    // the remote port number is often ALREADY taken (the WSL-hosted local
    // daemon) — and this Windows-side probe can't see that. Skip straight to
    // an ephemeral pick there; matching the remote port is only a nicety.
    if wsl_transport().is_none() && std::net::TcpListener::bind(("127.0.0.1", remote_port)).is_ok()
    {
        return Ok(remote_port);
    }
    ephemeral_port()
}

fn ephemeral_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .context("failed to find a free local port")?;
    Ok(listener.local_addr()?.port())
}

fn spawn_tunnel(host: &str, local: u16, remote: u16) -> anyhow::Result<Child> {
    ssh_base(host)
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

/// Bring up one login-host forward and prove the intended daemon answers
/// through it before publishing "connected". A local `-L` listener appears
/// before the path behind it is usable, and a stale ControlMaster forward can
/// keep accepting after its transport died; TCP readiness alone therefore
/// creates a connected-but-everything-fails window.
async fn open_proven_tunnel(
    host: &str,
    local: u16,
    remote: u16,
    token: &str,
) -> anyhow::Result<(Child, bool)> {
    let mut child = spawn_tunnel(host, local, remote)?;
    let mux_observed = match wait_for_port(local, &mut child).await {
        Ok(mux) => mux,
        Err(error) => {
            // `wait_for_port` may have observed a successful mux exit before
            // timing out on its listener. That proves this attempt registered
            // the forward, so it also owns cancelling it.
            let cancel_master = forward_delegated(false, &mut child);
            abandon_tunnel_attempt(host, local, remote, child, cancel_master).await;
            return Err(error);
        }
    };
    let Some(mux_delegated) = tunnel_proven(local, token, 15, mux_observed, &mut child).await
    else {
        let cancel_master = forward_delegated(mux_observed, &mut child);
        abandon_tunnel_attempt(host, local, remote, child, cancel_master).await;
        return Err(TunnelPhaseError(format!(
            "ssh tunnel on 127.0.0.1:{local} opened, but the authenticated remote daemon did not answer"
        ))
        .into());
    };
    Ok((child, mux_delegated))
}

/// Tear down the ownership shape of an abandoned attempt. Only issue
/// `-O cancel` when this child proved it delegated: cancelling speculatively
/// after a bind collision could tear down a different healthy caller's
/// identical forward.
async fn abandon_tunnel_attempt(
    host: &str,
    local: u16,
    remote: u16,
    mut child: Child,
    cancel_master: bool,
) {
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    if cancel_master {
        cancel_master_forward(
            host,
            &route_of(host),
            &format!("{local}:127.0.0.1:{remote}"),
        )
        .await;
    }
}

fn forward_delegated(observed: bool, child: &mut Child) -> bool {
    observed
        || child
            .try_wait()
            .ok()
            .flatten()
            .is_some_and(|status| status.success())
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

/// One-shot ssh/scp deadline. Generous on purpose: the FIRST call to a host
/// raises the ControlMaster, and that may sit in an in-app askpass prompt
/// (password / Duo) for up to its 180s window — a tighter bound here would
/// cut users off mid-typing.
const SSH_ONESHOT_SECS: u64 = 240;

/// Run a remote command, failing loudly if it does not exit 0.
async fn ssh_run(host: &str, cmd: &str) -> anyhow::Result<()> {
    let output = output_bounded(ssh_cmd(host).arg(cmd), SSH_ONESHOT_SECS, "ssh").await?;
    if !output.status.success() {
        bail!(
            "ssh {host} {cmd:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

// --- Mode 2: compute-node sessions ------------------------------------------
//
// A chimaera daemon launched AS a Slurm job (by the login daemon's
// POST /compute/sessions) lives on a compute node. Reaching it is a
// two-rung ladder, probed per connect and honest about defeat:
//
//   B (SshAdopt, preferred) — ssh to the NODE itself, first leg relayed
//     through the already-authenticated login ControlMaster (`-W`). The job
//     daemon stays loopback-bound, and pam_slurm_adopt clusters also adopt
//     the connection into the job's cgroup.
//   A (Direct) — forward `local -> node:port` over the login master. Only
//     works when the job was launched with `--bind-routable` (token-gated
//     0.0.0.0); the fallback for clusters that refuse laptop→node ssh.
//   neither — "compute-node sessions not supported on this cluster", the
//     job keeps running and login-node (Mode 1) use still works.

/// Which rung of the node-tunnel ladder carried the connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComputeRung {
    /// Laptop ssh's end-to-end to the node (pam_slurm_adopt clusters that
    /// allow laptop-credential auth on nodes).
    SshAdopt,
    /// A login-node-resident relay to the node's loopback, running AS the
    /// remote command of the same laptop ssh that forwards to it — the
    /// cluster-native path where node sshd is hostbased-only (Sherlock:
    /// found live, laptop legs get "Permission denied (hostbased)").
    /// Lifetimes are coupled: kill the child, both legs die.
    Chained,
    /// Direct login→node port forward; routable-bound jobs only.
    Direct,
}

/// A live tunnel to a compute-node daemon.
pub struct ComputeTunnel {
    /// The LOGIN host alias (what the user calls the cluster).
    pub host: String,
    pub node: String,
    pub job_id: String,
    pub local_port: u16,
    /// The job daemon's port on the node.
    pub port: u16,
    pub token: String,
    pub rung: ComputeRung,
    /// The `-L` spec of a forward the login ControlMaster holds instead of
    /// `child`: rung A's when its mux client delegated, and the chained
    /// rung's outer forward always (its ssh rides `ssh_base`, so the master
    /// owns the local listener even while the child holds the relay).
    /// `None` for rung B1 — `node_ssh_base` pins `ControlPath=none`, so
    /// that child owns its forward end-to-end and dies with it.
    master_forward: Option<String>,
    /// The login alias's [`Route`] when the tunnel opened — which master
    /// holds `master_forward`.
    route: Route,
    child: Child,
}

impl ComputeTunnel {
    /// The UI url: alias + job + node ride the hash so the window can label
    /// itself "alias › node" and scope itself to the allocation.
    pub fn url(&self) -> String {
        format!(
            "http://127.0.0.1:{}/#token={}&host={}&job={}&node={}",
            self.local_port, self.token, self.host, self.job_id, self.node
        )
    }

    /// Wait for the tunnel child (never returns for a healthy rung-B
    /// forward; quickly when rung A delegated to the master).
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }

    /// Kill the tunnel; a master-held forward is also cancelled so local
    /// ports don't leak past the window that opened them.
    pub async fn close(mut self) {
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
        if let Some(spec) = &self.master_forward {
            cancel_master_forward(&self.host, &self.route, spec).await;
        }
    }
}

/// The `-W`-relay ProxyCommand that carries a node-bound ssh's first leg
/// over the login host's existing ControlMaster — no re-auth, no ProxyJump
/// entry required in the user's ssh config. `pin` rides the master of one
/// login node ([`Route::Node`]); `None` the alias's own. The full shared
/// option set, so a first leg that has to dial the master gets the same
/// keepalives and compression as every other. Quoted so a spacey ControlPath
/// survives the shell that runs ProxyCommand; every `%` is doubled because
/// the OUTER ssh percent-expands the ProxyCommand string (a bare `%C` dies
/// with "unknown key %C" — found live on the first rung-B attempt) and the
/// INNER ssh must receive it intact.
fn master_proxy_command(host: &str, pin: Option<&str>) -> String {
    let mut words = vec!["ssh".to_string()];
    if let Some(node) = pin {
        words.push(format!("-o HostName={node}"));
    }
    words.extend(
        ssh_opts()
            .iter()
            .map(|opt| format!("\"{}\"", opt.replace('%', "%%"))),
    );
    words.push(format!("-W %h:%p {host}"));
    words.join(" ")
}

/// [`master_proxy_command`] over whichever master `host`'s route rides: a
/// login node's own for [`Route::Node`]; the alias's for
/// [`Route::NodeViaAlias`] too, since that node's master is itself carried
/// over it.
fn node_proxy_command(host: &str) -> String {
    match route_of(host) {
        Route::Node(node) => master_proxy_command(host, Some(&node)),
        Route::Alias | Route::NodeViaAlias(_) => master_proxy_command(host, None),
    }
}

/// The ssh options for the node leg itself: NO ControlMaster (the child
/// owns its connection; a per-node master would leak sockets per job), fail
/// fast instead of prompting (a rung probe must never hang on interactive
/// auth — a cluster that needs it reads as "rung unavailable" for now).
fn node_ssh_base(host: &str) -> Command {
    let mut c = transport_command("ssh");
    c.env(ASKPASS_ALIAS_ENV, host);
    c.args([
        "-o",
        "ControlPath=none",
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=accept-new",
        "-o",
        "ConnectTimeout=15",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        "-o",
    ]);
    c.arg(format!("ProxyCommand={}", node_proxy_command(host)));
    c
}

/// `user@node` for the direct node leg: node hostnames don't match the
/// user's `Host <alias>` config stanza, so the alias's resolved username
/// must be carried explicitly (found live: the node leg went out under the
/// LOCAL username). `ssh -G` resolves config locally — no connection.
async fn node_target(host: &str, node: &str) -> String {
    let mut cmd = transport_command("ssh");
    cmd.env(ASKPASS_ALIAS_ENV, host).arg("-G").arg(host);
    let user = match output_bounded(&mut cmd, 15, "ssh -G").await {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("user ").map(|u| u.trim().to_string())),
        _ => None,
    };
    match user {
        Some(user) if !user.is_empty() => format!("{user}@{node}"),
        _ => node.to_string(),
    }
}

fn spawn_node_tunnel(
    host: &str,
    node_target: &str,
    local: u16,
    remote: u16,
) -> anyhow::Result<Child> {
    node_ssh_base(host)
        .args(["-o", "ExitOnForwardFailure=yes"])
        .arg("-N")
        .arg("-L")
        .arg(format!("{local}:127.0.0.1:{remote}"))
        .arg(node_target)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| TunnelPhaseError(format!("failed to spawn node ssh tunnel: {e}")).into())
}

/// The chained rung: ONE laptop ssh that (a) forwards `local` to a relay
/// port on the LOGIN node and (b) runs, as its remote command, a
/// login-resident `ssh -N -L` to the node's loopback — hostbased
/// login→node auth, exactly what cluster-internal ssh is built for. The
/// inner relay dies with the outer channel, so nothing is orphaned on the
/// login node.
fn spawn_chained_node_tunnel(
    host: &str,
    node: &str,
    local: u16,
    relay_port: u16,
    remote: u16,
) -> anyhow::Result<Child> {
    ssh_base(host)
        .args(["-o", "ExitOnForwardFailure=yes"])
        .arg("-L")
        .arg(format!("{local}:127.0.0.1:{relay_port}"))
        .arg(host)
        .arg(format!(
            // The INNER ssh runs on the login node, whose OpenSSH can be
            // ancient (Sherlock's rejects `accept-new` — found live).
            // `no` + a null known_hosts is the old-ssh-safe form, and right
            // for cluster-internal hops anyway: node host keys churn on
            // reimage, and the login→node trust is hostbased, not TOFU.
            "exec ssh -N -o BatchMode=yes -o ExitOnForwardFailure=yes \
             -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
             -o ConnectTimeout=15 -L {relay_port}:127.0.0.1:{remote} {node}"
        ))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| TunnelPhaseError(format!("failed to spawn chained node tunnel: {e}")).into())
}

fn spawn_direct_node_tunnel(
    host: &str,
    node: &str,
    local: u16,
    remote: u16,
) -> anyhow::Result<Child> {
    ssh_base(host)
        .args(["-o", "ExitOnForwardFailure=yes"])
        .arg("-N")
        .arg("-L")
        .arg(format!("{local}:{node}:{remote}"))
        .arg(host)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| TunnelPhaseError(format!("failed to spawn direct node tunnel: {e}")).into())
}

/// Build the tunnel to a compute-node daemon: rung B, then rung A when the
/// job daemon has a routable bind, else an honest error. The arbiter on
/// every rung is `tunnel_proven`: an authed 200 through OUR forward, from a
/// child that is still running afterwards (or that delegated the forward to
/// the ControlMaster and exited 0 — rung A whenever a master is up) — a
/// forward that binds but can't reach the daemon, answers with the wrong
/// daemon, or dies right after answering (a bind-clash exit racing a stale
/// relay) is a failure, not a success.
pub async fn connect_compute_node(
    host: &str,
    node: &str,
    job_id: &str,
    port: u16,
    token: &str,
    routable: bool,
) -> anyhow::Result<ComputeTunnel> {
    anyhow::ensure!(!node.is_empty(), "job {job_id} has no node yet (queued?)");
    let mk = |local_port, rung, master_forward, child| ComputeTunnel {
        host: host.to_string(),
        node: node.to_string(),
        job_id: job_id.to_string(),
        local_port,
        port,
        token: token.to_string(),
        rung,
        master_forward,
        route: route_of(host),
        child,
    };

    // Rung B1 — laptop ssh end-to-end to the node; daemon stays loopback.
    let target = node_target(host, node).await;
    let local = pick_local_port(None, port)?;
    match spawn_node_tunnel(host, &target, local, port) {
        Ok(mut child) => match wait_for_port(local, &mut child).await {
            Ok(mux) => {
                if tunnel_proven(local, token, 10, mux, &mut child)
                    .await
                    .is_some()
                {
                    tracing::info!(%node, %job_id, "compute tunnel up (rung B, ssh-adopt)");
                    return Ok(mk(local, ComputeRung::SshAdopt, None, child));
                }
                child.kill().await.ok();
                tracing::info!(%node, "rung B forwarded but the job daemon did not answer");
            }
            Err(err) => tracing::info!(%node, %err, "rung B unavailable"),
        },
        Err(err) => tracing::info!(%node, %err, "rung B spawn failed"),
    }

    // Rung B2 (chained) — the login node relays to the node's loopback.
    // The relay port must be free ON THE LOGIN NODE — always randomized:
    // the daemon's own port number is exactly where a previous connect's
    // relay (or another tenant of a shared login node) already sits, so it
    // is the one candidate guaranteed to clash with ourselves. A bind clash
    // exits the inner ssh (ExitOnForwardFailure), caught by wait_for_port's
    // early-exit branch or by tunnel_proven's still-running check.
    for relay_port in [fastrand_port(), fastrand_port(), fastrand_port()] {
        let local = pick_local_port(None, port)?;
        let Ok(mut child) = spawn_chained_node_tunnel(host, node, local, relay_port, port) else {
            break;
        };
        // The outer `-L` rides `ssh_base`, so the login master holds the
        // local listener — abandoning this attempt must cancel it (killing
        // the child only tears down the relay leg). Best-effort on the Err
        // arm too: an early inner-relay death can land AFTER the forward
        // registered, and cancelling a never-registered spec is a no-op.
        let outer_spec = format!("{local}:127.0.0.1:{relay_port}");
        match wait_for_port(local, &mut child).await {
            Ok(mux)
                if tunnel_proven(local, token, 15, mux, &mut child)
                    .await
                    .is_some() =>
            {
                tracing::info!(%node, %job_id, relay_port, "compute tunnel up (rung B, chained via login node)");
                return Ok(mk(local, ComputeRung::Chained, Some(outer_spec), child));
            }
            Ok(_) => {
                child.kill().await.ok();
                cancel_master_forward(host, &route_of(host), &outer_spec).await;
                tracing::info!(%node, relay_port, "chained rung forwarded but the job daemon did not answer");
            }
            Err(err) => {
                child.kill().await.ok();
                cancel_master_forward(host, &route_of(host), &outer_spec).await;
                tracing::info!(%node, relay_port, %err, "chained rung attempt failed");
            }
        }
    }

    // Rung A — direct login→node forward; only for routable-bound jobs.
    if routable {
        let local = pick_local_port(None, port)?;
        let spec = format!("{local}:{node}:{port}");
        if let Ok(mut child) = spawn_direct_node_tunnel(host, node, local, port) {
            match wait_for_port(local, &mut child).await {
                Ok(mux) => {
                    if let Some(mux) = tunnel_proven(local, token, 10, mux, &mut child).await {
                        tracing::info!(%node, %job_id, "compute tunnel up (rung A, direct)");
                        return Ok(mk(
                            local,
                            ComputeRung::Direct,
                            mux.then(|| spec.clone()),
                            child,
                        ));
                    }
                    // A delegated forward outlives the exited mux client —
                    // the probe failing does not tear it down, so cancel or
                    // the master keeps proxying the local port until it
                    // expires.
                    let cancel_master = forward_delegated(mux, &mut child);
                    child.kill().await.ok();
                    if cancel_master {
                        cancel_master_forward(host, &route_of(host), &spec).await;
                    }
                }
                Err(err) => tracing::info!(%node, %err, "rung A unavailable"),
            }
        }
    }

    bail!(
        "compute-node sessions are not supported on this cluster over ssh (rung B failed{}) — \
         the job keeps running; use it from the login node",
        if routable {
            ", and the direct forward to the node's routable port also failed"
        } else {
            ", and the job was not launched with a routable bind"
        }
    )
}

/// The compute-rung arbiter: poll [`http_alive_authed`] until
/// `deadline_secs` (the LOCAL listener accepts immediately, but the path
/// behind it — the chained rung's login-resident relay especially — takes
/// seconds to establish; a single-shot probe reads "still handshaking" as
/// "not supported", found live), then confirm the ssh child ITSELF is still
/// running. The second check closes a live-found race: a chained relay
/// whose login-side bind clashed can die (ExitOnForwardFailure) moments
/// AFTER a stale relay on the same port answered the probe for it.
///
/// Returns the effective mux ownership on success. `wait_for_port` can observe
/// the listener in the tiny interval before the mux client exits 0; a
/// successful exit after the authenticated answer therefore upgrades the
/// result to delegated instead of falsely rejecting a healthy forward.
async fn tunnel_proven(
    port: u16,
    token: &str,
    deadline_secs: u64,
    mux: bool,
    child: &mut Child,
) -> Option<bool> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(deadline_secs);
    loop {
        if http_alive_authed(port, token).await {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    match child.try_wait() {
        Ok(None) => Some(mux),
        // A direct `ssh -N` never exits successfully while it owns a live
        // forward. Success here means the ControlMaster accepted ownership,
        // even if `wait_for_port` saw the listener a scheduling tick first.
        Ok(Some(status)) if status.success() => Some(true),
        Ok(Some(_)) | Err(_) => None,
    }
}

/// A pseudo-random high port for the chained relay's login-node bind —
/// clock-derived plus a call counter (no rand dependency; bare subsecond
/// nanos can repeat across the quick successive calls of one rung loop);
/// collisions just burn one bounded retry.
fn fastrand_port() -> u16 {
    static SEQ: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let salt = SEQ
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        .wrapping_mul(7919) as u32;
    20000 + ((nanos.wrapping_add(salt)) % 40000) as u16
}

/// Escape a value for a curl `--config` line (`\` and `"` per curl's
/// documented config quoting).
fn curl_config_escape(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Call the LOGIN daemon's API via curl-over-ssh. Token, method, url, and
/// body all ride stdin as a curl config (`--config -`) so nothing sensitive
/// lands in argv on a shared login node. Returns stdout on HTTP success.
/// `timeout_secs` bounds curl end-to-end (`-m`) — size it to the route:
/// cutting a slow-but-legitimate response mid-flight aborts the request's
/// work server-side.
async fn login_daemon_api(
    host: &str,
    manifest: &Manifest,
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
    timeout_secs: u64,
) -> anyhow::Result<String> {
    let mut config = format!(
        "header = \"Authorization: Bearer {}\"\nrequest = \"{}\"\nurl = \"http://127.0.0.1:{}{}\"\n",
        manifest.token, method, manifest.port, path
    );
    if let Some(body) = body {
        config.push_str("header = \"Content-Type: application/json\"\n");
        config.push_str(&format!(
            "data = \"{}\"\n",
            curl_config_escape(&body.to_string())
        ));
    }
    let mut command = ssh_cmd(host);
    command
        .arg(format!("curl -fsS -m {timeout_secs} --config -"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().context("failed to run ssh")?;
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin.write_all(config.as_bytes()).await.ok();
    }
    let output = collect_child_bounded(child, SSH_ONESHOT_SECS, "ssh curl").await?;
    if !output.status.success() {
        bail!(
            "daemon API {method} {path} on {host} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// GET /compute/sessions on the login daemon (the stateless registry).
pub async fn compute_sessions(
    host: &str,
    manifest: &Manifest,
) -> anyhow::Result<serde_json::Value> {
    let out = login_daemon_api(host, manifest, "GET", "/api/v1/compute/sessions", None, 20).await?;
    serde_json::from_str(out.trim()).context("bad compute sessions payload")
}

/// POST /compute/sessions — submit; returns the job id.
pub async fn compute_launch(
    host: &str,
    manifest: &Manifest,
    spec: &serde_json::Value,
) -> anyhow::Result<String> {
    // 60, not the quick-verb 20: the launch route runs Slurm detection plus
    // a multi-round queue-adoption loop server-side (worst case ~30s) — a
    // client-side timeout here kills curl mid-launch and loses the job id.
    let out = login_daemon_api(
        host,
        manifest,
        "POST",
        "/api/v1/compute/sessions",
        Some(spec),
        60,
    )
    .await?;
    let v: serde_json::Value = serde_json::from_str(out.trim()).context("bad launch payload")?;
    v.get("job_id")
        .and_then(|j| j.as_str())
        .map(str::to_string)
        .context("launch returned no job_id")
}

/// DELETE /compute/sessions/{id} — scancel through the login daemon.
pub async fn compute_cancel(host: &str, manifest: &Manifest, job_id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        job_id.chars().all(|c| c.is_ascii_digit()) && !job_id.is_empty(),
        "invalid job id"
    );
    login_daemon_api(
        host,
        manifest,
        "DELETE",
        &format!("/api/v1/compute/sessions/{job_id}"),
        None,
        20,
    )
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe and start-wait frame the manifest on BOTH sides and print
    /// the pid they tested: noise around the frame (an echoing ~/.bashrc) is
    /// ignored, a cut-off frame, a pid that disagrees with the manifest, or a
    /// missing verdict is an error — never a silent "dead" that would start a
    /// second daemon — and a corrupt manifest is a fresh start only when
    /// nothing was found alive in it.
    #[test]
    fn probe_output_is_parsed_strictly_between_markers() {
        let manifest = serde_json::to_string_pretty(&fake_manifest(Some("b.1"), 42)).unwrap();
        let framed = |trailer: &str| {
            format!(
                "noise from ~/.bashrc\n{MANIFEST_BEGIN}\n{manifest}\n{MANIFEST_END}\n{trailer}more noise\n"
            )
        };
        let p = parse_probe_output(&framed("pid=42\nalive\n"))
            .unwrap()
            .expect("manifest");
        assert_eq!(p.manifest.pid, 42);
        assert!(p.alive);
        assert!(
            p.here(),
            "a host that doesn't name its node is taken at its word"
        );
        let p = parse_probe_output(&framed("pid=42\ndead\n"))
            .unwrap()
            .expect("manifest");
        assert!(!p.alive);
        // The node trailer: the fake manifest was written on "host".
        let p = parse_probe_output(&framed("pid=42\nnode=HOST\nhost=host\ndns=same\nalive\n"))
            .unwrap()
            .expect("manifest");
        assert!(p.here(), "node names are case-insensitive");
        assert_eq!(p.manifest_node_resolves, None);
        let p = parse_probe_output(&framed("pid=42\nnode=ln02\nhost=host\ndns=gone\ndead\n"))
            .unwrap()
            .expect("manifest");
        assert!(!p.here(), "written on another node");
        assert_eq!(p.node, "ln02");
        assert_eq!(p.manifest_node_resolves, Some(false));
        let p = parse_probe_output(&framed("pid=42\nnode=ln02\nhost=host\ndns=found\ndead\n"))
            .unwrap()
            .expect("manifest");
        assert_eq!(p.manifest_node_resolves, Some(true));
        let p = parse_probe_output(&framed("pid=42\nnode=ln02\nhost=host\ndns=unknown\ndead\n"))
            .unwrap()
            .expect("manifest");
        assert_eq!(p.manifest_node_resolves, None);
        assert!(
            parse_probe_output(&framed("pid=42\nnode=ln02\nhost=other\ndns=gone\ndead\n")).is_err(),
            "the resolver was asked about a name serde doesn't see — never a \"gone\" for it"
        );
        assert!(
            parse_probe_output("only noise\n").unwrap().is_none(),
            "no begin marker = no manifest"
        );
        assert!(
            parse_probe_output(&format!("{MANIFEST_BEGIN}\n{manifest}\n")).is_err(),
            "cut off before the end marker"
        );
        assert!(
            parse_probe_output(&framed("pid=43\nalive\n")).is_err(),
            "sed and serde disagree on the pid"
        );
        assert!(
            parse_probe_output(&framed("pid=\ndead\n")).is_err(),
            "sed found no pid"
        );
        assert!(
            parse_probe_output(&framed("pid=42\n")).is_err(),
            "no verdict"
        );
        let corrupt =
            |trailer: &str| format!("{MANIFEST_BEGIN}\n{{not json\n{MANIFEST_END}\n{trailer}");
        assert!(
            parse_probe_output(&corrupt("pid=\ndead\n"))
                .unwrap()
                .is_none(),
            "a corrupt manifest with nothing alive is a fresh start"
        );
        assert!(
            parse_probe_output(&corrupt("pid=42\nalive\n")).is_err(),
            "a corrupt manifest with a live pid is never a fresh start"
        );
        assert!(
            parse_probe_output(&corrupt("pid=42\nnode=ln02\nhost=ln01\ndns=found\ndead\n"))
                .is_err(),
            "nor one another node wrote — its pid was never tested there"
        );
        assert!(
            parse_probe_output(&corrupt("pid=42\nnode=ln01\nhost=ln01\ndns=same\ndead\n"))
                .unwrap()
                .is_none(),
            "a corrupt manifest this node wrote, nothing alive: a fresh start"
        );
        assert_eq!(
            parse_start_wait_output(&framed("pid=42\n")).unwrap().pid,
            42
        );
        assert!(parse_start_wait_output("nothing\n").is_err());
        assert!(parse_start_wait_output(&framed("pid=7\n")).is_err());
    }

    /// The remote pid and node-name extraction is a `sed` over the manifest;
    /// pin both patterns against serde's real pretty AND compact renderings
    /// with the host's own sed (BSD on macOS, GNU on Linux — both POSIX), so
    /// a renderer change can never silently make every daemon look dead, or
    /// every manifest look like another node's.
    #[cfg(unix)]
    #[test]
    fn manifest_pid_sed_matches_serde_pretty_and_compact() {
        let dir =
            std::env::temp_dir().join(format!("chimaera-remote-pid-sed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = fake_manifest(Some("b.1"), 4242);
        // The pattern takes the FIRST "pid" line: the real Manifest must stay
        // flat, or a nested pid would be picked instead of the daemon's.
        let pretty = serde_json::to_string_pretty(&manifest).unwrap();
        assert_eq!(
            pretty.lines().filter(|l| l.contains("\"pid\"")).count(),
            1,
            "Manifest must stay flat — a nested pid would be picked first:\n{pretty}"
        );
        for (name, text) in [
            (
                "pretty.json",
                serde_json::to_string_pretty(&manifest).unwrap(),
            ),
            ("compact.json", serde_json::to_string(&manifest).unwrap()),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, text).unwrap();
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("f={}; {SH_MANIFEST_PID}", path.display()))
                .output()
                .expect("run sh");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                "4242",
                "{name}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("f={}; {SH_MANIFEST_HOSTNAME}", path.display()))
                .output()
                .expect("run sh");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                manifest.hostname,
                "{name}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The LOGIN shells the wrapped remote commands get run through here —
    /// sshd hands `ssh host <cmd>` to `$SHELL -c`, so the tests do the same.
    /// The POSIX ones (`sh`: bash-in-POSIX-mode on macOS, dash on Debian CI;
    /// `dash` itself, the strictest), `bash`/`zsh`, and the hostile ones —
    /// `tcsh` (ships with macOS) and `fish` when installed — which read none
    /// of the script's syntax and reach it only through `sh_wrap`. Absent
    /// shells skip.
    #[cfg(unix)]
    fn login_shells() -> Vec<&'static str> {
        ["sh", "dash", "bash", "zsh", "tcsh", "fish"]
            .into_iter()
            .filter(|shell| {
                std::process::Command::new(shell)
                    .args(["-c", "true"])
                    .output()
                    .is_ok_and(|out| out.status.success())
            })
            .collect()
    }

    /// Run `script` exactly as a remote would: wrapped by `sh_wrap`, handed
    /// to `shell -c` the way sshd hands a command to the login shell.
    #[cfg(unix)]
    fn run_script(shell: &str, script: &str) -> std::process::Output {
        std::process::Command::new(shell)
            .args(["-c", &sh_wrap(script)])
            .output()
            .expect("spawn shell")
    }

    /// `sh_wrap`'s quoting is the one form every login shell reads the same:
    /// a script full of single quotes, double quotes, `$(…)`, `$((…))`,
    /// backslashes, and a `!` (csh history expansion — `tcsh -c "sh -c 'echo
    /// hello!world'"` dies with "Event not found") must reach `sh`
    /// byte-for-byte through each of them, tcsh and fish included.
    #[cfg(unix)]
    #[test]
    fn sh_wrap_survives_every_login_shell() {
        let script =
            r#"printf '%s\n' "a'b" 'c"d' $((1+2)) "$(printf '%s' '\(x\)')" '\1' hello!world"#;
        let shells = login_shells();
        // macOS ships tcsh: the hostile-login-shell case must really run here,
        // not silently skip.
        #[cfg(target_os = "macos")]
        assert!(shells.contains(&"tcsh"), "{shells:?}");
        for shell in shells {
            let out = run_script(shell, script);
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "a'b\nc\"d\n3\n\\(x\\)\n\\1\nhello!world\n",
                "{shell}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    /// A pid that is certainly not alive: a child already reaped.
    #[cfg(unix)]
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    /// Spawn `cmd` detached from this process (backgrounded by a throwaway
    /// shell that exits at once), returning its pid. The stop script's
    /// `kill -0` must see the process VANISH after SIGTERM, which a direct
    /// child of the test process never does — it lingers as a zombie until
    /// reaped. Orphaning it to init mirrors the daemon's setsid'd lifetime.
    #[cfg(unix)]
    fn spawn_orphan(cmd: &str) -> u32 {
        // The orphan must not inherit the pipe `output()` waits on, or the
        // throwaway shell's exit is invisible until the orphan itself ends.
        let out = std::process::Command::new("sh")
            .args(["-c", &format!("{cmd} >/dev/null 2>&1 </dev/null & echo $!")])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .expect("pid")
    }

    /// A manifest written on THIS machine, as the daemon here would.
    #[cfg(unix)]
    fn write_manifest(path: &Path, pid: u32) {
        write_manifest_on(path, pid, &uname_n());
    }

    /// A manifest as the daemon on `node` would have written it.
    #[cfg(unix)]
    fn write_manifest_on(path: &Path, pid: u32, node: &str) {
        let manifest = Manifest {
            hostname: node.to_string(),
            ..fake_manifest(Some("b.1"), pid)
        };
        std::fs::write(path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    }

    /// `uname -n` — what the remote scripts compare a manifest against. The
    /// daemon records `gethostname(2)`; the two must agree on one machine.
    #[cfg(unix)]
    fn uname_n() -> String {
        let out = std::process::Command::new("uname")
            .arg("-n")
            .output()
            .unwrap();
        let node = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(
            Some(node.clone()),
            chimaera_core::this_node(),
            "uname -n vs gethostname"
        );
        node
    }

    #[cfg(unix)]
    fn perl_available() -> bool {
        std::process::Command::new("perl")
            .args(["-MSocket=:addrinfo", "-e", "exit 0"])
            .output()
            .is_ok_and(|out| out.status.success())
    }

    /// The one-exec probe command through every login shell, against real
    /// files and pids: alive (this test process), dead (a reaped child), and
    /// absent (no manifest → nothing printed, exit 0).
    #[cfg(unix)]
    #[test]
    fn probe_script_runs_under_every_login_shell() {
        let dir =
            std::env::temp_dir().join(format!("chimaera-probe-script-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("manifest.json");
        let script = probe_script(&path.display().to_string());
        for shell in login_shells() {
            write_manifest(&path, std::process::id());
            let out = run_script(shell, &script);
            assert!(
                out.status.success(),
                "{shell}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let p = parse_probe_output(&String::from_utf8_lossy(&out.stdout))
                .unwrap()
                .expect("manifest");
            assert_eq!(p.manifest.pid, std::process::id());
            assert!(p.alive, "{shell}: this process is alive");
            assert_eq!(p.node, uname_n(), "{shell}: the probe names its node");
            assert!(p.here(), "{shell}: written on this node");
            assert_eq!(
                p.manifest_node_resolves, None,
                "{shell}: no lookup when local"
            );

            write_manifest(&path, dead_pid());
            let out = run_script(shell, &script);
            let p = parse_probe_output(&String::from_utf8_lossy(&out.stdout))
                .unwrap()
                .expect("manifest");
            assert!(!p.alive, "{shell}: a reaped pid is dead");

            // Another node's record: its pid (alive HERE — this process) is
            // not the daemon's liveness, and its name is looked up.
            write_manifest_on(&path, std::process::id(), "localhost");
            let out = run_script(shell, &script);
            let p = parse_probe_output(&String::from_utf8_lossy(&out.stdout))
                .unwrap()
                .expect("manifest");
            assert!(!p.here(), "{shell}: written on another node");
            if perl_available() {
                assert_eq!(
                    p.manifest_node_resolves,
                    Some(true),
                    "{shell}: localhost resolves"
                );
            }
            write_manifest_on(&path, dead_pid(), "chimaera-gone-node.invalid");
            let out = run_script(shell, &script);
            let p = parse_probe_output(&String::from_utf8_lossy(&out.stdout))
                .unwrap()
                .expect("manifest");
            assert!(!p.here());
            // `.invalid` never resolves (RFC 2606); only a resolver that
            // couldn't answer at all may leave it unknown — never "found".
            assert_ne!(p.manifest_node_resolves, Some(true), "{shell}");

            std::fs::remove_file(&path).unwrap();
            let out = run_script(shell, &script);
            assert!(out.status.success(), "{shell}: absent manifest exits 0");
            assert!(parse_probe_output(&String::from_utf8_lossy(&out.stdout))
                .unwrap()
                .is_none());
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The start-wait command through every login shell: prints the manifest
    /// as soon as one appears with a live pid (written mid-loop here, so the
    /// sleep path is exercised) and exits 1 at the deadline otherwise.
    #[cfg(unix)]
    #[test]
    fn start_wait_script_runs_under_every_login_shell() {
        let dir =
            std::env::temp_dir().join(format!("chimaera-start-script-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("manifest.json");
        for shell in login_shells() {
            std::fs::remove_file(&path).ok();
            let child = std::process::Command::new(shell)
                .args([
                    "-c",
                    &sh_wrap(&start_wait_script(&path.display().to_string(), 6)),
                ])
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            std::thread::sleep(Duration::from_millis(700));
            write_manifest(&path, std::process::id());
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success(), "{shell}: manifest appeared in time");
            let m = parse_start_wait_output(&String::from_utf8_lossy(&out.stdout)).unwrap();
            assert_eq!(m.pid, std::process::id());

            // A manifest whose pid is dead never satisfies the loop.
            write_manifest(&path, dead_pid());
            let out = run_script(shell, &start_wait_script(&path.display().to_string(), 2));
            assert_eq!(
                out.status.code(),
                Some(1),
                "{shell}: a dead pid must time out"
            );

            // Nor does another node's record, even when its pid happens to
            // be alive here: that is the file a fresh start replaces, not
            // the daemon it started.
            write_manifest_on(&path, std::process::id(), "other-login-node");
            let out = run_script(shell, &start_wait_script(&path.display().to_string(), 2));
            assert_eq!(
                out.status.code(),
                Some(1),
                "{shell}: another node's manifest must time out"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The stop command through every login shell: SIGTERM ends a cooperative
    /// process (exit 0); one that ignores SIGTERM reaches the deadline and
    /// reports the distinct still-alive code — never SIGKILL; a dead pid
    /// reports the signal failure.
    #[cfg(unix)]
    #[test]
    fn stop_script_runs_under_every_login_shell() {
        for shell in login_shells() {
            let victim = spawn_orphan("sleep 30");
            let out = run_script(shell, &stop_script(victim, 6));
            assert!(
                out.status.success(),
                "{shell}: {}",
                String::from_utf8_lossy(&out.stderr)
            );

            // `exec` keeps the pid: the ignored disposition survives into sleep.
            let stubborn = spawn_orphan("sh -c 'trap \"\" TERM; exec sleep 30'");
            std::thread::sleep(Duration::from_millis(200));
            let out = run_script(shell, &stop_script(stubborn, 2));
            assert_eq!(
                out.status.code(),
                Some(STOP_STILL_ALIVE_EXIT),
                "{shell}: still alive after the deadline"
            );
            let _ = run_script("sh", &format!("kill -9 {stubborn}"));

            let out = run_script(shell, &stop_script(dead_pid(), 2));
            assert_eq!(
                out.status.code(),
                Some(STOP_SIGNAL_FAILED_EXIT),
                "{shell}: SIGTERM to a dead pid fails"
            );
        }
    }

    /// A remote with no `sleep` at all must not spin through its tick budget
    /// in milliseconds and fabricate "still running" / "did not start": both
    /// wait loops exit the distinct no-sleep code instead. Run with a PATH
    /// holding only `sh`, so `sleep` (external) is missing while `sh -c`
    /// still resolves.
    #[cfg(unix)]
    #[test]
    fn wait_loops_report_a_missing_sleep_instead_of_spinning() {
        let dir = std::env::temp_dir().join(format!("chimaera-no-sleep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink("/bin/sh", dir.join("sh")).ok();
        let run = |script: &str| {
            std::process::Command::new("/bin/sh")
                .env("PATH", &dir)
                .args(["-c", &sh_wrap(script)])
                .output()
                .unwrap()
        };
        let stubborn = spawn_orphan("sh -c 'trap \"\" TERM; exec sleep 30'");
        std::thread::sleep(Duration::from_millis(200));
        let out = run(&stop_script(stubborn, 20));
        assert_eq!(
            out.status.code(),
            Some(NO_SLEEP_EXIT),
            "stop: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = run_script("sh", &format!("kill -9 {stubborn}"));
        let out = run(&start_wait_script(
            &dir.join("absent.json").display().to_string(),
            30,
        ));
        assert_eq!(
            out.status.code(),
            Some(NO_SLEEP_EXIT),
            "start wait: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn child_stream_reader_fails_at_byte_cap() {
        let err = read_bounded(tokio::io::repeat(b'x'), 32, "test")
            .await
            .expect_err("infinite output must hit the byte ceiling");
        assert!(err.to_string().contains("32-byte limit"), "{err:#}");
    }

    /// Every ssh/scp call must carry the ControlMaster trio (so one auth
    /// covers the whole session; socket path uses ssh's short `%C` token),
    /// the trust-on-first-use host-key policy that lets a fresh install reach
    /// a never-seen host with no tty, and the liveness bounds that keep a
    /// dead link (laptop sleep) from leaving zombie masters and forwards.
    /// A deep isolated CHIMAERA_HOME (the dev app) must not blow the ~104-byte
    /// unix-socket path limit: the socket falls back to a short /tmp dir keyed
    /// by the home, still ending in `/cm` (so `…/cm/%C` holds), and distinct
    /// homes never collide (a dev master never rides the real app's).
    #[test]
    fn control_dir_stays_under_sun_path_for_a_deep_home() {
        use std::path::Path;
        // Normal home: unchanged — data_dir/cm.
        let normal = control_dir(Path::new("/Users/x/.chimaera"));
        assert!(
            normal.to_string_lossy().ends_with("/.chimaera/cm"),
            "{}",
            normal.display()
        );

        // Deep isolated home (a worktree CHIMAERA_HOME) overshoots → /tmp fallback.
        let deep = control_dir(Path::new(
            "/Users/martinkjellberg/dev/chimaera/.claude/worktrees/magical-colden-00b63c/.chimaera-dev-app/data",
        ));
        assert!(deep.starts_with("/tmp/"), "{}", deep.display());
        assert!(deep.ends_with("cm"), "{}", deep.display());
        // dir + '/' + the 40-char %C expansion + OpenSSH's temporary suffix
        // must clear the ~104-byte cap.
        assert!(
            deep.as_os_str().len() + 1 + 40 + 1 + 16 <= 104,
            "{}",
            deep.display()
        );
        // A different deep home resolves to a different socket dir.
        let other = control_dir(Path::new(
            "/Users/martinkjellberg/dev/chimaera/.claude/worktrees/some-other-worktree-abcdef/.chimaera-dev-app/data",
        ));
        assert_ne!(other, deep);

        // The isolated native app's normal state dir is shorter than a deep
        // worktree but still too long once OpenSSH's temporary suffix is
        // included.
        let app_home = control_dir(Path::new(
            "/Users/martinkjellberg/.chimaera-dev-app/chimaera/data",
        ));
        assert!(app_home.starts_with("/tmp/"), "{}", app_home.display());
        assert!(
            app_home.as_os_str().len() + 1 + 40 + 1 + 16 <= 104,
            "{}",
            app_home.display()
        );
    }

    /// The WSL transport's path vocabulary, pure halves only (the global
    /// transport switch is never flipped in tests — other tests exercise the
    /// direct-ssh path concurrently).
    #[test]
    fn wsl_transport_path_spelling() {
        // scp's "local" side runs in the distro: drive-letter absolutes
        // become /mnt paths, spaces survive, non-drive paths pass through.
        assert_eq!(
            windows_path_as_wsl(r"C:\Users\First Last\bin\chimaera.exe"),
            "/mnt/c/Users/First Last/bin/chimaera.exe"
        );
        assert_eq!(windows_path_as_wsl(r"D:\x"), "/mnt/d/x");
        // \\?\-verbatim paths (current_exe / canonicalize can produce them).
        assert_eq!(
            windows_path_as_wsl(r"\\?\C:\Users\x\chimaera.exe"),
            "/mnt/c/Users/x/chimaera.exe"
        );
        // UNC passes through — callers must treat that as unreachable.
        assert_eq!(windows_path_as_wsl(r"\\server\share\f"), "//server/share/f");
        assert_eq!(windows_path_as_wsl("/already/unixy"), "/already/unixy");
        // The in-distro master socket: same ~/.chimaera/cm shape an
        // in-distro connect uses, comfortably under the sun_path cap.
        let cp = wsl_control_path("/home/someuser");
        assert_eq!(cp, "/home/someuser/.chimaera/cm/%C");
        assert!(cp.len() <= 100);
    }

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
        assert_eq!(opts[14], "-o");
        // The documented one-line revert if a fast-LAN flow ever regresses
        // (docs/perf-remote-plan.md R5) — pinned so dropping or reordering
        // it is a visible, deliberate change.
        assert_eq!(opts[15], "Compression=yes");
        assert_eq!(opts.len(), 16, "pin the whole option list");
    }

    #[test]
    fn ssh_and_scp_children_carry_their_askpass_alias() {
        fn alias(command: &Command) -> Option<String> {
            command
                .as_std()
                .get_envs()
                .find(|(key, _)| *key == ASKPASS_ALIAS_ENV)
                .and_then(|(_, value)| value)
                .map(|value| value.to_string_lossy().into_owned())
        }

        assert_eq!(alias(&ssh_cmd("Sherlock")), Some("Sherlock".into()));
        assert_eq!(alias(&scp_cmd("remote-2")), Some("remote-2".into()));
        assert_eq!(
            alias(&node_ssh_base("login.example.edu")),
            Some("login.example.edu".into())
        );
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

    #[tokio::test]
    async fn http_alive_authed_probes_health_with_endpoint_identity() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut chunk = [0u8; 512];
                    let n = stream.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..n]);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8(request).unwrap();
                assert!(
                    request.starts_with("GET /api/v1/health HTTP/1.1\r\n"),
                    "liveness must use the cheap health route: {request:?}"
                );
                let status = if request.contains("\r\nAuthorization: Bearer right-token\r\n") {
                    "200 OK"
                } else {
                    "401 Unauthorized"
                };
                stream
                    .write_all(format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\n\r\n").as_bytes())
                    .await
                    .unwrap();
            }
        });

        assert!(http_alive_authed(port, "right-token").await);
        assert!(
            !http_alive_authed(port, "wrong-token").await,
            "an HTTP answer from the wrong endpoint identity is not live"
        );
        server.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tunnel_proof_detects_mux_exit_after_listener_readiness() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 512];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        let mut exited = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
        exited.wait().await.unwrap();

        assert_eq!(
            tunnel_proven(port, "token", 1, false, &mut exited).await,
            Some(true),
            "a successful exit after the listener appeared is mux delegation"
        );
        server.await.unwrap();
    }

    /// The remote-home fragments are load-bearing shell strings: every remote
    /// side effect derives from them, so this pins both roots — especially the
    /// dev asymmetry (`CHIMAERA_HOME` relocates the daemon's data to
    /// `<home>/data`, so its manifest/log sit one level deeper than the real
    /// home's) and that scoping rides an ENV PREFIX, never a flag on the
    /// load-bearing `chimaera serve` string.
    #[test]
    fn remote_home_fragments_split_real_and_dev() {
        use RemoteHome::{Dev, Real};
        assert_eq!(Real.manifest_path(), "$HOME/.chimaera/manifest.json");
        assert_eq!(
            Dev.manifest_path(),
            "$HOME/.chimaera-dev/data/manifest.json"
        );
        assert_eq!(Real.log_path(), "$HOME/.chimaera/logs/serve.log");
        assert_eq!(Dev.log_path(), "$HOME/.chimaera-dev/data/logs/serve.log");
        assert_eq!(Real.bin_path(), "$HOME/.chimaera/bin/chimaera");
        assert_eq!(Dev.bin_path(), "$HOME/.chimaera-dev/bin/chimaera");
        assert_eq!(Real.serve_env(), "");
        assert_eq!(Dev.serve_env(), "CHIMAERA_HOME=$HOME/.chimaera-dev ");
        // scp destinations are $HOME-relative: scp expands no shell variables.
        assert!(!Real.scp_staged_bin().contains('$'));
        assert!(!Dev.scp_staged_bin().contains('$'));
        // The two roots must be disjoint — a dev path may never alias a real
        // one (".chimaera-dev" starts with ".chimaera", so check the boundary).
        assert!(!Dev.dir().starts_with(&format!("{}/", Real.dir())));
        assert_ne!(Dev.dir(), Real.dir());
        // The build is the only selector: tests run on the unstamped 0.0.1
        // sentinel, so `current()` must resolve Dev here — a release build
        // (stamped version) resolves Real by the same predicate.
        assert!(chimaera_core::is_dev_build());
        assert_eq!(RemoteHome::current(), RemoteHome::Dev);
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

    /// The real-home reuse check keys on this parse: a dev (0.0.1) version
    /// must be recognized so a stranded dev binary gets replaced, and shell
    /// noise must never read as a version (that would wrongly skip a deploy).
    #[test]
    fn cli_version_parses_only_the_expected_shape() {
        assert_eq!(parse_cli_version("chimaera 0.1.7\n"), Some("0.1.7".into()));
        assert_eq!(parse_cli_version("\nchimaera 0.0.1"), Some("0.0.1".into()));
        assert!(chimaera_core::version_is_dev("0.0.1"));
        assert_eq!(parse_cli_version("bash: chimaera: command not found"), None);
        assert_eq!(parse_cli_version("chimaera"), None);
        assert_eq!(parse_cli_version(""), None);
    }

    // --- resolve_daemon characterization (fake side effects) ----------------
    //
    // The crate can't be live-verified (no remote host in CI), so these pin
    // the connect DECISION phase against a fake RemoteOps: the ordered call log
    // proves which side effects fire (and their order — the Update arm's
    // resolve-before-stop guard), and a phase-label capture proves the progress
    // emits. SshOps delegates verbatim, so what holds for the fake holds live.

    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    /// One recorded [`RemoteOps`] call, in order.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Call {
        RemoteProbe,
        RemoteSessionsCount,
        ResolveLocalBinary,
        StopRemote,
        DeployBinary,
        StartRemote,
        EnsureRemoteBinary,
    }

    /// What a scripted probe answers along one route.
    type ProbeScript = Box<dyn Fn(&Route) -> anyhow::Result<ProbeRun>>;

    /// A scripted [`RemoteOps`] that records its ordered call log — each call
    /// with the route it ran along — and returns canned outcomes: no ssh, no
    /// host, no real binary.
    struct FakeOps {
        log: RefCell<Vec<(Call, Route)>>,
        route: RefCell<Route>,
        probe_manifest: Option<Manifest>,
        alive: bool,
        sessions: Option<usize>,
        resolved_bin: PathBuf,
        start_manifest: Manifest,
        /// Overrides the default probe (the manifest, written on the node
        /// the probe lands on) — for multi-node scenarios.
        probe: Option<ProbeScript>,
    }

    impl FakeOps {
        fn base() -> Self {
            FakeOps {
                log: RefCell::new(Vec::new()),
                route: RefCell::new(Route::Alias),
                probe_manifest: None,
                alive: false,
                sessions: None,
                resolved_bin: PathBuf::from("/unused"),
                start_manifest: fake_manifest(Some(chimaera_core::BUILD_ID), 999),
                probe: None,
            }
        }
        fn record(&self, c: Call) {
            let route = self.route.borrow().clone();
            self.log.borrow_mut().push((c, route));
        }
        fn calls(&self) -> Vec<Call> {
            self.log.borrow().iter().map(|(c, _)| *c).collect()
        }
        fn routed(&self) -> Vec<(Call, Route)> {
            self.log.borrow().clone()
        }
    }

    impl RemoteOps for FakeOps {
        fn route(&self, _host: &str) -> Route {
            self.route.borrow().clone()
        }
        fn set_route(&self, _host: &str, route: Route) {
            *self.route.borrow_mut() = route;
        }
        async fn remote_probe(&self, _host: &str) -> anyhow::Result<ProbeRun> {
            self.record(Call::RemoteProbe);
            if let Some(probe) = &self.probe {
                return probe(&self.route.borrow());
            }
            Ok(ProbeRun::Ran(self.probe_manifest.clone().map(|m| Probe {
                node: m.hostname.clone(),
                manifest: m,
                alive: self.alive,
                manifest_node_resolves: None,
            })))
        }
        async fn remote_sessions_count(
            &self,
            _host: &str,
            _manifest: &Manifest,
        ) -> anyhow::Result<Option<usize>> {
            self.record(Call::RemoteSessionsCount);
            Ok(self.sessions)
        }
        async fn resolve_local_binary(
            &self,
            _host: &str,
            _binary: Option<&Path>,
            _progress: &impl Fn(Phase),
        ) -> anyhow::Result<PathBuf> {
            self.record(Call::ResolveLocalBinary);
            Ok(self.resolved_bin.clone())
        }
        async fn stop_remote(&self, _host: &str, _pid: u32) -> anyhow::Result<()> {
            self.record(Call::StopRemote);
            Ok(())
        }
        async fn deploy_binary(
            &self,
            _host: &str,
            _path: &Path,
            _progress: &impl Fn(Phase),
        ) -> anyhow::Result<()> {
            self.record(Call::DeployBinary);
            Ok(())
        }
        async fn start_remote(&self, _host: &str) -> anyhow::Result<Manifest> {
            self.record(Call::StartRemote);
            Ok(self.start_manifest.clone())
        }
        async fn ensure_remote_binary(
            &self,
            _host: &str,
            _binary: Option<&Path>,
            _progress: &impl Fn(Phase),
        ) -> anyhow::Result<()> {
            self.record(Call::EnsureRemoteBinary);
            Ok(())
        }
    }

    fn fake_manifest(build: Option<&str>, pid: u32) -> Manifest {
        Manifest {
            hostname: "host".into(),
            port: 4600,
            token: "token".into(),
            pid,
            version: "0.0.1".into(),
            started_at: 0,
            build: build.map(str::to_string),
        }
    }

    /// Drive `resolve_daemon` against `fake`, returning its result and the
    /// ordered list of phase labels emitted via the progress sink.
    async fn run_resolve(
        fake: &FakeOps,
        update_daemon: bool,
    ) -> ((Manifest, bool, Option<usize>), Vec<&'static str>) {
        let (out, phases) = try_resolve(fake, update_daemon).await;
        (out.expect("resolve_daemon"), phases)
    }

    async fn try_resolve(
        fake: &FakeOps,
        update_daemon: bool,
    ) -> (
        anyhow::Result<(Manifest, bool, Option<usize>)>,
        Vec<&'static str>,
    ) {
        let phases = RefCell::new(Vec::<&'static str>::new());
        let progress = |p: Phase| {
            phases.borrow_mut().push(match p {
                Phase::Probing => "probing",
                Phase::Routing { .. } => "routing",
                Phase::Updating => "updating",
                Phase::Downloading { .. } => "downloading",
                Phase::Installing { .. } => "installing",
                Phase::Starting => "starting",
                Phase::Tunneling { .. } => "tunneling",
            });
        };
        let opts = ConnectOpts {
            update_daemon,
            ..Default::default()
        };
        let out = resolve_daemon(fake, "host", &opts, &progress).await;
        (out, phases.into_inner())
    }

    /// Reuse: a matching build with no forced update attaches to the running
    /// daemon as-is — the session count is skipped (it can't change the
    /// decision), nothing is stopped/deployed/started, and the only phase is
    /// the initial probe.
    #[tokio::test]
    async fn resolve_daemon_reuses_matching_build() {
        let fake = FakeOps {
            probe_manifest: Some(fake_manifest(Some(chimaera_core::BUILD_ID), 42)),
            alive: true,
            sessions: Some(3),
            resolved_bin: PathBuf::from("/unused"),
            start_manifest: fake_manifest(Some(chimaera_core::BUILD_ID), 999),
            ..FakeOps::base()
        };
        let ((manifest, outdated, live), phases) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 42, "returns the probed daemon");
        assert!(!outdated);
        assert_eq!(live, None);
        assert_eq!(fake.calls(), vec![Call::RemoteProbe]);
        assert_eq!(phases, vec!["probing"]);
    }

    /// Update: a build mismatch with a provably idle daemon (sessions == 0)
    /// replaces it — and CRITICALLY resolves the replacement binary BEFORE
    /// stopping the old daemon, so a failed fetch never strands the host.
    /// Returns the freshly started daemon's manifest, not outdated.
    #[tokio::test]
    async fn resolve_daemon_update_resolves_binary_before_stop() {
        let fake = FakeOps {
            // No build id = ancient = mismatch against any real BUILD_ID.
            probe_manifest: Some(fake_manifest(None, 42)),
            alive: true,
            sessions: Some(0),
            resolved_bin: PathBuf::from("/tmp/chimaera"),
            start_manifest: fake_manifest(Some(chimaera_core::BUILD_ID), 999),
            ..FakeOps::base()
        };
        let ((manifest, outdated, live), phases) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 999, "returns the freshly started daemon");
        assert!(!outdated);
        assert_eq!(live, None);
        assert_eq!(
            fake.calls(),
            vec![
                Call::RemoteProbe,
                Call::RemoteSessionsCount,
                Call::ResolveLocalBinary,
                Call::StopRemote,
                Call::DeployBinary,
                Call::StartRemote,
            ],
            "resolve-local-binary must precede stop-remote"
        );
        assert_eq!(phases, vec!["probing", "updating", "starting"]);
    }

    /// Update via `--update-daemon`: force replaces even a matching build with
    /// live sessions, still fetching the count first (for the log line). Same
    /// resolve-before-stop ordering as the mismatch path.
    #[tokio::test]
    async fn resolve_daemon_force_update_ignores_live_sessions() {
        let fake = FakeOps {
            probe_manifest: Some(fake_manifest(Some(chimaera_core::BUILD_ID), 42)),
            alive: true,
            sessions: Some(5),
            resolved_bin: PathBuf::from("/tmp/chimaera"),
            start_manifest: fake_manifest(Some(chimaera_core::BUILD_ID), 999),
            ..FakeOps::base()
        };
        let ((manifest, outdated, _live), phases) = run_resolve(&fake, true).await;
        assert_eq!(manifest.pid, 999);
        assert!(!outdated);
        assert_eq!(
            fake.calls(),
            vec![
                Call::RemoteProbe,
                Call::RemoteSessionsCount,
                Call::ResolveLocalBinary,
                Call::StopRemote,
                Call::DeployBinary,
                Call::StartRemote,
            ]
        );
        assert_eq!(phases, vec!["probing", "updating", "starting"]);
    }

    /// Update whose stop went unconfirmed: the start-wait hands back the OLD
    /// daemon's manifest (same pid, old build) because the new one refused to
    /// start beside it. That is a working daemon, so connect — but report it
    /// as outdated with its live count, never as a completed update.
    #[tokio::test]
    async fn resolve_daemon_update_that_did_not_replace_reports_outdated() {
        let fake = FakeOps {
            probe_manifest: Some(fake_manifest(Some("old.1"), 42)),
            alive: true,
            sessions: Some(0),
            resolved_bin: PathBuf::from("/tmp/chimaera"),
            start_manifest: fake_manifest(Some("old.1"), 42),
            ..FakeOps::base()
        };
        let ((manifest, outdated, live), phases) = run_resolve(&fake, false).await;
        assert_eq!(
            manifest.pid, 42,
            "connects to the daemon that is actually serving"
        );
        assert!(
            outdated,
            "an update that left the old build serving is not a success"
        );
        assert_eq!(live, Some(0));
        assert_eq!(phases, vec!["probing", "updating", "starting"]);
    }

    /// ConnectOutdated: a build mismatch with live sessions and no forced
    /// update attaches to the old daemon as-is — surfacing the mismatch and the
    /// live count, with no stop/deploy/start and only the probe phase.
    #[tokio::test]
    async fn resolve_daemon_connects_outdated_with_live_sessions() {
        let fake = FakeOps {
            probe_manifest: Some(fake_manifest(None, 42)),
            alive: true,
            sessions: Some(2),
            resolved_bin: PathBuf::from("/unused"),
            start_manifest: fake_manifest(Some(chimaera_core::BUILD_ID), 999),
            ..FakeOps::base()
        };
        let ((manifest, outdated, live), phases) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 42, "attaches to the old daemon");
        assert!(outdated);
        assert_eq!(live, Some(2));
        assert_eq!(
            fake.calls(),
            vec![Call::RemoteProbe, Call::RemoteSessionsCount]
        );
        assert_eq!(phases, vec!["probing"]);
    }

    /// Fresh start (no manifest): the one-exec probe reports nothing running,
    /// then ensure-binary and start a new daemon.
    #[tokio::test]
    async fn resolve_daemon_fresh_start_when_no_manifest() {
        let fake = FakeOps {
            probe_manifest: None,
            // Unused here: with no manifest the fake probe reports nothing.
            alive: false,
            sessions: None,
            resolved_bin: PathBuf::from("/unused"),
            start_manifest: fake_manifest(Some(chimaera_core::BUILD_ID), 999),
            ..FakeOps::base()
        };
        let ((manifest, outdated, live), phases) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 999);
        assert!(!outdated);
        assert_eq!(live, None);
        assert_eq!(
            fake.calls(),
            vec![
                Call::RemoteProbe,
                Call::EnsureRemoteBinary,
                Call::StartRemote
            ]
        );
        assert_eq!(phases, vec!["probing", "starting"]);
    }

    /// Fresh start (stale manifest, dead pid): the probe returns a manifest
    /// whose pid is dead, which falls through to the same fresh-start path.
    #[tokio::test]
    async fn resolve_daemon_fresh_start_when_manifest_pid_dead() {
        let fake = FakeOps {
            probe_manifest: Some(fake_manifest(Some(chimaera_core::BUILD_ID), 42)),
            alive: false,
            sessions: None,
            resolved_bin: PathBuf::from("/unused"),
            start_manifest: fake_manifest(Some(chimaera_core::BUILD_ID), 999),
            ..FakeOps::base()
        };
        let ((manifest, _outdated, _live), phases) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 999);
        assert_eq!(
            fake.calls(),
            vec![
                Call::RemoteProbe,
                Call::EnsureRemoteBinary,
                Call::StartRemote,
            ]
        );
        assert_eq!(phases, vec!["probing", "starting"]);
    }

    // --- round-robin login nodes ------------------------------------------
    //
    // A pool alias ("login.cluster.edu" → ln01..lnNN) over one shared $HOME:
    // the daemon runs on ln01, a fresh ControlMaster landed on ln02. Before
    // the node was compared, the ln02 probe's `kill -0` (an unrelated process
    // table) read "dead" and connect started a second daemon on ln02 — over
    // the same manifest and session ledger, orphaning ln01's daemon and
    // resuming its sessions a second time.

    const LN01: &str = "ln01.cluster.edu";
    const LN02: &str = "ln02.cluster.edu";

    /// The daemon's manifest, written on `node`.
    fn manifest_on(node: &str, build: Option<&str>, pid: u32) -> Manifest {
        Manifest {
            hostname: node.to_string(),
            ..fake_manifest(build, pid)
        }
    }

    /// `manifest` as seen by a probe that ran on `node`.
    fn seen_from(
        node: &str,
        manifest: &Manifest,
        alive: bool,
        resolves: Option<bool>,
    ) -> anyhow::Result<ProbeRun> {
        Ok(ProbeRun::Ran(Some(Probe {
            manifest: manifest.clone(),
            node: node.to_string(),
            alive,
            manifest_node_resolves: resolves,
        })))
    }

    fn ssh_failed(stderr: &str) -> anyhow::Result<ProbeRun> {
        Ok(ProbeRun::Failed(ProbeFailure {
            status: "exit status: 255".to_string(),
            stderr: stderr.to_string(),
        }))
    }

    /// The pool alias lands on ln02; `node_route` answers for every routed
    /// probe.
    fn pool(
        daemon: Manifest,
        landed_kill0: bool,
        resolves: Option<bool>,
        node_route: impl Fn(&Route, &Manifest) -> anyhow::Result<ProbeRun> + 'static,
    ) -> FakeOps {
        FakeOps {
            probe: Some(Box::new(move |route| match route {
                Route::Alias => seen_from(LN02, &daemon, landed_kill0, resolves),
                route => node_route(route, &daemon),
            })),
            ..FakeOps::base()
        }
    }

    /// THE bug: a manifest from another login node is never judged dead by
    /// the node the connection landed on. The routed probe on ln01 finds the
    /// daemon alive, so connect attaches to it — every later op (here none;
    /// see the update case) runs along the ln01 route, and nothing starts.
    #[tokio::test]
    async fn a_manifest_from_another_login_node_is_not_judged_from_this_one() {
        let fake = pool(
            manifest_on(LN01, Some(chimaera_core::BUILD_ID), 42),
            // ln02 has no pid 42 (or an unrelated one): the old verdict.
            false,
            Some(true),
            |route, m| match route {
                Route::Node(n) if n == LN01 => seen_from(LN01, m, true, None),
                other => panic!("unexpected route {other:?}"),
            },
        );
        let ((manifest, outdated, _), phases) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 42, "attaches to ln01's daemon");
        assert!(!outdated);
        assert_eq!(
            fake.routed(),
            vec![
                (Call::RemoteProbe, Route::Alias),
                (Call::RemoteProbe, Route::Node(LN01.into())),
            ],
            "no fresh start"
        );
        assert_eq!(
            *fake.route.borrow(),
            Route::Node(LN01.into()),
            "left routed"
        );
        assert_eq!(phases, vec!["probing", "routing"]);
    }

    /// Every op after the routing decision runs on the daemon's node: an
    /// idle outdated daemon on ln01 is counted, stopped, and restarted THERE.
    #[tokio::test]
    async fn a_routed_update_counts_stops_and_restarts_on_the_daemons_node() {
        let fake = FakeOps {
            sessions: Some(0),
            resolved_bin: PathBuf::from("/tmp/chimaera"),
            start_manifest: manifest_on(LN01, Some(chimaera_core::BUILD_ID), 999),
            ..pool(
                manifest_on(LN01, Some("old.1"), 42),
                false,
                Some(true),
                |_, m| seen_from(LN01, m, true, None),
            )
        };
        let ((manifest, outdated, _), phases) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 999);
        assert!(!outdated);
        let ln01 = Route::Node(LN01.into());
        assert_eq!(
            fake.routed(),
            vec![
                (Call::RemoteProbe, Route::Alias),
                (Call::RemoteProbe, ln01.clone()),
                (Call::RemoteSessionsCount, ln01.clone()),
                (Call::ResolveLocalBinary, ln01.clone()),
                (Call::StopRemote, ln01.clone()),
                (Call::DeployBinary, ln01.clone()),
                (Call::StartRemote, ln01),
            ]
        );
        assert_eq!(phases, vec!["probing", "routing", "updating", "starting"]);
    }

    /// Provably dead ON its own node: the verdict now counts, and the fresh
    /// daemon starts on that node (the route stays there).
    #[tokio::test]
    async fn a_daemon_dead_on_its_own_node_is_replaced_there() {
        let fake = pool(
            manifest_on(LN01, Some(chimaera_core::BUILD_ID), 42),
            true, // ln02 has an unrelated pid 42 alive — irrelevant
            Some(true),
            |_, m| seen_from(LN01, m, false, None),
        );
        let ((manifest, ..), _) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 999);
        let ln01 = Route::Node(LN01.into());
        assert_eq!(
            fake.routed(),
            vec![
                (Call::RemoteProbe, Route::Alias),
                (Call::RemoteProbe, ln01.clone()),
                (Call::EnsureRemoteBinary, ln01.clone()),
                (Call::StartRemote, ln01),
            ]
        );
    }

    /// Unreachable is not dead: when ln01 can't be reached (here: the auth
    /// prompt was refused) nothing is started, the error says what is where
    /// and why, and no second route is tried — it would prompt again.
    #[tokio::test]
    async fn an_unreachable_daemon_node_is_an_error_never_a_fresh_start() {
        let fake = pool(
            manifest_on(LN01, Some(chimaera_core::BUILD_ID), 42),
            false,
            Some(true),
            |_, _| {
                ssh_failed(
                    "mkjellbe@ln01.cluster.edu: Permission denied (gssapi-with-mic,password).",
                )
            },
        );
        let (out, phases) = try_resolve(&fake, false).await;
        let err = format!("{:#}", out.expect_err("must not resolve"));
        assert!(
            err.contains("runs on login node ln01.cluster.edu (pid 42)"),
            "{err}"
        );
        assert!(err.contains("landed on ln02.cluster.edu"), "{err}");
        assert!(err.contains("Permission denied"), "{err}");
        assert!(err.contains("Nothing was started"), "{err}");
        assert_eq!(
            fake.routed(),
            vec![
                (Call::RemoteProbe, Route::Alias),
                (Call::RemoteProbe, Route::Node(LN01.into())),
            ]
        );
        assert_eq!(*fake.route.borrow(), Route::Alias, "no half-learned route");
        assert_eq!(phases, vec!["probing", "routing"]);
    }

    /// A direct dial that never reached ln01's sshd (the name doesn't
    /// resolve here, a firewall) falls back to reaching it from inside the
    /// cluster, through the alias's own master.
    #[tokio::test]
    async fn a_node_this_machine_cannot_dial_is_reached_through_the_alias() {
        let fake = pool(
            manifest_on(LN01, Some(chimaera_core::BUILD_ID), 42),
            false,
            Some(true),
            |route, m| match route {
                Route::Node(_) => {
                    ssh_failed("ssh: connect to host ln01.cluster.edu port 22: Operation timed out")
                }
                Route::NodeViaAlias(_) => seen_from(LN01, m, true, None),
                Route::Alias => unreachable!(),
            },
        );
        let ((manifest, ..), _) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 42);
        assert_eq!(
            fake.routed(),
            vec![
                (Call::RemoteProbe, Route::Alias),
                (Call::RemoteProbe, Route::Node(LN01.into())),
                (Call::RemoteProbe, Route::NodeViaAlias(LN01.into())),
            ]
        );
        assert_eq!(*fake.route.borrow(), Route::NodeViaAlias(LN01.into()));
    }

    /// Both routes failing is the same honest error.
    #[tokio::test]
    async fn no_route_to_the_daemons_node_is_an_error() {
        let fake = pool(
            manifest_on(LN01, Some(chimaera_core::BUILD_ID), 42),
            false,
            None,
            |_, _| {
                ssh_failed("ssh: Could not resolve hostname ln01.cluster.edu: nodename nor servname provided")
            },
        );
        let (out, _) = try_resolve(&fake, false).await;
        assert!(format!("{:#}", out.unwrap_err()).contains("could not reach ln01.cluster.edu"));
        assert_eq!(
            fake.calls(),
            vec![Call::RemoteProbe; 3],
            "alias + both routes, no start"
        );
        assert_eq!(*fake.route.borrow(), Route::Alias);
    }

    /// A bare node name is never dialed from this machine (its search
    /// domains could reach an unrelated host — and hand it the password);
    /// it only travels inside the cluster.
    #[tokio::test]
    async fn a_bare_node_name_is_only_reached_from_inside_the_cluster() {
        let fake = pool(
            manifest_on("login1", Some(chimaera_core::BUILD_ID), 42),
            false,
            Some(true),
            |route, m| match route {
                Route::NodeViaAlias(n) if n == "login1" => seen_from("login1", m, true, None),
                other => panic!("dialed {other:?}"),
            },
        );
        let ((manifest, ..), _) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 42);
        assert_eq!(*fake.route.borrow(), Route::NodeViaAlias("login1".into()));
    }

    /// The node's name no longer exists in the cluster (decommissioned or
    /// renamed): its daemon went with it, so a fresh start on the landed node
    /// is safe — and the only case a foreign manifest yields one unrouted.
    #[tokio::test]
    async fn a_daemon_whose_node_no_longer_exists_is_replaced_where_we_landed() {
        let fake = pool(
            manifest_on(LN01, Some(chimaera_core::BUILD_ID), 42),
            false,
            Some(false),
            |route, _| panic!("dialed {route:?} for a node that doesn't resolve"),
        );
        let ((manifest, ..), phases) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 999);
        assert_eq!(
            fake.routed(),
            vec![
                (Call::RemoteProbe, Route::Alias),
                (Call::EnsureRemoteBinary, Route::Alias),
                (Call::StartRemote, Route::Alias),
            ]
        );
        assert_eq!(phases, vec!["probing", "starting"]);
    }

    /// A renamed host: the manifest's old name still leads to the node we
    /// landed on, so the landed probe's verdict was local after all.
    #[tokio::test]
    async fn a_renamed_host_keeps_its_local_verdict() {
        let fake = pool(
            manifest_on("old-name.cluster.edu", Some(chimaera_core::BUILD_ID), 42),
            true,
            Some(true),
            |_, m| seen_from(LN02, m, true, None),
        );
        let ((manifest, ..), _) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 42);
        assert_eq!(*fake.route.borrow(), Route::Alias, "one node — no route");
    }

    /// A manifest's node name is data from a remote disk: anything but a
    /// plain host name is refused before it reaches ssh's argv or a
    /// ProxyCommand's shell.
    #[tokio::test]
    async fn a_hostile_node_name_is_never_dialed() {
        let fake = pool(
            manifest_on("ln01;touch${IFS}/tmp/x", Some(chimaera_core::BUILD_ID), 42),
            false,
            Some(true),
            |route, _| panic!("dialed {route:?}"),
        );
        let (out, _) = try_resolve(&fake, false).await;
        assert!(format!("{:#}", out.unwrap_err()).contains("not a plain host name"));
        assert_eq!(fake.calls(), vec![Call::RemoteProbe]);
    }

    /// A route learned by an earlier connect goes first — straight to the
    /// daemon's node, no detour through wherever the pool lands.
    #[tokio::test]
    async fn a_learned_route_is_probed_first() {
        let fake = pool(
            manifest_on(LN01, Some(chimaera_core::BUILD_ID), 42),
            false,
            Some(true),
            |_, m| seen_from(LN01, m, true, None),
        );
        *fake.route.borrow_mut() = Route::Node(LN01.into());
        let ((manifest, ..), phases) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 42);
        assert_eq!(
            fake.routed(),
            vec![(Call::RemoteProbe, Route::Node(LN01.into()))]
        );
        assert_eq!(phases, vec!["probing"]);
    }

    /// A learned route that no longer answers starts over from the alias —
    /// here the daemon has since been started on the node the alias lands on.
    #[tokio::test]
    async fn a_stale_learned_route_starts_over() {
        let fake = FakeOps {
            probe: Some(Box::new(|route| match route {
                Route::Node(_) => {
                    ssh_failed("ssh: connect to host ln01.cluster.edu port 22: No route to host")
                }
                _ => seen_from(
                    LN02,
                    &manifest_on(LN02, Some(chimaera_core::BUILD_ID), 77),
                    true,
                    None,
                ),
            })),
            ..FakeOps::base()
        };
        *fake.route.borrow_mut() = Route::Node(LN01.into());
        let ((manifest, ..), _) = run_resolve(&fake, false).await;
        assert_eq!(manifest.pid, 77);
        assert_eq!(
            fake.routed(),
            vec![
                (Call::RemoteProbe, Route::Node(LN01.into())),
                (Call::RemoteProbe, Route::Alias),
            ]
        );
        assert_eq!(*fake.route.borrow(), Route::Alias);
    }

    /// The route's ssh options: a node route overrides only `HostName`; the
    /// via-alias route adds a `-W` first leg over the alias's own master,
    /// with its `%` escaped for the outer ssh's expansion.
    #[test]
    fn route_options_pin_the_node() {
        assert!(route_opts("pool", &Route::Alias).is_empty());
        assert_eq!(
            route_opts("pool", &Route::Node(LN01.into())),
            vec!["-o".to_string(), format!("HostName={LN01}")]
        );
        let via = route_opts("pool", &Route::NodeViaAlias(LN01.into()));
        assert_eq!(via[..2], ["-o".to_string(), format!("HostName={LN01}")]);
        assert_eq!(via[2], "-o");
        let proxy = via[3]
            .strip_prefix("ProxyCommand=")
            .expect("a ProxyCommand");
        assert!(proxy.starts_with("ssh "), "{proxy}");
        assert!(proxy.ends_with(" -W %h:%p pool"), "{proxy}");
        assert!(
            proxy.contains("%%C"),
            "ControlPath token survives the outer expansion: {proxy}"
        );
        assert!(
            !proxy.contains("HostName"),
            "the first leg is the alias's own master: {proxy}"
        );
        assert!(master_proxy_command("pool", Some(LN01)).contains(&format!("-o HostName={LN01}")));
    }

    #[test]
    fn node_names_are_validated_before_ssh_sees_them() {
        for ok in [LN01, "login1", "sh03-ln06.stanford.edu", "node_7"] {
            assert!(valid_node_name(ok), "{ok}");
        }
        for bad in [
            "",
            "-oProxyCommand=x",
            ".hidden",
            "a b",
            "a;b",
            "$(id)",
            "a%C",
            "a\"b",
        ] {
            assert!(!valid_node_name(bad), "{bad}");
        }
        assert_eq!(
            routes_to(LN01),
            vec![Route::Node(LN01.into()), Route::NodeViaAlias(LN01.into())]
        );
        assert_eq!(
            routes_to("login1"),
            vec![Route::NodeViaAlias("login1".into())]
        );
    }

    #[test]
    fn only_network_level_failures_try_another_route() {
        let failure = |stderr: &str| ProbeFailure {
            status: "exit status: 255".to_string(),
            stderr: stderr.to_string(),
        };
        for net in [
            "ssh: Could not resolve hostname ln01: Name or service not known",
            "ssh: connect to host ln01 port 22: Connection timed out",
            "ssh: connect to host ln01 port 22: Connection refused",
            "kex_exchange_identification: read: Connection reset by peer",
            "Connection timed out during banner exchange",
        ] {
            assert!(failure(net).network_level(), "{net}");
        }
        for auth in [
            "u@ln01: Permission denied (gssapi-with-mic,password).",
            "Host key verification failed.",
            "Received disconnect from 1.2.3.4 port 22:2: Too many authentication failures",
        ] {
            assert!(!failure(auth).network_level(), "{auth}");
        }
        assert_eq!(
            failure("Welcome to the cluster\nu@ln01: Permission denied (password).\n").to_string(),
            "u@ln01: Permission denied (password).",
            "ssh's own last line, not the banner"
        );
    }
}
