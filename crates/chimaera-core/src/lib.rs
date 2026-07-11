//! Shared types and helpers for the chimaera daemon and CLI.

#[cfg(unix)]
pub mod shellint;

use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Version of the chimaera workspace.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Whether this binary is a DEV build (built from a checkout) rather than a
/// stamped release. Rides the release convention: every `Cargo.toml` in the
/// repo holds the literal `0.0.1` sentinel, and the release workflow seds it
/// to the real version at build time — so `0.0.1` at runtime means "never
/// release-stamped". Gates developer-only capability (the isolated dev
/// connect) out of production builds.
pub fn is_dev_build() -> bool {
    version_is_dev(VERSION)
}

/// The pure predicate behind [`is_dev_build`] — `VERSION` is fixed at compile
/// time (always the sentinel under `cargo test`), so the release case is only
/// testable through this.
pub fn version_is_dev(version: &str) -> bool {
    version == "0.0.1"
}

/// The project's repository URL (from `Cargo.toml`), e.g.
/// `https://github.com/<owner>/<repo>`. Used to locate the GitHub release a
/// build came from when auto-fetching the matching daemon for a remote host.
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// Build identity of this binary: `<git-short-hash>[-dirty].<build-unix-secs>`
/// (e.g. `ff52221-dirty.1783438290`), embedded by build.rs. Builds outside a
/// git checkout embed `unknown.<secs>`.
pub const BUILD_ID: &str = env!("CHIMAERA_BUILD_ID");

/// The source-identity part of a build id — everything before the build
/// timestamp (`ff52221-dirty.1783438290` → `ff52221-dirty`).
pub fn build_ref(id: &str) -> &str {
    id.rsplit_once('.').map_or(id, |(r, _)| r)
}

/// Whether two build ids denote the same source. Timestamps are deliberately
/// ignored: the same commit is cross-compiled for clusters (`just dist`) and
/// built natively (the connecting CLI/app) at different moments, and those
/// must compare equal or every connect would "update" the daemon forever.
/// `None` (a manifest that predates build ids) never matches — missing means
/// ancient by definition — and `unknown` hashes only match bytewise-identical
/// ids, never on hash alone.
pub fn builds_match(local: &str, remote: Option<&str>) -> bool {
    let Some(remote) = remote else { return false };
    if local == remote {
        return true;
    }
    let (l, r) = (build_ref(local), build_ref(remote));
    l == r && !l.is_empty() && !l.starts_with("unknown")
}

/// Optional isolated root for ALL per-user chimaera state. When `CHIMAERA_HOME`
/// is set, the daemon's data/config/runtime dirs live under it instead of the
/// shared `~/.chimaera` — so parallel daemons (git worktrees, CI, a second dev
/// instance) never clobber each other's manifest. Only the daemon's own
/// bookkeeping moves; spawned shells and agents keep the real `$HOME`
/// (dotfiles, `~/.claude` auth). Empty means unset.
///
/// Dev is dev, on both ends: a dev build ([`is_dev_build`]) with no explicit
/// `CHIMAERA_HOME` defaults to `~/.chimaera-dev` — the same home `connect`
/// gives a dev daemon on a remote host — so a bare `cargo run -- serve` (or
/// a dev app launched outside the isolated-rig script) can never adopt,
/// update, or clobber the real `~/.chimaera` daemon's state. Release builds
/// are unaffected.
fn state_home() -> Option<PathBuf> {
    match std::env::var_os("CHIMAERA_HOME") {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ if is_dev_build() => dirs::home_dir().map(|h| h.join(".chimaera-dev")),
        _ => None,
    }
}

/// Resolve the data dir given an optional isolated [`state_home`] (pure; the
/// public [`data_dir`] wraps this and creates the directory).
fn data_dir_in(home: Option<&Path>) -> PathBuf {
    match home {
        Some(home) => home.join("data"),
        None => dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".chimaera"),
    }
}

/// Per-user data directory (`~/.chimaera`, or `$CHIMAERA_HOME/data` when
/// isolated), created on demand.
pub fn data_dir() -> PathBuf {
    let dir = data_dir_in(state_home().as_deref());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("failed to create data dir {}: {e}", dir.display());
    }
    dir
}

/// Per-user config directory (`$XDG_CONFIG_HOME/chimaera`, else
/// `~/.config/chimaera`; `$CHIMAERA_HOME/config` when isolated), created on
/// demand. Holds user-editable files (settings.json) as opposed to
/// daemon-owned state in [`data_dir`].
pub fn config_dir() -> PathBuf {
    let dir = match state_home() {
        Some(home) => home.join("config"),
        None => {
            let base = match std::env::var_os("XDG_CONFIG_HOME") {
                Some(base) if !base.is_empty() => PathBuf::from(base),
                _ => dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".config"),
            };
            base.join("chimaera")
        }
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("failed to create config dir {}: {e}", dir.display());
    }
    dir
}

/// Per-user runtime directory: `$XDG_RUNTIME_DIR/chimaera` if set, else
/// `/tmp/chimaera-$UID` (`$CHIMAERA_HOME/run` when isolated; on Windows
/// `%LOCALAPPDATA%\chimaera\run` — already per-user, no /tmp analogue).
/// Created with mode 0700 on demand where the platform has modes.
pub fn runtime_dir() -> PathBuf {
    let dir = match state_home() {
        Some(home) => home.join("run"),
        None => default_runtime_dir(),
    };
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    if let Err(e) = builder.create(&dir) {
        tracing::warn!("failed to create runtime dir {}: {e}", dir.display());
    }
    dir
}

#[cfg(unix)]
fn default_runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(base) if !base.is_empty() => PathBuf::from(base).join("chimaera"),
        _ => PathBuf::from(format!(
            "/tmp/chimaera-{}",
            nix::unistd::Uid::current().as_raw()
        )),
    }
}

#[cfg(windows)]
fn default_runtime_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("chimaera")
        .join("run")
}

/// The user's real login shell.
///
/// `$SHELL` is authoritative when a terminal set it, but a daemon launched
/// from Finder/Dock/launchd (the packaged app, not a terminal) frequently has
/// no `$SHELL` at all — or a bare `/bin/sh` from a minimal launchd
/// environment — and `/bin/sh` never sources the user's zsh/bash rc where
/// PATH additions like `~/.local/bin` (where `claude.ai/install.sh` lands)
/// live. So an unusable `$SHELL` falls back to the passwd database: the shell
/// the OS itself starts for this user (exactly what Terminal.app reads),
/// then `/bin/sh` as a last resort.
#[cfg(unix)]
pub fn login_shell() -> String {
    let passwd_shell = nix::unistd::User::from_uid(nix::unistd::Uid::current())
        .ok()
        .flatten()
        .map(|u| u.shell.to_string_lossy().into_owned());
    resolve_login_shell(std::env::var("SHELL").ok(), passwd_shell, |p| {
        Path::new(p).is_file()
    })
}

/// Pure core of [`login_shell`] (env + passwd threaded in for testability).
/// `$SHELL` wins when it names an existing shell that isn't the launchd-
/// minimal `/bin/sh`; otherwise the passwd shell; otherwise `/bin/sh`.
#[cfg(unix)]
fn resolve_login_shell(
    shell_env: Option<String>,
    passwd_shell: Option<String>,
    is_file: impl Fn(&str) -> bool,
) -> String {
    if let Some(shell) = shell_env {
        // A launchd-minimal `SHELL=/bin/sh` is not a real user choice — prefer
        // the passwd entry, which names the actual interactive shell.
        if !shell.is_empty() && shell != "/bin/sh" && is_file(&shell) {
            return shell;
        }
    }
    passwd_shell
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".to_string())
}

/// Generate an auth token: 32 random bytes as lowercase hex (64 chars).
pub fn generate_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// On-disk record of a running chimaera daemon.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Manifest {
    pub hostname: String,
    pub port: u16,
    pub token: String,
    pub pid: u32,
    pub version: String,
    pub started_at: u64,
    /// Build id of the daemon binary ([`BUILD_ID`] at `serve` time). `None`
    /// on manifests written by builds that predate build ids — which is
    /// itself the signal: missing = ancient = outdated.
    #[serde(default)]
    pub build: Option<String>,
}

impl Manifest {
    /// Path of the manifest file: `data_dir()/manifest.json`.
    pub fn path() -> PathBuf {
        data_dir().join("manifest.json")
    }

    /// Load the manifest if it exists. `Ok(None)` when the file is absent.
    pub fn load() -> anyhow::Result<Option<Manifest>> {
        let contents = match std::fs::read_to_string(Self::path()) {
            Ok(c) => c,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Ok(Some(serde_json::from_str(&contents)?))
    }

    /// Atomically write the manifest (tmp file + rename), mode 0600.
    pub fn write(&self) -> anyhow::Result<()> {
        let path = Self::path();
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        #[cfg(unix)]
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Remove the manifest file. Ok if it does not exist.
    pub fn remove() -> anyhow::Result<()> {
        match std::fs::remove_file(Self::path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Whether the recorded pid is alive (signal 0 probe). Unix-only by
    /// design: on Windows the pid in a manifest belongs to a process inside
    /// WSL, so liveness must be probed in the distro, never on the host.
    #[cfg(unix)]
    pub fn is_alive(&self) -> bool {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(self.pid as i32), None).is_ok()
    }
}

/// Carryover from a daemon stopped for a *planned* restart (update/replace):
/// the successor rebinds the same port with the same token, so existing ssh
/// forwards stay valid and every attached client heals with a plain WebSocket
/// reconnect — no re-home, no re-auth. Written by whoever drives the restart
/// (the app, `chimaera connect`), consumed exactly once by the next `serve`.
/// A crash never writes one, so unplanned restarts keep fresh credentials.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Handoff {
    pub port: u16,
    pub token: String,
    /// Unix seconds at write time; stale files are ignored (a handoff is only
    /// meaningful for the restart it was written for).
    pub written_at: u64,
}

/// A handoff older than this is orphaned (the planned restart never happened
/// or already consumed a fallback path) and must not resurrect its token.
const HANDOFF_MAX_AGE_SECS: u64 = 120;

impl Handoff {
    /// Path of the handoff file: `data_dir()/handoff.json`.
    pub fn path() -> PathBuf {
        data_dir().join("handoff.json")
    }

    pub fn new(port: u16, token: String) -> Self {
        let written_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Handoff {
            port,
            token,
            written_at,
        }
    }

    /// Atomically write the handoff (tmp + rename), mode 0600 — it carries
    /// the bearer token.
    pub fn write(&self) -> anyhow::Result<()> {
        self.write_at(&Self::path())
    }

    /// Take the pending handoff: read it, delete the file, and return it only
    /// if fresh. Consume-once semantics — even a stale or unparsable file is
    /// removed, so a dead handoff can never linger and leak an old token into
    /// some later boot.
    pub fn consume() -> Option<Handoff> {
        Self::consume_at(&Self::path())
    }

    /// [`Handoff::write`] against an explicit path (pure of [`data_dir`], so
    /// tests need no HOME mutation — the same split as [`data_dir_in`]).
    pub fn write_at(&self, path: &Path) -> anyhow::Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        #[cfg(unix)]
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// [`Handoff::consume`] against an explicit path.
    pub fn consume_at(path: &Path) -> Option<Handoff> {
        let contents = std::fs::read_to_string(path).ok()?;
        if let Err(e) = std::fs::remove_file(path) {
            tracing::warn!("failed to remove {}: {e}", path.display());
        }
        let handoff: Handoff = serde_json::from_str(&contents).ok()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        (now.saturating_sub(handoff.written_at) <= HANDOFF_MAX_AGE_SECS).then_some(handoff)
    }
}

/// Parse `x.y.z` (an optional leading `v` is tolerated — release tags carry
/// one) into a comparable tuple. Anything else is `None`: pre-release or
/// exotic versions never win a comparison by accident.
pub fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.strip_prefix('v').unwrap_or(v);
    let mut parts = v.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
}

/// Whether `latest` denotes a strictly newer release than `current`. Dev
/// builds (the `0.0.1` workspace sentinel — never a published tag) are never
/// "outdated": a dev daemon nagging about GitHub releases is noise.
pub fn release_is_newer(current: &str, latest: &str) -> bool {
    if current == "0.0.1" {
        return false;
    }
    match (parse_version(current), parse_version(latest)) {
        (Some(cur), Some(new)) => new > cur,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dev-build gate rides the release stamping convention: the literal
    /// `0.0.1` sentinel means never-release-stamped; anything else is a
    /// shipped version. `cargo test` always runs on the sentinel, so
    /// `is_dev_build` itself must read true here.
    #[test]
    fn dev_build_is_the_unstamped_sentinel() {
        assert!(version_is_dev("0.0.1"));
        assert!(!version_is_dev("0.15.1"));
        assert!(!version_is_dev("1.0.0"));
        assert!(is_dev_build(), "tests always run on the 0.0.1 sentinel");
    }

    #[cfg(unix)]
    #[test]
    fn login_shell_prefers_real_shell_then_passwd() {
        let exists = |_: &str| true;
        let missing = |_: &str| false;

        // A terminal-set $SHELL that exists wins.
        assert_eq!(
            resolve_login_shell(Some("/bin/zsh".into()), Some("/bin/bash".into()), exists),
            "/bin/zsh"
        );
        // A launchd-minimal `/bin/sh` is ignored in favour of the passwd shell
        // (the "claude vanishes when launched from Finder" fix).
        assert_eq!(
            resolve_login_shell(Some("/bin/sh".into()), Some("/bin/zsh".into()), exists),
            "/bin/zsh"
        );
        // No $SHELL at all (stripped GUI env): passwd shell.
        assert_eq!(
            resolve_login_shell(None, Some("/usr/bin/fish".into()), exists),
            "/usr/bin/fish"
        );
        // A $SHELL that doesn't point at a real file: passwd shell.
        assert_eq!(
            resolve_login_shell(Some("/nope/zsh".into()), Some("/bin/zsh".into()), missing),
            "/bin/zsh"
        );
        // Nothing usable anywhere: /bin/sh, never empty.
        assert_eq!(resolve_login_shell(None, None, exists), "/bin/sh");
        assert_eq!(
            resolve_login_shell(Some(String::new()), Some(String::new()), exists),
            "/bin/sh"
        );
    }

    #[cfg(unix)]
    #[test]
    fn login_shell_resolves_to_a_real_shell() {
        // On the host, it must name a non-empty absolute path (never a panic).
        let shell = login_shell();
        assert!(shell.starts_with('/'), "{shell}");
    }

    #[test]
    fn token_is_64_lowercase_hex_chars() {
        let token = generate_token();
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')));
        assert_ne!(generate_token(), token, "tokens should be random");
    }

    #[test]
    fn data_dir_honors_isolated_home() {
        // CHIMAERA_HOME relocates the daemon's state under its own root...
        let iso = Path::new("/tmp/example-chimaera-home");
        assert_eq!(data_dir_in(Some(iso)), iso.join("data"));
        // ...while the default still resolves to ~/.chimaera.
        assert!(data_dir_in(None).ends_with(".chimaera"));
    }

    // Relies on $HOME relocation and unix file modes.
    #[cfg(unix)]
    #[test]
    fn manifest_round_trip() {
        let tmp_home =
            std::env::temp_dir().join(format!("chimaera-core-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp_home).unwrap();
        std::env::set_var("HOME", &tmp_home);

        assert!(Manifest::load().unwrap().is_none(), "no manifest yet");

        let manifest = Manifest {
            hostname: "testhost".to_string(),
            port: 43210,
            token: generate_token(),
            pid: std::process::id(),
            version: VERSION.to_string(),
            started_at: 1_750_000_000,
            build: Some(BUILD_ID.to_string()),
        };
        manifest.write().unwrap();

        let mode = std::fs::metadata(Manifest::path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "manifest should be chmod 0600");

        let loaded = Manifest::load().unwrap().unwrap();
        assert_eq!(loaded.hostname, manifest.hostname);
        assert_eq!(loaded.port, manifest.port);
        assert_eq!(loaded.token, manifest.token);
        assert_eq!(loaded.pid, manifest.pid);
        assert_eq!(loaded.version, manifest.version);
        assert_eq!(loaded.started_at, manifest.started_at);
        assert_eq!(loaded.build, manifest.build, "build id round-trips");
        assert!(loaded.is_alive(), "our own pid is alive");

        Manifest::remove().unwrap();
        assert!(Manifest::load().unwrap().is_none());
        Manifest::remove().unwrap(); // idempotent

        std::fs::remove_dir_all(&tmp_home).ok();
    }

    /// Manifests written before build ids existed (no `build` field) must
    /// keep parsing — a missing build is data ("ancient"), not an error.
    #[test]
    fn manifest_without_build_field_parses() {
        let old = r#"{
            "hostname": "cluster",
            "port": 9700,
            "token": "abc",
            "pid": 1234,
            "version": "0.0.1",
            "started_at": 1750000000
        }"#;
        let m: Manifest = serde_json::from_str(old).unwrap();
        assert_eq!(m.build, None, "missing field reads as None");
        assert_eq!(m.port, 9700);
    }

    #[test]
    fn build_id_has_source_ref_and_timestamp() {
        let (source, secs) = BUILD_ID.rsplit_once('.').expect("hash.time shape");
        assert!(!source.is_empty());
        assert!(
            secs.parse::<u64>().is_ok(),
            "timestamp is unix secs: {secs}"
        );
        assert_eq!(build_ref(BUILD_ID), source);
    }

    #[test]
    fn version_parse_and_release_comparison() {
        assert_eq!(parse_version("0.6.0"), Some((0, 6, 0)));
        assert_eq!(parse_version("v1.2.30"), Some((1, 2, 30)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("1.2.x"), None);
        assert_eq!(parse_version(""), None);

        assert!(release_is_newer("0.5.0", "v0.6.0"));
        assert!(release_is_newer("0.5.9", "0.5.10"));
        assert!(!release_is_newer("0.6.0", "v0.6.0"));
        assert!(!release_is_newer("0.6.1", "v0.6.0"));
        // Unparsable versions never win.
        assert!(!release_is_newer("0.5.0", "v0.6.0-rc1"));
        // The dev sentinel is never outdated.
        assert!(!release_is_newer("0.0.1", "v99.0.0"));
    }

    #[test]
    fn handoff_consume_once_and_freshness() {
        // Explicit-path variants: no HOME mutation (which would race the
        // manifest test across parallel test threads).
        let dir =
            std::env::temp_dir().join(format!("chimaera-handoff-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("handoff.json");

        assert!(Handoff::consume_at(&path).is_none(), "no handoff yet");

        let h = Handoff::new(43_211, "tok".to_string());
        h.write_at(&path).unwrap();
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "handoff carries the token");
        }
        let taken = Handoff::consume_at(&path).expect("fresh handoff consumed");
        assert_eq!((taken.port, taken.token.as_str()), (43_211, "tok"));
        assert!(Handoff::consume_at(&path).is_none(), "consume-once");

        // Stale handoffs are removed AND rejected.
        let stale = Handoff {
            written_at: 1_000,
            ..Handoff::new(43_212, "old".to_string())
        };
        stale.write_at(&path).unwrap();
        assert!(
            Handoff::consume_at(&path).is_none(),
            "stale handoff rejected"
        );
        assert!(!path.exists(), "stale file removed");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn builds_match_ignores_timestamp_but_not_source() {
        // Same commit, different build moments (native app vs musl dist).
        assert!(builds_match("ff52221.100", Some("ff52221.999")));
        assert!(builds_match("ff52221-dirty.100", Some("ff52221-dirty.999")));
        assert!(builds_match("ff52221.100", Some("ff52221.100")));
        // Different source never matches.
        assert!(!builds_match("ff52221.100", Some("d4e587f.100")));
        assert!(!builds_match("ff52221.100", Some("ff52221-dirty.100")));
        // Missing = pre-build-id = ancient.
        assert!(!builds_match("ff52221.100", None));
        // Non-git builds only match bytewise-identical ids.
        assert!(!builds_match("unknown.100", Some("unknown.999")));
        assert!(builds_match("unknown.100", Some("unknown.100")));
    }
}
