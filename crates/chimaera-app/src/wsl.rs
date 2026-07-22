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
    /// WSL is older than the floor we gate on (`wsl --update` fixes it).
    /// Pre-2.1.1 WSL kills daemonized processes with clock skew after
    /// sleep/resume — the exact failure the version gate exists to prevent
    /// (Docker gates on 2.1.5 the same way).
    NeedsUpdate,
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

/// POSIX optional-variable expansion used by the generated askpass wrapper.
/// Kept pure and cross-platform-tested because macOS cannot compile the
/// surrounding Windows implementation.
#[cfg_attr(not(windows), allow(dead_code))]
fn optional_env(name: &str) -> String {
    format!("${{{name}-}}")
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

    /// Graceful stop as ONE in-distro invocation (each wsl.exe spawn costs
    /// hundreds of ms, so polling from the host would stretch the deadline
    /// several-fold): TERM, then wait up to ~15s, never KILL. Exit 3 =
    /// still alive.
    pub fn stop_and_wait(pid: u32) -> String {
        format!(
            "kill -TERM {pid} 2>/dev/null; i=0; \
             while kill -0 {pid} 2>/dev/null; do \
             i=$((i+1)); [ $i -ge 150 ] && exit 3; sleep 0.1; done"
        )
    }
}

// ---------------------------------------------------------------------------
// Windows implementation
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod imp {
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::time::Duration;

    use anyhow::{bail, Context};
    use chimaera_core::Manifest;
    use chimaera_remote::Decision;
    use serde::{Deserialize, Serialize};
    use tokio::io::AsyncWriteExt;

    use super::*;

    const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
    const SCRIPT_TIMEOUT: Duration = Duration::from_secs(30);
    /// The single-invocation stop script waits up to ~15s in-distro.
    const STOP_TIMEOUT: Duration = Duration::from_secs(25);
    /// Streaming ~25 MB into the distro + chmod can take a moment.
    const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);

    /// The WSL floor: pre-2.1.1 kills daemonized processes with clock skew
    /// after sleep/resume (fixed by the ICTIMESYNCFLAG_SYNC patch).
    const MIN_WSL: (u64, u64, u64) = (2, 1, 1);

    /// The resolved place the daemon lives: distro + PINNED user + $HOME.
    /// The user is pinned because the distro's default can change under us
    /// (Ubuntu's OOBE flips it on first interactive launch) — every spawn
    /// passes `-u` so the daemon's home never silently moves.
    #[derive(Clone, Debug)]
    pub struct Target {
        pub distro: String,
        pub user: String,
        pub home: String,
    }

    /// What survives restarts (wsl.json next to the manifest): which distro/
    /// user the wizard chose, and whether OUR wizard installed a distro whose
    /// first-run user we still owe (Ubuntu with --no-launch registers as
    /// root — OOBE never ran).
    #[derive(Serialize, Deserialize, Default, Clone)]
    struct Persisted {
        #[serde(default)]
        distro: Option<String>,
        #[serde(default)]
        user: Option<String>,
        #[serde(default)]
        pending_first_setup: Option<String>,
    }

    fn persisted_path() -> PathBuf {
        chimaera_core::data_dir().join("wsl.json")
    }

    fn load_persisted() -> Persisted {
        std::fs::read_to_string(persisted_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save_persisted(p: &Persisted) {
        let path = persisted_path();
        let tmp = path.with_extension("json.tmp");
        if let Ok(bytes) = serde_json::to_vec_pretty(p) {
            let _ = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, &path));
        }
    }

    fn wsl_command() -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("wsl.exe");
        // Covers current WSL's own messages; distro output is UTF-8 anyway.
        // Legacy inbox WSL ignores it — decode_wsl_output sniffs for that.
        cmd.env("WSL_UTF8", "1");
        cmd.creation_flags(chimaera_core::CREATE_NO_WINDOW);
        cmd.kill_on_drop(true);
        cmd
    }

    /// What run() feeds the child's stdin: small generated scripts as bytes,
    /// the daemon binary streamed from disk (never a whole-file buffer).
    pub(super) enum Stdin {
        Bytes(Vec<u8>),
        File(PathBuf),
    }

    /// Spawn with a hard deadline over the ENTIRE exchange — including the
    /// stdin streaming, which can wedge exactly like the wait can (a stalled
    /// distro-side reader blocks write_all once the pipe fills). Output is
    /// collected concurrently with the write so a chatty child can't
    /// deadlock the pipe either.
    async fn run(
        mut cmd: tokio::process::Command,
        stdin: Option<Stdin>,
        timeout: Duration,
    ) -> anyhow::Result<std::process::Output> {
        cmd.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let mut child = cmd.spawn().context("failed to spawn wsl.exe")?;
        let mut child_stdin = child.stdin.take();
        let write = async {
            match stdin {
                Some(Stdin::Bytes(bytes)) => {
                    if let Some(mut s) = child_stdin.take() {
                        s.write_all(&bytes).await?;
                    }
                }
                Some(Stdin::File(path)) => {
                    if let Some(mut s) = child_stdin.take() {
                        let mut f = tokio::fs::File::open(&path)
                            .await
                            .with_context(|| format!("open {}", path.display()))?;
                        tokio::io::copy(&mut f, &mut s).await?;
                    }
                }
                None => {}
            }
            // Drop closes the pipe so the distro-side `cat` sees EOF.
            Ok::<(), anyhow::Error>(())
        };
        tokio::time::timeout(timeout, async {
            let (wrote, out) = tokio::join!(write, child.wait_with_output());
            wrote?;
            out.context("failed to collect wsl.exe output")
        })
        .await
        .context("wsl.exe timed out")?
    }

    /// `wsl -d <distro> [-u <user>] --exec /bin/sh -c <script>` — POSIX sh,
    /// never the user's login shell. `user: None` = the distro default (only
    /// used before a target's user is pinned). `pub(super)` for the e2e
    /// smoke's assertions.
    pub(super) async fn run_script(
        distro: &str,
        user: Option<&str>,
        script: &str,
        stdin: Option<Stdin>,
        timeout: Duration,
    ) -> anyhow::Result<std::process::Output> {
        let mut cmd = wsl_command();
        cmd.args(["-d", distro]);
        if let Some(u) = user {
            cmd.args(["-u", u]);
        }
        cmd.args(["--exec", "/bin/sh", "-c", script]);
        run(cmd, stdin, timeout).await
    }

    async fn target_script(
        t: &Target,
        script: &str,
        stdin: Option<Stdin>,
        timeout: Duration,
    ) -> anyhow::Result<std::process::Output> {
        run_script(&t.distro, Some(&t.user), script, stdin, timeout).await
    }

    /// Registry-first detection: never pokes wsl.exe, so it cannot hit the
    /// 24H2 interactive-install stub or a hung service. Read errors are
    /// logged and treated as absence — but never silently, so a
    /// wizard-over-a-live-daemon misdetection is at least diagnosable.
    pub fn detect() -> WslReport {
        use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        use winreg::RegKey;

        const LXSS: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";

        let package_installed = match RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(format!(r"{LXSS}\Msi"))
            .and_then(|k| k.get_value::<String, _>("InstallLocation"))
        {
            Ok(_) => true,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("WSL package registry read failed (treating as absent): {e}");
                }
                false
            }
        };

        let mut raw = Vec::new();
        match RegKey::predef(HKEY_CURRENT_USER).open_subkey(LXSS) {
            Ok(lxss) => {
                let default_guid: Option<String> = lxss.get_value("DefaultDistribution").ok();
                for guid in lxss.enum_keys().flatten() {
                    let Ok(k) = lxss.open_subkey(&guid) else {
                        tracing::warn!("could not open distro registry key {guid}");
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
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                tracing::warn!("WSL distro registry read failed (treating as none): {e}");
            }
            Err(_) => {}
        }
        build_report(package_installed, raw)
    }

    /// detect() plus the async WSL version gate. Only this may flip a Ready
    /// report to NeedsUpdate — the version probe spawns wsl.exe, which is
    /// safe here because the registry already confirmed the package (the
    /// 24H2 stub only bites when WSL is absent).
    pub async fn full_report() -> WslReport {
        let mut report = detect();
        if report.state == WslState::Ready && !wsl_version_ok().await {
            report.state = WslState::NeedsUpdate;
        }
        report
    }

    /// Parse `wsl --version`'s first line (labels are localized; the last
    /// token is the version number). Unparseable or erroring — the legacy
    /// inbox WSL has no --version at all — means "too old".
    async fn wsl_version_ok() -> bool {
        let mut cmd = wsl_command();
        cmd.arg("--version");
        let Ok(out) = run(cmd, None, STATUS_TIMEOUT).await else {
            return false;
        };
        if !out.status.success() {
            return false;
        }
        let text = decode_wsl_output(&out.stdout);
        let Some(v) = text
            .lines()
            .find(|l| !l.trim().is_empty())
            .and_then(|l| l.split_whitespace().last())
        else {
            return false;
        };
        let mut nums = v.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
        let got = (
            nums.next().unwrap_or(0),
            nums.next().unwrap_or(0),
            nums.next().unwrap_or(0),
        );
        if got < MIN_WSL {
            tracing::warn!("WSL {v} is below the {MIN_WSL:?} floor — offering wsl --update");
            return false;
        }
        true
    }

    /// One-time WSL enablement. `-PassThru; exit $p.ExitCode` because
    /// Start-Process -Wait alone returns nothing — powershell would exit 0
    /// even when the ELEVATED wsl.exe failed (virtualization disabled, WSL
    /// servicing errors), looping the user through pointless reboots. A UAC
    /// decline throws and surfaces on stderr. The elevated console's own
    /// stderr is not capturable from an unelevated parent; the exit code is.
    pub async fn launch_wsl_install() -> anyhow::Result<()> {
        let mut cmd = tokio::process::Command::new("powershell.exe");
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$p = Start-Process -FilePath wsl.exe -ArgumentList \
             '--install','--no-distribution','--no-launch' -Verb RunAs -Wait -PassThru; \
             exit $p.ExitCode",
        ]);
        cmd.creation_flags(chimaera_core::CREATE_NO_WINDOW);
        cmd.kill_on_drop(true);
        cmd.stdin(Stdio::null());
        let out = tokio::time::timeout(Duration::from_secs(600), cmd.output())
            .await
            .context("wsl --install timed out")??;
        if !out.status.success() {
            bail!(
                "wsl --install failed (exit {:?}): {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// `wsl --update` for the NeedsUpdate wizard state.
    pub async fn launch_wsl_update() -> anyhow::Result<()> {
        let mut cmd = wsl_command();
        cmd.arg("--update");
        cmd.stdin(Stdio::null());
        let out = run(cmd, None, Duration::from_secs(600)).await?;
        if !out.status.success() {
            bail!(
                "wsl --update failed: {}",
                decode_wsl_output(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// Install the default distro (Ubuntu). No elevation needed once the WSL
    /// package exists. Genuinely fire-and-forget: kill_on_drop must be OFF —
    /// this Child is dropped immediately and the image download runs minutes
    /// while the wizard polls `detect()`. Records that WE installed it, so
    /// the first daemon setup knows to create a real user (with --no-launch
    /// Ubuntu's OOBE never runs and the distro registers with root as its
    /// only user).
    pub async fn launch_distro_install() -> anyhow::Result<()> {
        let mut cmd = tokio::process::Command::new("wsl.exe");
        cmd.env("WSL_UTF8", "1");
        cmd.creation_flags(chimaera_core::CREATE_NO_WINDOW);
        cmd.args(["--install", "-d", "Ubuntu", "--no-launch"]);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        cmd.spawn().context("failed to launch the Ubuntu install")?;
        let mut p = load_persisted();
        p.pending_first_setup = Some("Ubuntu".to_string());
        save_persisted(&p);
        Ok(())
    }

    /// Resolve WHERE the daemon lives — distro, pinned user, home — and
    /// persist the answer so every later launch targets the same place (the
    /// wizard's pick must survive restarts, and the registry default must
    /// not silently win over it).
    async fn resolve_target(preferred: Option<String>) -> anyhow::Result<Target> {
        let report = full_report().await;
        if report.state != WslState::Ready {
            return Err(anyhow::Error::new(WslNotReady(report)));
        }
        let mut persisted = load_persisted();
        let persisted_distro = persisted
            .distro
            .clone()
            // A persisted distro that was since unregistered must not wedge
            // startup — fall through to the default.
            .filter(|d| {
                report
                    .distros
                    .iter()
                    .any(|i| i.name == *d && i.version == 2)
            });
        let Some(distro) = preferred
            .or(persisted_distro)
            .or_else(|| report.default_distro.clone())
        else {
            return Err(anyhow::Error::new(WslNotReady(report)));
        };

        let user = match persisted
            .user
            .clone()
            .filter(|_| persisted.distro.as_deref() == Some(distro.as_str()))
        {
            Some(u) => u,
            None => {
                let out = run_script(&distro, None, "id -un", None, SCRIPT_TIMEOUT).await?;
                if !out.status.success() {
                    bail!(
                        "could not resolve the default user in {distro}: {}",
                        decode_wsl_output(&out.stderr).trim()
                    );
                }
                let mut user = decode_wsl_output(&out.stdout).trim().to_string();
                // A wizard-installed Ubuntu registered as root (no OOBE):
                // create the real account once, make it the distro default,
                // and use it — otherwise everything lands root-owned and is
                // orphaned the moment OOBE later flips the default user.
                if user == "root" && persisted.pending_first_setup.as_deref() == Some(&distro) {
                    user = create_default_user(&distro).await?;
                }
                persisted.pending_first_setup = None;
                user
            }
        };

        let home_out = target_probe_home(&distro, &user).await?;
        let target = Target {
            distro: distro.clone(),
            user: user.clone(),
            home: home_out,
        };
        persisted.distro = Some(distro);
        persisted.user = Some(user);
        save_persisted(&persisted);
        Ok(target)
    }

    async fn target_probe_home(distro: &str, user: &str) -> anyhow::Result<String> {
        let out = run_script(distro, Some(user), "echo \"$HOME\"", None, SCRIPT_TIMEOUT).await?;
        let home = decode_wsl_output(&out.stdout).trim().to_string();
        if !out.status.success() || home.is_empty() || !home.starts_with('/') {
            bail!("could not resolve $HOME for {user} in {distro}");
        }
        Ok(home)
    }

    /// Create the wizard-installed distro's first real account: named after
    /// the Windows user (sanitized to a safe charset — it is interpolated
    /// into sh), default shell bash, sudo where the group exists, and made
    /// the distro default via wsl.conf (which needs a distro restart to
    /// apply — hence the --terminate).
    async fn create_default_user(distro: &str) -> anyhow::Result<String> {
        let name = default_username();
        tracing::info!("creating user {name} in wizard-installed {distro}");
        let script = format!(
            "useradd -m -s /bin/bash {name} 2>/dev/null || true; \
             usermod -aG sudo {name} 2>/dev/null || true; \
             printf '[user]\\ndefault={name}\\n' >> /etc/wsl.conf && id -u {name}"
        );
        let out = run_script(distro, Some("root"), &script, None, SCRIPT_TIMEOUT).await?;
        if !out.status.success() {
            bail!(
                "could not create user {name} in {distro}: {}",
                decode_wsl_output(&out.stderr).trim()
            );
        }
        // Apply the new default (and drop the root session state).
        let mut cmd = wsl_command();
        cmd.args(["--terminate", distro]);
        let _ = run(cmd, None, STATUS_TIMEOUT).await;
        Ok(name)
    }

    /// The Windows username as a valid Linux account name; whitelist
    /// charset so it can never break the shell it is interpolated into.
    fn default_username() -> String {
        let raw = std::env::var("USERNAME").unwrap_or_default().to_lowercase();
        let name: String = raw
            .chars()
            .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-' || *c == '_')
            .collect();
        let name = name.trim_start_matches(['-', '_']).to_string();
        if name.is_empty() || !name.chars().next().is_some_and(|c| c.is_ascii_lowercase()) {
            "chimaera".to_string()
        } else {
            name
        }
    }

    /// Manifest → authenticated health, both or nothing. Health runs against
    /// the Windows loopback (the NAT forward) — exactly the path the UI uses
    /// — and doubles as the liveness check (a 200 from the token handshake
    /// proves the daemon better than any kill -0 could, without the extra
    /// wsl.exe spawn per poll).
    async fn probe(t: &Target) -> Option<Manifest> {
        let out = target_script(t, script::READ_MANIFEST, None, SCRIPT_TIMEOUT)
            .await
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let m: Manifest = serde_json::from_str(decode_wsl_output(&out.stdout).trim()).ok()?;
        health(&m).await.then_some(m)
    }

    async fn health(m: &Manifest) -> bool {
        let (port, token) = (m.port, m.token.clone());
        tokio::task::spawn_blocking(move || crate::daemon::health_ok(port, &token))
            .await
            .unwrap_or(false)
    }

    /// Graceful stop: one in-distro TERM+wait invocation, never KILL.
    async fn stop(t: &Target, pid: u32) -> anyhow::Result<()> {
        tracing::info!("stopping WSL daemon (pid {pid} in {})", t.distro);
        let out = target_script(t, &script::stop_and_wait(pid), None, STOP_TIMEOUT).await?;
        match out.status.code() {
            Some(0) => Ok(()),
            Some(3) => bail!(
                "WSL daemon (pid {pid}) is still running 15s after SIGTERM — refusing to \
                 kill -9 it; something is keeping it busy (open browser tabs hold its \
                 sockets). Close them and retry.",
            ),
            _ => bail!(
                "could not stop pid {pid} in {}: {}",
                t.distro,
                decode_wsl_output(&out.stderr).trim()
            ),
        }
    }

    /// Download the release daemon for the distro's arch into the local
    /// cache (same assets + sha256 verify as `connect`) WITHOUT touching the
    /// running daemon — fetch-before-stop, so a failed download never
    /// strands the distro with nothing running.
    async fn fetch_binary(t: &Target) -> anyhow::Result<PathBuf> {
        let arch_out = target_script(t, "uname -m", None, SCRIPT_TIMEOUT).await?;
        let arch = decode_wsl_output(&arch_out.stdout).trim().to_string();
        chimaera_remote::fetch_release_binary("linux", &arch, &|_| {})
            .await
            .context("failed to fetch the daemon release binary")
    }

    /// Stream `staged` (the verified cache file) into the distro and install
    /// it atomically (staged name + rename).
    async fn install_binary(t: &Target, staged: &std::path::Path) -> anyhow::Result<()> {
        let out = target_script(
            t,
            script::INSTALL,
            Some(Stdin::File(staged.to_path_buf())),
            INSTALL_TIMEOUT,
        )
        .await?;
        if !out.status.success() {
            bail!(
                "failed to install the daemon into {}: {}",
                t.distro,
                decode_wsl_output(&out.stderr).trim()
            );
        }
        Ok(())
    }

    async fn has_binary(t: &Target) -> anyhow::Result<bool> {
        Ok(target_script(t, script::HAS_BINARY, None, SCRIPT_TIMEOUT)
            .await?
            .status
            .success())
    }

    /// Start the daemon and adopt it: poll the manifest until it parses
    /// (a spawn per try, usually one), then poll ONLY the spawn-free health
    /// check on the forwarded loopback.
    async fn start_and_adopt(t: &Target) -> anyhow::Result<LocalDaemon> {
        let out = target_script(t, script::START, None, SCRIPT_TIMEOUT).await?;
        if !out.status.success() {
            bail!(
                "failed to start the daemon in {}: {}",
                t.distro,
                decode_wsl_output(&out.stderr).trim()
            );
        }
        let mut manifest: Option<Manifest> = None;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            if manifest.is_none() {
                let out = target_script(t, script::READ_MANIFEST, None, SCRIPT_TIMEOUT).await?;
                if out.status.success() {
                    manifest = serde_json::from_str(decode_wsl_output(&out.stdout).trim()).ok();
                }
            }
            let healthy = match &manifest {
                Some(m) => health(m).await,
                None => false,
            };
            if healthy {
                let m = manifest.take().expect("checked above");
                tracing::info!("WSL daemon up in {} on 127.0.0.1:{}", t.distro, m.port);
                return Ok(crate::daemon::attached(m, false, None));
            }
        }
        bail!(
            "the daemon did not come up in {} within 15s — check \
             ~/.chimaera/logs/serve.log inside the distro",
            t.distro
        )
    }

    /// Adopt-only: attach to a HEALTHY daemon (same build-parity policy as
    /// unix), or report why the shell must go through the wizard instead —
    /// anything that needs provisioning/replacing runs there, visibly, not
    /// invisibly inside a window-less startup.
    pub async fn adopt_daemon() -> anyhow::Result<LocalDaemon> {
        let target = resolve_target(None).await?;
        let Some(m) = probe(&target).await else {
            return Err(anyhow::Error::new(WslNotReady(full_report().await)));
        };
        let local_build = chimaera_core::BUILD_ID;
        let sessions = if chimaera_core::builds_match(local_build, m.build.as_deref()) {
            None
        } else {
            crate::daemon::live_session_count(m.port, &m.token).await
        };
        let local = match chimaera_remote::update_decision(
            local_build,
            m.build.as_deref(),
            sessions,
            false,
        ) {
            Decision::Reuse => {
                tracing::info!(
                    "attaching to WSL daemon in {} on 127.0.0.1:{}",
                    target.distro,
                    m.port
                );
                crate::daemon::attached(m, false, sessions)
            }
            // Replacing is provisioning work — the wizard shows it.
            Decision::Update => return Err(anyhow::Error::new(WslNotReady(full_report().await))),
            Decision::ConnectOutdated => {
                tracing::warn!(
                    "WSL daemon is an older build with live sessions — attaching as outdated"
                );
                crate::daemon::attached(m, true, sessions)
            }
        };
        wire_connect(&target).await;
        Ok(local)
    }

    /// The full ensure: adopt a healthy same-build daemon, REPLACE a
    /// different-build one (fetch first, then stop, then install — a failed
    /// download must never stop a working daemon), or provision from
    /// nothing. `force_replace` is the explicit update affordance: it
    /// replaces regardless of live sessions (the button says what it ends).
    pub async fn ensure_daemon(
        distro: Option<String>,
        force_replace: bool,
        progress: &(dyn Fn(&str) + Send + Sync),
    ) -> anyhow::Result<LocalDaemon> {
        let target = resolve_target(distro).await?;
        progress("checking");
        if let Some(m) = probe(&target).await {
            let local_build = chimaera_core::BUILD_ID;
            let sessions = if chimaera_core::builds_match(local_build, m.build.as_deref()) {
                None
            } else {
                crate::daemon::live_session_count(m.port, &m.token).await
            };
            match chimaera_remote::update_decision(
                local_build,
                m.build.as_deref(),
                sessions,
                force_replace,
            ) {
                Decision::Reuse => {
                    wire_connect(&target).await;
                    return Ok(crate::daemon::attached(m, false, sessions));
                }
                Decision::Update => {
                    tracing::info!("replacing the WSL daemon (build differs)");
                    progress("downloading");
                    let staged = fetch_binary(&target).await?;
                    stop(&target, m.pid).await?;
                    progress("installing");
                    install_binary(&target, &staged).await?;
                }
                Decision::ConnectOutdated => {
                    wire_connect(&target).await;
                    return Ok(crate::daemon::attached(m, true, sessions));
                }
            }
        } else if !has_binary(&target).await? {
            progress("downloading");
            let staged = fetch_binary(&target).await?;
            progress("installing");
            install_binary(&target, &staged).await?;
        }
        progress("starting");
        let local = start_and_adopt(&target).await?;
        wire_connect(&target).await;
        Ok(local)
    }

    /// Explicit update affordance: replace regardless of session count.
    pub async fn update_daemon(
        progress: &(dyn Fn(&str) + Send + Sync),
    ) -> anyhow::Result<LocalDaemon> {
        ensure_daemon(None, true, progress).await
    }

    /// Wire remote-host support through the target (idempotent, re-run on
    /// every successful adopt/ensure). The chimaera-remote transport is set
    /// FIRST — it only needs facts we already have — so a later askpass
    /// hiccup degrades exactly like unix (key/agent hosts still work,
    /// password hosts fail cleanly) instead of silently falling back to
    /// Win32-OpenSSH. Failures are warned, never fatal: remote wiring must
    /// not take local sessions down.
    async fn wire_connect(t: &Target) {
        chimaera_remote::set_wsl_transport(Some(chimaera_remote::WslTransport {
            distro: t.distro.clone(),
            user: t.user.clone(),
            home: t.home.clone(),
        }));
        // The ControlMaster socket dir is the TRANSPORT's need, not the
        // askpass relay's: key/agent-auth hosts must work even when the
        // relay is absent, and ssh creates sockets but never their dir.
        match target_script(t, "mkdir -p \"$HOME/.chimaera/cm\"", None, SCRIPT_TIMEOUT).await {
            Ok(out) if out.status.success() => {}
            Ok(out) => tracing::warn!(
                "could not create the ssh control dir in {} — remote connects will fail: {}",
                t.distro,
                decode_wsl_output(&out.stderr).trim()
            ),
            Err(e) => tracing::warn!("could not create the ssh control dir: {e:#}"),
        }
        if let Err(e) = wire_askpass(t).await {
            tracing::warn!(
                "askpass wiring through {} failed — password/2FA hosts will fail cleanly: {e:#}",
                t.distro
            );
        }
    }

    /// Install the distro-side SSH_ASKPASS wrapper and export the env that
    /// crosses into WSL via WSLENV.
    async fn wire_askpass(t: &Target) -> anyhow::Result<()> {
        let Some((port, token)) = crate::askpass::relay_endpoint() else {
            bail!("askpass relay not listening");
        };
        let exe = std::env::current_exe().context("resolve current executable")?;
        let exe_wsl = chimaera_remote::windows_path_as_wsl(&exe.to_string_lossy());
        if !exe_wsl.starts_with("/mnt/") {
            // UNC or otherwise un-translatable: the distro cannot exec it.
            bail!("app path {exe_wsl:?} is not reachable from inside WSL");
        }
        // The exe path is user-controlled (profile dir) — quote-escape it
        // for the single-quoted sh assignment (O'Brien is a legal Windows
        // user name). Port/token are u16/hex and safe by construction.
        let exe_quoted = exe_wsl.replace('\'', r"'\''");
        // The prompt travels on stdin end to end (ssh → script arg → helper
        // stdin): interop's Linux-argv→Windows-cmdline marshaling is
        // unverified for arbitrary prompt text, so only fixed tokens ride
        // argv. Missing exe / disabled interop exits 0 with no output — ssh
        // gets an empty answer and fails cleanly.
        let askpass_alias = optional_env(chimaera_remote::ASKPASS_ALIAS_ENV);
        let scope_frame = crate::askpass::SCOPE_FRAME;
        let wrapper = format!(
            "#!/bin/sh\n\
             # chimaera ssh-askpass relay (generated at app launch; do not edit).\n\
             exe='{exe_quoted}'\n\
             [ -x \"$exe\" ] || exit 0\n\
             {{ printf '%s %s\\n' '{port}' '{token}'; \
                printf '%s\\n' '{scope_frame}'; \
                printf '%s\\n' \"{askpass_alias}\"; \
                printf '%s' \"$1\"; }} | \"$exe\" --askpass\n"
        );
        let out = target_script(
            t,
            "cat > \"$HOME/.chimaera/askpass.sh\" && \
             chmod 700 \"$HOME/.chimaera/askpass.sh\"",
            Some(Stdin::Bytes(wrapper.into_bytes())),
            SCRIPT_TIMEOUT,
        )
        .await?;
        if !out.status.success() {
            bail!(
                "failed to install the askpass wrapper: {}",
                decode_wsl_output(&out.stderr).trim()
            );
        }
        // Values are distro-side paths/strings passed verbatim — no /p
        // path-translation flags. ssh children of every wsl.exe we spawn
        // inherit these through WSLENV. (set_var is fine here: Windows
        // SetEnvironmentVariableW is OS-synchronized, and the values are
        // idempotent across re-wires.)
        std::env::set_var("SSH_ASKPASS", format!("{}/.chimaera/askpass.sh", t.home));
        std::env::set_var("SSH_ASKPASS_REQUIRE", "force");
        extend_wslenv(&[
            "SSH_ASKPASS",
            "SSH_ASKPASS_REQUIRE",
            chimaera_remote::ASKPASS_ALIAS_ENV,
        ]);
        Ok(())
    }

    /// Add `vars` to `WSLENV` (colon-separated) so they cross into every
    /// wsl.exe child, preserving whatever was already listed.
    fn extend_wslenv(vars: &[&str]) {
        let mut parts: Vec<String> = std::env::var("WSLENV")
            .map(|v| {
                v.split(':')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        for v in vars {
            let listed = parts
                .iter()
                .any(|p| p == v || p.starts_with(&format!("{v}/")));
            if !listed {
                parts.push(v.to_string());
            }
        }
        std::env::set_var("WSLENV", parts.join(":"));
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

    pub async fn full_report() -> WslReport {
        detect()
    }

    pub async fn launch_wsl_install() -> anyhow::Result<()> {
        anyhow::bail!("WSL setup only applies to the Windows app")
    }

    pub async fn launch_wsl_update() -> anyhow::Result<()> {
        anyhow::bail!("WSL setup only applies to the Windows app")
    }

    pub async fn launch_distro_install() -> anyhow::Result<()> {
        anyhow::bail!("WSL setup only applies to the Windows app")
    }

    pub async fn ensure_daemon(
        _distro: Option<String>,
        _force_replace: bool,
        _progress: &(dyn Fn(&str) + Send + Sync),
    ) -> anyhow::Result<LocalDaemon> {
        anyhow::bail!("WSL hosting only applies to the Windows app")
    }
}

// `detect` is only called from the windows imp + the e2e smoke; the unix
// build re-exports it solely so the surface is identical on both platforms.
#[cfg_attr(not(windows), allow(unused_imports))]
pub use imp::{
    detect, ensure_daemon, full_report, launch_distro_install, launch_wsl_install,
    launch_wsl_update,
};
// Only the Windows shell adopts/updates a WSL daemon; unix owns its local
// daemon in daemon.rs, never through here.
#[cfg(windows)]
pub use imp::{adopt_daemon, update_daemon};

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
    fn optional_env_uses_posix_unset_safe_expansion() {
        assert_eq!(
            optional_env(chimaera_remote::ASKPASS_ALIAS_ENV),
            "${CHIMAERA_ASKPASS_ALIAS-}"
        );
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
        let report = full_report().await;
        assert_eq!(
            report.state,
            WslState::Ready,
            "the runner must provide a modern WSL2 distro: {report:?}"
        );
        let progress = |p: &str| eprintln!("[smoke] phase: {p}");

        let local = ensure_daemon(None, false, &progress)
            .await
            .expect("ensure daemon");
        eprintln!("[smoke] daemon on 127.0.0.1:{}", local.port);

        // The daemon API through the forward — a real workspace and a real
        // PTY session prove the whole stack, not just a socket accept.
        let auth = format!("Bearer {}", local.token);
        let base = format!("http://127.0.0.1:{}/api/v1", local.port);
        let ws: serde_json::Value = crate::http::agent()
            .post(&format!("{base}/workspaces"))
            .header("Authorization", &auth)
            .send_json(serde_json::json!({"root": "/tmp"}))
            .expect("create workspace")
            .body_mut()
            .read_json()
            .expect("workspace json");
        let ws_id = ws["id"].as_str().expect("workspace id").to_string();
        let session: serde_json::Value = crate::http::agent()
            .post(&format!("{base}/sessions"))
            .header("Authorization", &auth)
            .send_json(serde_json::json!({"workspace_id": ws_id, "kind": "shell"}))
            .expect("create session")
            .body_mut()
            .read_json()
            .expect("session json");
        eprintln!("[smoke] shell session {} is up", session["id"]);

        // Re-ensure must adopt, not respawn (same port = same daemon), and
        // so must the startup-path adopt-only probe.
        let again = ensure_daemon(None, false, &progress)
            .await
            .expect("re-ensure");
        assert_eq!(again.port, local.port, "second ensure adopted the daemon");
        let adopted = adopt_daemon().await.expect("adopt-only attach");
        assert_eq!(
            adopted.port, local.port,
            "adopt_daemon attached, not respawned"
        );

        // wire_connect ran on the ensure path: the ControlMaster socket dir
        // for the ssh-in-WSL transport must exist in the distro. (The full
        // askpass interop chain needs the installed app exe — real-hardware
        // territory, not reproducible from a test binary.)
        let report = detect();
        let distro = report.default_distro.expect("ready distro");
        let cm = imp::run_script(
            &distro,
            None,
            "test -d \"$HOME/.chimaera/cm\"",
            None,
            std::time::Duration::from_secs(30),
        )
        .await
        .expect("check cm dir");
        assert!(cm.status.success(), "connect wiring created ~/.chimaera/cm");

        // wsl --shutdown kills the VM (users run it routinely); the engine
        // must detect the dead daemon and bring a fresh one up.
        let out = std::process::Command::new("wsl.exe")
            .arg("--shutdown")
            .output()
            .expect("wsl --shutdown");
        assert!(out.status.success(), "wsl --shutdown failed");
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let revived = ensure_daemon(None, false, &progress)
            .await
            .expect("revive after wsl --shutdown");
        eprintln!("[smoke] revived on 127.0.0.1:{}", revived.port);
        assert!(
            crate::daemon::health_ok(revived.port, &revived.token),
            "revived daemon must answer the authenticated health check"
        );
    }
}
