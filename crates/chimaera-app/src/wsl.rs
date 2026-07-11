//! The WSL2 engine: how the Windows shell hosts "local" sessions.
//!
//! On Windows the daemon is never this executable — it is the SAME Linux musl
//! binary every release ships for remote hosts, running inside the user's
//! WSL2 distro. This module owns that lifecycle end to end: detect WSL and
//! its distros (registry-first — see below), provision the daemon binary into
//! the distro, spawn it daemonized so it outlives the shell AND the WSL
//! session that launched it, and probe/adopt/stop it. The UI reaches the
//! daemon over WSL2's NAT localhost forwarding (`127.0.0.1:{port}` on the
//! Windows side), which is why the rest of the shell — window URLs, health
//! checks, tokens — is unchanged from macOS.
//!
//! Design constraints, each backed by docs/windows-wsl-plan.md research:
//!
//! - **Registry before wsl.exe.** On Windows 11 24H2 a bare wsl.exe run with
//!   WSL absent can block 60s on an interactive "Press any key to install"
//!   stub. `HKLM\...\Lxss\Msi` (package installed) and `HKCU\...\Lxss`
//!   (distros) answer the same questions without spawning anything.
//! - **Every wsl.exe spawn is hardened**: `WSL_UTF8=1` (wsl.exe speaks
//!   UTF-16LE otherwise), stdin explicitly wired, CREATE_NO_WINDOW (a GUI
//!   parent pops a console per child without it), and a hard timeout.
//! - **`--exec`, never the user's shell.** `wsl <cmd>` runs `$SHELL -c`,
//!   inheriting fish/nushell/rc-file breakage; `--exec /bin/sh -c '…'` pins
//!   POSIX sh semantics.
//! - **The daemon start line is the Podman pattern** (nohup + full stdio
//!   redirect + settle-sleep, rooted in a wsl.exe session): a daemonized
//!   process keeps the distro instance and utility VM alive indefinitely via
//!   its Windows-side wslhost.exe; a systemd unit would NOT.
//! - **A TCP accept proves nothing.** NAT forwarding fails silently on port
//!   collision (localhost:{port} then reaches some Windows process), so
//!   adoption always requires the manifest-token health handshake.
//!
//! Compiled on every platform: detection/parsing is pure and unit-tested on
//! the dev machine (macOS cannot build the Windows target locally); only the
//! registry reads and process spawns are `cfg(windows)`. On unix the public
//! entry points report [`WslState::Unsupported`] / fail cleanly — the wizard
//! is unreachable there anyway.

use serde::Serialize;

use crate::daemon::LocalDaemon;

/// Setup states the first-run wizard renders. Kebab-case on the wire.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum WslState {
    /// Not a Windows build — the wizard never shows.
    Unsupported,
    /// The WSL package is not installed (needs `wsl --install`, one-time
    /// admin + reboot).
    NotInstalled,
    /// WSL is installed but no usable WSL2 distro is registered.
    NoDistro,
    /// Distros exist but every visible one is WSL1 (needs `--set-version 2`).
    Wsl1Only,
    /// At least one WSL2 distro is ready to host the daemon.
    Ready,
}

/// One registered distro, as the wizard's picker sees it.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DistroInfo {
    pub name: String,
    /// 1 = WSL1 (unusable for us), 2 = WSL2.
    pub version: u32,
    pub is_default: bool,
    /// Utility distros (docker-desktop, …) — hidden from the default picker.
    pub hidden: bool,
}

/// The full detection report the `wsl_status` command returns.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct WslReport {
    pub state: WslState,
    pub distros: Vec<DistroInfo>,
    /// The distro the daemon should land in: the registry default when it is
    /// a visible WSL2 distro, else the first visible WSL2 one.
    pub default_distro: Option<String>,
}

/// Marker error for "the shell cannot have a local daemon yet": startup
/// catches it (via downcast) and opens the setup wizard instead of failing.
#[derive(Debug, Clone)]
pub struct WslNotReady(pub WslReport);

impl std::fmt::Display for WslNotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "WSL is not ready to host the local daemon: {:?}",
            self.0.state
        )
    }
}

impl std::error::Error for WslNotReady {}

/// Implementation-detail distros other products register (VS Code hides the
/// same set). Prefix match: docker-desktop-data, podman-machine-default, ….
#[cfg_attr(not(windows), allow(dead_code))]
const UTILITY_PREFIXES: &[&str] = &["docker-desktop", "rancher-desktop", "podman-machine"];

#[cfg_attr(not(windows), allow(dead_code))]
fn is_utility_distro(name: &str) -> bool {
    UTILITY_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// A distro row as read from the registry, before policy is applied.
#[derive(Clone, Debug)]
#[cfg_attr(not(windows), allow(dead_code))]
struct RawDistro {
    name: String,
    version: u32,
    /// Registry `State`: 1 = installed/ready; anything else is mid
    /// install/convert/uninstall. Missing reads as ready (pre-State WSL).
    ready: bool,
    is_default: bool,
}

/// Pure policy: registry facts → wizard report. Split from the registry read
/// so the state machine is unit-testable off-Windows.
#[cfg_attr(not(windows), allow(dead_code))]
fn build_report(package_installed: bool, raw: Vec<RawDistro>) -> WslReport {
    if !package_installed {
        return WslReport {
            state: WslState::NotInstalled,
            distros: Vec::new(),
            default_distro: None,
        };
    }
    let distros: Vec<DistroInfo> = raw
        .iter()
        .filter(|d| d.ready)
        .map(|d| DistroInfo {
            name: d.name.clone(),
            version: d.version,
            is_default: d.is_default,
            hidden: is_utility_distro(&d.name),
        })
        .collect();
    let visible: Vec<&DistroInfo> = distros.iter().filter(|d| !d.hidden).collect();
    let state = if visible.is_empty() {
        WslState::NoDistro
    } else if !visible.iter().any(|d| d.version == 2) {
        WslState::Wsl1Only
    } else {
        WslState::Ready
    };
    let default_distro = visible
        .iter()
        .find(|d| d.is_default && d.version == 2)
        .or_else(|| visible.iter().find(|d| d.version == 2))
        .map(|d| d.name.clone());
    WslReport {
        state,
        distros,
        default_distro,
    }
}

/// wsl.exe emits UTF-16LE unless `WSL_UTF8=1` — which the legacy inbox WSL
/// ignores. Sniff for NULs and decode accordingly so parsing never sees
/// interleaved zero bytes.
#[cfg_attr(not(windows), allow(dead_code))]
fn decode_wsl_output(bytes: &[u8]) -> String {
    if bytes.iter().take(64).any(|&b| b == 0) {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Everything runs through POSIX sh inside the distro; `$HOME` expansion is
/// the reason these are `sh -c` scripts and not bare `--exec` argv.
#[cfg_attr(not(windows), allow(dead_code))]
mod script {
    /// Podman-pattern daemon start: full stdio redirect is mandatory (an
    /// inherited pipe keeps wsl.exe from returning) and the settle-sleep
    /// guards the fork racing the launching shell's exit. Rooted in a wsl.exe
    /// session so wslhost.exe keeps the VM alive — never systemd.
    pub const START: &str = "mkdir -p \"$HOME/.chimaera/logs\"; \
         setsid nohup \"$HOME/.chimaera/bin/chimaera\" serve \
         </dev/null >> \"$HOME/.chimaera/logs/serve.log\" 2>&1 & sleep 0.2";

    /// Install the daemon binary streamed on stdin. Same staged-rename shape
    /// as the ssh deploy (`chimaera.new` → rename) so a torn transfer never
    /// replaces a working binary.
    pub const INSTALL: &str = "mkdir -p \"$HOME/.chimaera/bin\" && \
         cat > \"$HOME/.chimaera/bin/chimaera.new\" && \
         chmod 755 \"$HOME/.chimaera/bin/chimaera.new\" && \
         mv -f \"$HOME/.chimaera/bin/chimaera.new\" \"$HOME/.chimaera/bin/chimaera\"";

    pub const HAS_BINARY: &str = "test -x \"$HOME/.chimaera/bin/chimaera\"";

    pub const READ_MANIFEST: &str = "cat \"$HOME/.chimaera/manifest.json\" 2>/dev/null";

    pub fn alive(pid: u32) -> String {
        format!("kill -0 {pid} 2>/dev/null")
    }

    pub fn stop(pid: u32) -> String {
        format!("kill -TERM {pid}")
    }
}

// ---------------------------------------------------------------------------
// Windows implementation
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod imp {
    use std::process::Stdio;
    use std::time::Duration;

    use anyhow::{bail, Context};
    use chimaera_core::Manifest;
    use chimaera_remote::Decision;
    use tokio::io::AsyncWriteExt;

    use super::*;

    /// CREATE_NO_WINDOW: a windowless GUI parent otherwise flashes a console
    /// per spawned wsl.exe.
    const NO_WINDOW: u32 = 0x0800_0000;

    const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
    const SCRIPT_TIMEOUT: Duration = Duration::from_secs(30);
    /// Streaming ~25 MB into the distro + chmod can take a moment.
    const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);

    fn wsl_command() -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("wsl.exe");
        // Covers current WSL's own messages; distro output is UTF-8 anyway.
        // Legacy inbox WSL ignores it — decode_wsl_output sniffs for that.
        cmd.env("WSL_UTF8", "1");
        cmd.creation_flags(NO_WINDOW);
        cmd.kill_on_drop(true);
        cmd
    }

    /// Spawn with hard timeout; stdin is nulled unless bytes are supplied.
    async fn run(
        mut cmd: tokio::process::Command,
        stdin_bytes: Option<Vec<u8>>,
        timeout: Duration,
    ) -> anyhow::Result<std::process::Output> {
        cmd.stdin(if stdin_bytes.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let mut child = cmd.spawn().context("failed to spawn wsl.exe")?;
        if let Some(bytes) = stdin_bytes {
            let mut stdin = child.stdin.take().expect("piped stdin");
            stdin.write_all(&bytes).await?;
            // Close so the distro-side `cat` sees EOF.
            drop(stdin);
        }
        tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .context("wsl.exe timed out")?
            .context("failed to collect wsl.exe output")
    }

    /// `wsl -d <distro> --exec /bin/sh -c <script>` — POSIX sh, never the
    /// user's login shell.
    async fn run_script(
        distro: &str,
        script: &str,
        stdin_bytes: Option<Vec<u8>>,
        timeout: Duration,
    ) -> anyhow::Result<std::process::Output> {
        let mut cmd = wsl_command();
        cmd.args(["-d", distro, "--exec", "/bin/sh", "-c", script]);
        run(cmd, stdin_bytes, timeout).await
    }

    /// Registry-first detection: never pokes wsl.exe, so it cannot hit the
    /// 24H2 interactive-install stub or a hung service.
    pub fn detect() -> WslReport {
        use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        use winreg::RegKey;

        const LXSS: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";

        let package_installed = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(format!(r"{LXSS}\Msi"))
            .and_then(|k| k.get_value::<String, _>("InstallLocation"))
            .is_ok();

        let mut raw = Vec::new();
        if let Ok(lxss) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(LXSS) {
            let default_guid: Option<String> = lxss.get_value("DefaultDistribution").ok();
            for guid in lxss.enum_keys().flatten() {
                let Ok(k) = lxss.open_subkey(&guid) else {
                    continue;
                };
                let Ok(name) = k.get_value::<String, _>("DistributionName") else {
                    continue;
                };
                let version: u32 = k.get_value("Version").unwrap_or(2);
                // Missing State predates the value; treat as ready.
                let state: u32 = k.get_value("State").unwrap_or(1);
                raw.push(RawDistro {
                    name,
                    version,
                    ready: state == 1,
                    is_default: default_guid.as_deref() == Some(guid.as_str()),
                });
            }
        }
        build_report(package_installed, raw)
    }

    /// One-time WSL enablement. wsl.exe self-elevates for the component
    /// install, but launching it via an elevated PowerShell keeps the UAC
    /// prompt attributed to a console the user can read, and `--install`
    /// without a distro never silently reboots. A reboot is usually still
    /// REQUIRED afterwards — the wizard says so; there is no headless
    /// no-admin path by design (Docker documents the same).
    pub async fn launch_wsl_install() -> anyhow::Result<()> {
        let mut cmd = tokio::process::Command::new("powershell.exe");
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Process -FilePath wsl.exe -ArgumentList \
             '--install','--no-distribution','--no-launch' -Verb RunAs -Wait",
        ]);
        cmd.creation_flags(NO_WINDOW);
        cmd.kill_on_drop(true);
        cmd.stdin(Stdio::null());
        let out = tokio::time::timeout(Duration::from_secs(600), cmd.output())
            .await
            .context("wsl --install timed out")??;
        if !out.status.success() {
            bail!(
                "wsl --install failed: {}",
                decode_wsl_output(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// Install the default distro (Ubuntu). No elevation needed once the WSL
    /// package exists. Fire-and-forget: the image download runs minutes; the
    /// wizard polls `detect()` until the distro registers.
    pub async fn launch_distro_install() -> anyhow::Result<()> {
        let mut cmd = wsl_command();
        cmd.args(["--install", "-d", "Ubuntu", "--no-launch"]);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        cmd.spawn().context("failed to launch the Ubuntu install")?;
        Ok(())
    }

    /// Manifest → live pid → authenticated health, all three or nothing —
    /// the WSL twin of the unix probe. The health check runs against the
    /// Windows loopback (the NAT forward), which is exactly the path the UI
    /// will use, so passing it proves the forward too.
    async fn probe(distro: &str) -> Option<Manifest> {
        let out = run_script(distro, script::READ_MANIFEST, None, SCRIPT_TIMEOUT)
            .await
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let m: Manifest = serde_json::from_str(decode_wsl_output(&out.stdout).trim()).ok()?;
        let alive = run_script(distro, &script::alive(m.pid), None, SCRIPT_TIMEOUT)
            .await
            .ok()?
            .status
            .success();
        if !alive {
            return None;
        }
        let (port, token) = (m.port, m.token.clone());
        let ok = tokio::task::spawn_blocking(move || crate::daemon::health_ok(port, &token))
            .await
            .unwrap_or(false);
        ok.then_some(m)
    }

    /// Graceful stop, unix-parity semantics: TERM, poll ~10s, never KILL.
    async fn stop(distro: &str, pid: u32) -> anyhow::Result<()> {
        tracing::info!("stopping WSL daemon (pid {pid} in {distro})");
        let out = run_script(distro, &script::stop(pid), None, SCRIPT_TIMEOUT).await?;
        if !out.status.success() {
            bail!("failed to signal pid {pid} in {distro}");
        }
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let alive = run_script(distro, &script::alive(pid), None, SCRIPT_TIMEOUT)
                .await?
                .status
                .success();
            if !alive {
                return Ok(());
            }
        }
        bail!("WSL daemon (pid {pid}) is still running 10s after SIGTERM — refusing to kill -9 it")
    }

    /// Ensure the daemon binary exists in the distro, fetching the release
    /// asset (same cache + sha256 verify as `connect`) and streaming it over
    /// wsl.exe stdin — never 9P, never curl-in-distro.
    async fn ensure_binary(
        distro: &str,
        progress: &(dyn Fn(&str) + Send + Sync),
    ) -> anyhow::Result<()> {
        let present = run_script(distro, script::HAS_BINARY, None, SCRIPT_TIMEOUT)
            .await?
            .status
            .success();
        if present {
            return Ok(());
        }
        progress("downloading");
        let arch_out = run_script(distro, "uname -m", None, SCRIPT_TIMEOUT).await?;
        let arch = decode_wsl_output(&arch_out.stdout).trim().to_string();
        let path = chimaera_remote::fetch_daemon_binary("linux", &arch, &|_| {})
            .await
            .context("failed to fetch the daemon release binary")?;
        progress("installing");
        let bytes = tokio::fs::read(&path).await?;
        let out = run_script(distro, script::INSTALL, Some(bytes), INSTALL_TIMEOUT).await?;
        if !out.status.success() {
            bail!(
                "failed to install the daemon into {distro}: {}",
                decode_wsl_output(&out.stderr).trim()
            );
        }
        Ok(())
    }

    fn attached(m: Manifest, outdated: bool, live_sessions: Option<usize>) -> LocalDaemon {
        LocalDaemon {
            port: m.port,
            token: m.token,
            build: m.build,
            outdated,
            live_sessions,
        }
    }

    /// The Windows `ensure_local_daemon`: adopt a healthy daemon in the
    /// chosen (or default) distro, applying the same build-parity decision
    /// policy as unix/connect; else provision + spawn + poll.
    pub async fn ensure_daemon(
        distro: Option<String>,
        progress: &(dyn Fn(&str) + Send + Sync),
    ) -> anyhow::Result<LocalDaemon> {
        let report = detect();
        let distro = match distro.or_else(|| report.default_distro.clone()) {
            Some(d) if report.state == WslState::Ready => d,
            _ => return Err(anyhow::Error::new(WslNotReady(report))),
        };

        progress("checking");
        if let Some(m) = probe(&distro).await {
            let local_build = chimaera_core::BUILD_ID;
            let sessions = if chimaera_core::builds_match(local_build, m.build.as_deref()) {
                None
            } else {
                crate::daemon::live_session_count(m.port, &m.token).await
            };
            match chimaera_remote::update_decision(local_build, m.build.as_deref(), sessions, false)
            {
                Decision::Reuse => {
                    tracing::info!(
                        "attaching to WSL daemon in {distro} on 127.0.0.1:{}",
                        m.port
                    );
                    return Ok(attached(m, false, sessions));
                }
                Decision::Update => {
                    tracing::info!("WSL daemon build differs and is idle — replacing it");
                    stop(&distro, m.pid).await?;
                }
                Decision::ConnectOutdated => {
                    tracing::warn!(
                        "WSL daemon is an older build with live sessions — attaching as outdated"
                    );
                    return Ok(attached(m, true, sessions));
                }
            }
        }

        ensure_binary(&distro, progress).await?;
        progress("starting");
        let out = run_script(&distro, script::START, None, SCRIPT_TIMEOUT).await?;
        if !out.status.success() {
            bail!(
                "failed to start the daemon in {distro}: {}",
                decode_wsl_output(&out.stderr).trim()
            );
        }
        progress("adopting");
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            if let Some(m) = probe(&distro).await {
                tracing::info!("WSL daemon up in {distro} on 127.0.0.1:{}", m.port);
                return Ok(attached(m, false, None));
            }
        }
        bail!(
            "the daemon did not come up in {distro} within 15s — check \
             ~/.chimaera/logs/serve.log inside the distro"
        )
    }

    /// Explicit update: stop whatever runs (the affordance says what it
    /// ends), then force a fresh provision — the daemon is NOT our exe here,
    /// so "update" means re-fetch the release asset, not respawn-self.
    pub async fn update_daemon(
        progress: &(dyn Fn(&str) + Send + Sync),
    ) -> anyhow::Result<LocalDaemon> {
        let report = detect();
        let Some(distro) = report.default_distro.clone() else {
            return Err(anyhow::Error::new(WslNotReady(report)));
        };
        if let Some(m) = probe(&distro).await {
            if chimaera_core::builds_match(chimaera_core::BUILD_ID, m.build.as_deref()) {
                return Ok(attached(m, false, None));
            }
            stop(&distro, m.pid).await?;
        }
        // Drop the possibly-stale binary so ensure_binary re-fetches.
        let _ = run_script(
            &distro,
            "rm -f \"$HOME/.chimaera/bin/chimaera\"",
            None,
            SCRIPT_TIMEOUT,
        )
        .await;
        ensure_daemon(Some(distro), progress).await
    }
}

// ---------------------------------------------------------------------------
// Non-Windows stubs: the wizard is unreachable, commands answer cleanly.
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn detect() -> WslReport {
        WslReport {
            state: WslState::Unsupported,
            distros: Vec::new(),
            default_distro: None,
        }
    }

    pub async fn launch_wsl_install() -> anyhow::Result<()> {
        anyhow::bail!("WSL setup only applies to the Windows app")
    }

    pub async fn launch_distro_install() -> anyhow::Result<()> {
        anyhow::bail!("WSL setup only applies to the Windows app")
    }

    pub async fn ensure_daemon(
        _distro: Option<String>,
        _progress: &(dyn Fn(&str) + Send + Sync),
    ) -> anyhow::Result<LocalDaemon> {
        anyhow::bail!("WSL hosting only applies to the Windows app")
    }
}

pub use imp::{detect, ensure_daemon, launch_distro_install, launch_wsl_install};
// Only the Windows daemon-update path exists; unix updates its local daemon
// by respawning itself (daemon.rs), never through here.
#[cfg(windows)]
pub use imp::update_daemon;

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(name: &str, version: u32, ready: bool, is_default: bool) -> RawDistro {
        RawDistro {
            name: name.to_string(),
            version,
            ready,
            is_default,
        }
    }

    #[test]
    fn report_states_cover_the_wizard_machine() {
        // No package: nothing else matters.
        assert_eq!(
            build_report(false, vec![raw("Ubuntu", 2, true, true)]).state,
            WslState::NotInstalled
        );
        // Package, no distros.
        assert_eq!(build_report(true, vec![]).state, WslState::NoDistro);
        // Only utility distros: effectively none for us.
        assert_eq!(
            build_report(true, vec![raw("docker-desktop", 2, true, true)]).state,
            WslState::NoDistro
        );
        // Only WSL1 visible.
        assert_eq!(
            build_report(true, vec![raw("Ubuntu-18.04", 1, true, true)]).state,
            WslState::Wsl1Only
        );
        // A mid-install distro (State != 1) is not usable yet.
        assert_eq!(
            build_report(true, vec![raw("Ubuntu", 2, false, true)]).state,
            WslState::NoDistro
        );
        assert_eq!(
            build_report(true, vec![raw("Ubuntu", 2, true, true)]).state,
            WslState::Ready
        );
    }

    #[test]
    fn default_distro_prefers_registry_default_then_first_wsl2() {
        // The registry default wins when it is a visible WSL2 distro.
        let r = build_report(
            true,
            vec![raw("Debian", 2, true, false), raw("Ubuntu", 2, true, true)],
        );
        assert_eq!(r.default_distro.as_deref(), Some("Ubuntu"));
        // A denylisted or WSL1 default falls through to the first usable one.
        let r = build_report(
            true,
            vec![
                raw("docker-desktop", 2, true, true),
                raw("old", 1, true, false),
                raw("Debian", 2, true, false),
            ],
        );
        assert_eq!(r.default_distro.as_deref(), Some("Debian"));
        assert_eq!(r.state, WslState::Ready);
    }

    #[test]
    fn utility_distros_are_hidden_not_dropped() {
        let r = build_report(
            true,
            vec![
                raw("docker-desktop", 2, true, false),
                raw("docker-desktop-data", 2, true, false),
                raw("rancher-desktop", 2, true, false),
                raw("podman-machine-default", 2, true, false),
                raw("Ubuntu", 2, true, true),
            ],
        );
        assert_eq!(r.distros.len(), 5, "hidden entries stay listable");
        assert_eq!(r.distros.iter().filter(|d| d.hidden).count(), 4);
        assert_eq!(r.default_distro.as_deref(), Some("Ubuntu"));
    }

    #[test]
    fn wsl_output_decodes_both_encodings() {
        assert_eq!(decode_wsl_output(b"Ubuntu\n"), "Ubuntu\n");
        // "Hi" as UTF-16LE, the legacy wsl.exe encoding.
        let utf16: &[u8] = &[b'H', 0, b'i', 0];
        assert_eq!(decode_wsl_output(utf16), "Hi");
    }

    /// The start line is the researched persistence pattern — if someone
    /// "simplifies" away setsid/nohup/redirects, the daemon dies with the
    /// shell (or wsl.exe never returns) on real Windows only. Pin it here.
    #[test]
    fn start_script_keeps_the_persistence_pattern() {
        for needle in [
            "setsid nohup",
            "</dev/null",
            ">> \"$HOME/.chimaera/logs/serve.log\" 2>&1 &",
            "sleep 0.2",
        ] {
            assert!(
                script::START.contains(needle),
                "start script lost {needle:?}"
            );
        }
        assert!(
            script::INSTALL.contains("chimaera.new"),
            "install must stage + rename, never write in place"
        );
    }
}

/// Full-engine e2e against a REAL WSL2 distro: provision from the published
/// release, spawn, adopt, exercise the daemon API through the NAT localhost
/// forward (workspace + real PTY session), then survive the ecosystem's #1
/// lifecycle event (`wsl --shutdown`) with a fresh bring-up. Ignored except
/// in .github/workflows/wsl-smoke.yml — it needs a Windows runner with
/// nested virtualization and a registered distro.
#[cfg(all(test, windows))]
mod e2e {
    use super::*;

    #[tokio::test]
    #[ignore = "needs a real WSL2 distro — run via the wsl-smoke workflow"]
    async fn wsl_end_to_end() {
        let report = detect();
        assert_eq!(
            report.state,
            WslState::Ready,
            "the runner must provide a WSL2 distro: {report:?}"
        );
        let progress = |p: &str| eprintln!("[smoke] phase: {p}");

        let local = ensure_daemon(None, &progress).await.expect("ensure daemon");
        eprintln!("[smoke] daemon on 127.0.0.1:{}", local.port);

        // The daemon API through the forward — a real workspace and a real
        // PTY session prove the whole stack, not just a socket accept.
        let auth = format!("Bearer {}", local.token);
        let base = format!("http://127.0.0.1:{}/api/v1", local.port);
        let ws: serde_json::Value = ureq::post(&format!("{base}/workspaces"))
            .set("Authorization", &auth)
            .send_json(serde_json::json!({"root": "/tmp"}))
            .expect("create workspace")
            .into_json()
            .expect("workspace json");
        let ws_id = ws["id"].as_str().expect("workspace id").to_string();
        let session: serde_json::Value = ureq::post(&format!("{base}/sessions"))
            .set("Authorization", &auth)
            .send_json(serde_json::json!({"workspace_id": ws_id, "kind": "shell"}))
            .expect("create session")
            .into_json()
            .expect("session json");
        eprintln!("[smoke] shell session {} is up", session["id"]);

        // Re-ensure must adopt, not respawn (same port = same daemon).
        let again = ensure_daemon(None, &progress).await.expect("re-ensure");
        assert_eq!(again.port, local.port, "second ensure adopted the daemon");

        // wsl --shutdown kills the VM (users run it routinely); the engine
        // must detect the dead daemon and bring a fresh one up.
        let out = std::process::Command::new("wsl.exe")
            .arg("--shutdown")
            .output()
            .expect("wsl --shutdown");
        assert!(out.status.success(), "wsl --shutdown failed");
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let revived = ensure_daemon(None, &progress)
            .await
            .expect("revive after wsl --shutdown");
        eprintln!("[smoke] revived on 127.0.0.1:{}", revived.port);
        assert!(
            crate::daemon::health_ok(revived.port, &revived.token),
            "revived daemon must answer the authenticated health check"
        );
    }
}
