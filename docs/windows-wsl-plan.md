# Windows via WSL2 — implementation plan

A native Windows chimaera app **without porting the daemon**: the same Tauri shell
(real windows, WebView2, NSIS installer, auto-updater) drives the **unmodified
Linux musl daemon** inside the user's WSL2 distro. This is the VS Code
Remote-WSL / Podman-machine architecture, chosen so the Unix core (PTYs, shells,
signals, ControlMaster ssh) stays single-platform — the Windows-specific surface
is a thin launch-and-transport layer in the shell.

> **Status: M0 (portability gates, three-platform CI, Linux bundles), M1
> (the WSL engine + first-run wizard + the wsl-smoke live gate) and M2
> (connect's ssh-in-WSL transport + the interop askpass relay) are
> implemented on this branch. The site holds back the Windows download until
> a real-hardware pass over the wizard + askpass chain.**
> Every claim below was researched 2026-07-10
> against official sources (Microsoft Learn, the open-sourced `microsoft/WSL`
> repo — including its shipping source code — OpenSSH sources, Tauri docs,
> tauri-action issues) because we develop on macOS and cannot test Windows
> locally. Claims are tagged: **[OFFICIAL]** verified against official
> docs/source, **[MULTI]** multiple independent credible sources,
> **[COMMUNITY]** field reports, **[UNVERIFIED]** needs a live Windows check
> (all collected in the [risk register](#risk-register) with their mitigation).

## Architecture

```
Windows                                 │  WSL2 (user's distro, e.g. Ubuntu)
                                        │
chimaera.exe (Tauri shell)              │   ~/.chimaera/bin/chimaera  (musl daemon,
 ├─ WebView2 windows ──────── HTTP/WS ──┼──▶  same release artifact as HPC hosts)
 │   http://127.0.0.1:{port}/#token=…   │      ├─ PTY sessions (zsh/bash, agents)
 │   (WSL NAT localhost forwarding)     │      ├─ ssh ControlMaster (connect)
 ├─ wsl.exe --exec … ── spawn/adopt/────┼──▶   └─ manifest.json, JSONL state
 │   provision/stop (stdio channel)     │
 └─ askpass TCP listener (loopback) ◀───┼── SSH_ASKPASS wrapper (interop exec
     127.0.0.1, token-authenticated     │    of chimaera.exe --askpass)
```

Three channels, each on the mechanism proven by an existing product:

1. **UI traffic** (HTTP/WS from WebView2) rides WSL2's NAT **localhost
   forwarding** — a 127.0.0.1-only bind inside WSL is forwarded to Windows
   127.0.0.1 [OFFICIAL: `.wslconfig localhostForwarding` default true, "wildcard
   or localhost"; mechanism read from `src/linux/init/localhost.cpp`].
2. **Control** (detect, provision, spawn, adopt, stop, port/token discovery)
   rides **`wsl.exe` stdio** — the channel VS Code standardized on precisely
   because TCP into the VM breaks on sleep/network changes [OFFICIAL:
   vscode-remote `wslExeProxy` default].
3. **Askpass** (in-app SSH password/Duo prompts for `connect`) rides **WSL
   interop**: ssh-in-WSL execs a Linux wrapper script → wrapper execs the
   Windows exe (`/mnt/c/...`) → the Windows helper talks to the shell over
   Windows-side loopback TCP. Never WSL→Windows TCP (blocked by the default
   firewall on NAT setups [MULTI: microsoft/WSL #4139/#4585]).

## Locked decisions and their evidence

### D1. The daemon persists — sessions survive closing the app

A daemonized Linux process launched through a `wsl.exe` session keeps its distro
instance and the WSL VM alive **indefinitely**: `wslhost.exe` "takes over the
lifetime of the Linux process" when wsl.exe exits, and the instance/VM idle
timers (15 s / 60 s defaults) never fire while a client-rooted process runs
[OFFICIAL: WSL technical docs `wslhost.exe.md`, `WslCoreConfig.h`,
`LxssUserSession.cpp`]. The launch pattern is Podman machine's, verbatim shape
[OFFICIAL: `containers/podman pkg/machine/wsl/declares.go`]:

```
wsl.exe -d <distro> -u <user> --exec /bin/sh -c \
  'mkdir -p ~/.chimaera/logs; setsid nohup ~/.chimaera/bin/chimaera serve \
   </dev/null >> ~/.chimaera/logs/serve.log 2>&1 & sleep 0.2'
```

— which is character-for-character the shape `chimaera-remote::start_remote`
already runs over ssh (`crates/chimaera-remote/src/lib.rs:1092`). Constraints
that came with the evidence:

- **Never a systemd unit** — systemd services do NOT keep the instance alive
  [OFFICIAL: MS devblog + microsoft/WSL #10138].
- **Never spawn wsl.exe from a Windows service** — WSL is unusable from
  Session 0 [OFFICIAL: microsoft/WSL #9231, known-issues list].
- Redirect *all* stdio (an inherited pipe keeps wsl.exe from returning) and
  launch by absolute path with explicit env — `wsl <cmd>` runs `$SHELL -c`
  non-login; `--exec` bypasses the shell entirely [OFFICIAL: `WslClient.cpp`].
- Processes survive Windows sleep/resume; clock-skew after resume was fixed in
  WSL 2.1.1 [OFFICIAL: WSL release notes, #10006] → **gate on WSL ≥ 2.1.1**
  (Docker gates on 2.1.5 the same way). Hibernate has open wedge reports
  [COMMUNITY: #8696] — covered by the recovery UX (D3).

### D2. Reuse the remote-deploy machinery for WSL provisioning

Installing the daemon into the distro is the existing `connect` deploy flow with
`wsl.exe` replacing ssh as the command transport: same release asset
(`chimaera-x86_64-unknown-linux-musl`), same version resolution
(`releases/tags/v{VERSION}` → `latest` fallback), same `~/.chimaera/bin`
layout, same start string (`lib.rs:790–1115`). Binary transfer streams over
wsl.exe **stdin** — binary-safe by design (Microsoft ships `wsl --import … -`
reading tar/VHD from stdin) [OFFICIAL] — followed by explicit `chmod 755`;
never via the `\\wsl.localhost` 9P share (flaky, undocumented exec-bit
semantics [UNVERIFIED]) and never assuming curl/wget exist in the distro.

### D3. Failure UX: `wsl --shutdown` and forward-death are normal events

- Users run `wsl --shutdown` routinely (the docs prescribe it for every config
  change); it is the #1 documented lifecycle failure for Docker Desktop
  [OFFICIAL: docker/for-win #13917]. The shell health-checks on window focus /
  Windows resume, and shows detect → one-click "Restart daemon" (Docker's UX,
  Podman's mechanics). Sessions die like a host reboot — the session ledger
  restores the workspace shape, PTY scrollback is lost. Same semantics as a
  laptop reboot today.
- NAT localhost forwarding **breaks** after sleep/resume/VPN changes in
  well-documented cases; universal fix is `wsl --shutdown` [MULTI: #5317,
  #4992, #12747]. Because a dead forward can also be a **silent port collision**
  (if a Windows process holds the port, `localhost:<port>` reaches *that
  process* with no error to the Linux side [MULTI: relay source + community
  docs]), the shell must never trust a TCP accept: adoption requires the
  authenticated `/api/v1/health` handshake (which we already do —
  `crates/chimaera-app/src/daemon.rs:146`). Recovery path: re-probe over the
  wsl.exe stdio channel (which survives), re-discover the port, offer
  "Restart WSL networking" (`wsl --shutdown` + respawn) as the last resort.
- Future hardening, only if field reports demand it: a Windows-side TCP relay
  (shell listens on Windows loopback, pipes to the daemon over wsl.exe stdio) —
  VS Code's endgame. Not in scope for v1.

### D4. Detection is registry-first; every wsl.exe spawn is hardened

Read state without spawning anything [OFFICIAL: registry layout verified in
`DistributionRegistration.cpp` — the same keys the WSL service itself reads]:

- `HKLM\Software\Microsoft\Windows\CurrentVersion\Lxss\Msi\InstallLocation` —
  the modern WSL package is installed.
- `HKCU\...\Lxss\{GUID}` subkeys — per-distro `DistributionName`, `Version`
  (1|2 — **require 2**), `State` (1 = ready), plus `DefaultDistribution` GUID.
- Filter utility distros by prefix denylist: `docker-desktop`,
  `rancher-desktop`, `podman-machine` (VS Code's exact approach [OFFICIAL:
  `terminalProfiles.ts`]), with a "show all" escape hatch.

Then confirm with hardened spawns. **Every** wsl.exe invocation gets:
`WSL_UTF8=1` in env (wsl.exe emits UTF-16LE otherwise; the var covers stdout+
stderr since WSL 0.64.0 [OFFICIAL]), stdin explicitly null/piped, all stdio
handles set, `creation_flags(CREATE_NO_WINDOW = 0x08000000)` (console-flash fix
[OFFICIAL: Win32 docs]), and a hard timeout (~10 s for status calls). Failure
classification keys off exit code `-1`/`0xFFFFFFFF` (wsl.exe-level failure — a
deliberate, source-documented contract distinct from Linux exit codes 0–255)
and the locale-independent `WSL_E_*` constant names on stderr — never localized
prose [OFFICIAL: `WslClient.cpp`, `wslservice.idl`].

The timeout+preflight is load-bearing: on Windows 11 24H2 the in-box wsl.exe
stub can print "Press any key to install…" and **block for 60 s** when WSL
isn't installed (broke Rancher Desktop's installer [MULTI: rancher-desktop
#7975]); registry preflight means we never poke wsl.exe blind [UNVERIFIED
whether the stub skips the prompt with redirected stdio — mitigated anyway].

### D5. First-run wizard is a state machine; there is no silent install

`wsl --install` self-elevates (UAC prompt) for the one-time Virtual Machine
Platform enablement and then **requires a reboot** before a distro can be
installed [OFFICIAL: `InstallWsl`/`InstallPrerequisites` source + Learn]. No
fully headless no-admin path exists — Docker documents the same. So the wizard
states are: `wsl-absent` → launch `wsl --install --no-distribution --no-launch`
via `runas` (UAC attributed to wsl.exe, our GUI stays unelevated) → `reboot-
pending` (first-class step, persisted) → `no-distro` → `wsl --install -d Ubuntu
--no-launch` (no admin) + poll HKCU until `State` = installed → `needs-update`
(WSL &lt; 2.1.1 → offer `wsl --update`) → `provisioning` → `ready`.

### D6. `connect` runs its ssh **inside WSL** — which un-blocks ControlMaster

Win32-OpenSSH has no ControlMaster; Linux OpenSSH in WSL does. The app keeps
calling `chimaera_remote::connect` in-process, but every `Command::new("ssh"/
"scp")` goes through a **command transport** seam: direct on Unix, `wsl.exe -d
<distro> --exec env <vars> ssh …` on Windows. Consequences mapped in the seam
audit:

- Control-socket and download-cache paths must be **WSL-side** paths when the
  transport is WSL (a socket ssh creates lives in the distro), so path
  vocabulary hangs off the transport, not off `data_dir()` unconditionally.
- The `-L` tunnel listener binds inside WSL; NAT localhost forwarding makes it
  reachable at Windows `127.0.0.1`, so `wait_for_port`/`http_alive`/window URLs
  keep working unchanged [OFFICIAL for loopback-bind forwarding].
- Env for ssh children (`SSH_ASKPASS`, `SSH_ASKPASS_REQUIRE=force`,
  askpass endpoint/token) cannot ride `std::env::set_var` across the boundary —
  it's passed per-command (secrets via **WSLENV**, not the command line, so
  they don't appear in `/proc/*/cmdline`).

### D7. Askpass: interop exec, prompt via stdin, token-gated loopback TCP

Verified chain: OpenSSH execs `$SSH_ASKPASS` **directly** (`execlp`, no shell —
spaces in `C:\Users\First Last\…` translated paths are safe) and reads the
first stdout line [OFFICIAL: openssh `readpass.c`]; `SSH_ASKPASS_REQUIRE=force`
(OpenSSH ≥ 8.4) makes it fire even though our ssh runs in a PTY [OFFICIAL];
WSL interop executes Windows exes from Linux with faithfully piped stdout,
enabled by default [OFFICIAL]; working precedents exist (winaskpass,
wsl-ssh-agent, 1Password) [COMMUNITY].

Design (three small pieces, protocol from `askpass.rs` unchanged in spirit):

1. Shell listens on `127.0.0.1:0` (Windows side), advertises
   `<port>` + a per-launch **auth token** — required because loopback TCP loses
   the unix socket's 0700-directory confidentiality; any local process could
   otherwise fake a prompt and receive a typed password.
2. `SSH_ASKPASS` → a WSL-side `#!/bin/sh` wrapper that checks interop is
   available (degrades to failure-not-hang, mirroring today's empty-answer
   behavior) and pipes the prompt to the helper's **stdin** — deliberately not
   trusting interop's Linux-argv→Windows-cmdline marshaling with arbitrary
   prompt text [UNVERIFIED fidelity → designed around].
3. `chimaera.exe --askpass` (existing argv role, new transport) connects to the
   Windows loopback listener, authenticates with the token (via WSLENV-passed
   env), forwards the prompt, prints the secret to stdout → interop pipes it
   back to ssh.

Constraint discovered in research: interop sockets are per-session and go stale
for daemon-outliving processes (`$WSL_INTEROP`, microsoft/WSL #5065) — fine for
us because **the app spawns each ssh via a fresh wsl.exe session**; the
WSL-side daemon must never be the one spawning interop-dependent ssh.

### D8. Packaging: NSIS-only, unsigned v1, SignPath endgame

- **NSIS only** (`tauri.windows.conf.json` → `{"bundle":{"targets":["nsis"]}}`;
  per-platform config is RFC 7396 merge — arrays replace wholesale, so the
  macOS `["app","dmg"]` is untouched) [OFFICIAL: Tauri docs]. NSIS is per-user
  `%LOCALAPPDATA%` install, **no admin**, updater-compatible; MSI can't be the
  updater story and creates cross-format confusion [OFFICIAL + tauri-action
  #1027]. `webviewInstallMode` stays default `downloadBootstrapper` (Win11
  ships WebView2; Win10 dev machines have it).
- `icons/icon.ico` already exists on disk — it just needs adding to the
  config's `bundle.icon` list.
- WebView2 loads `http://127.0.0.1:{port}` fine (Chromium treats loopback as a
  trustworthy origin — no cleartext blocker) [MULTI]. Note web origin is
  scheme+host+**port**, so a stable daemon port keeps localStorage intact —
  same as macOS today.
- **Unsigned v1**: SmartScreen shows "Windows protected your PC" → "More info →
  Run anyway"; reputation is per-file-hash for unsigned binaries so every
  release restarts it [OFFICIAL]. EV no longer bypasses SmartScreen [OFFICIAL].
  Azure Artifact Signing (né Trusted Signing, $9.99/mo) is **unavailable to an
  EU individual** (individuals: US/Canada only) [OFFICIAL: FAQ]. Path: ship
  unsigned with a documented click-through + apply to **SignPath Foundation**
  (free OSS signing; chimaera qualifies — OSS license, public repo, release
  history); wire it later via `bundle.windows.signCommand` without touching the
  pipeline [MULTI].
- **release.yml**: windows job joins the tauri-action set with the same
  `tagName`. tauri-action merges `latest.json` across platform jobs
  read-modify-write, with a **documented race** under parallel jobs (#409,
  #927, #1270 — fixes landed through v1.0.0, `retryAttempts` exists) — we
  **serialize** (windows `needs:` the macOS app job) because a silently
  platform-missing `latest.json` breaks auto-update invisibly; also pin
  tauri-action v1 and bump `tauri-plugin-updater` ≥ 2.10.0 (v1.0.0's
  `latest.json` format additions) [OFFICIAL + COMMUNITY for serializing].
- **app.yml**: add a `windows` PR build-check job (windows-latest; not the
  scarce resource macOS is).

### D9. Verification without a Windows machine

This is the answer to "we know it will work":

1. **CI is the authoritative Windows harness.** GitHub-hosted Windows runners
   have nested virtualization since Jan 2024; **WSL2 works on them**
   (windows-2025 image ships the WSL update; `Vampire/setup-wsl` documents WSL2
   support) [MULTI: runner-images #10563, setup-wsl docs]. So a workflow job
   can run the *real* end-to-end flow on every PR that touches this area:
   install the NSIS artifact silently (`setup.exe /S`), provision a real Ubuntu
   distro, run the shell's headless smoke mode (detect → provision daemon →
   spawn → authenticated health → create a session via API → `wsl --shutdown`
   → re-adopt/respawn), and assert each step. This satisfies the repo's
   verify-live convention mechanically. A hidden `chimaera.exe --wsl-smoke`
   (or a separate smoke bin sharing the `wsl` module) makes the flow drivable
   headlessly; `tauri-driver` + msedgedriver exists later for UI-level checks
   [OFFICIAL: Tauri webdriver docs — Windows is a supported driver platform].
2. **Interactive look-and-feel** from the Mac: Windows 11 ARM in UTM (official
   ARM64 ISO; the x64 NSIS artifact runs under Windows' x64 emulation) — but
   **WSL2 does not work inside these VMs** (no nested virt for Windows guests
   on Apple Silicon) [MULTI], so this tier is visual QA only.
3. **Full interactive x64 + WSL2** when genuinely needed: an hourly Azure
   Dv5-family VM (nested-virt capable, ~$0.2–0.25/hr ballpark, deallocate when
   idle) — or a community tester once a beta artifact exists.

Fast local dev loop to try (not load-bearing): `rustup target add
x86_64-pc-windows-msvc` + `cargo check --target x86_64-pc-windows-msvc` in
`crates/chimaera-app` catches cfg-gate compile breaks from macOS without
waiting for CI; the CI windows job stays the honest gate.

## Code-change inventory

From the seam audit (file:line refs verified against this branch):

**`crates/chimaera-app`**
- `Cargo.toml` — `chimaera-server` and `nix` move to
  `[target.'cfg(unix)'.dependencies]`. The **only** chimaera-server call is
  `daemon.rs:44` reached from the `--daemon` argv branch (`main.rs:29–32`):
  two `#[cfg(unix)]` attributes gate the whole daemon role out of the Windows
  build. On Windows the binary has two roles (shell, `--askpass`), not three.
- `daemon.rs` — split into a shared core (decision policy `update_decision`,
  `health_ok`, `live_session_count`, `LocalDaemon`) + platform providers:
  `daemon_unix.rs` (today's code: probe via `Manifest::load`+`is_alive`
  (`daemon.rs:132`), SIGTERM stop (`:176`), `current_exe --daemon` spawn
  (`:202`)) and `daemon_wsl.rs` (probe = `wsl --exec cat manifest.json` +
  `wsl --exec sh -c 'kill -0 <pid>'` + the same authenticated health check;
  stop = `kill -TERM` in-distro; spawn = the D1 command; provision = D2).
  The update chain decouples on Windows: daemon ≠ `current_exe`, so daemon
  update = re-provision from the release, while the app keeps the Tauri
  updater (`update.rs` unchanged).
- **new `wsl.rs`** — detection state machine (D4/D5), distro enumeration/
  picker, the single hardened `run_in_wsl(distro, cwd, argv, env)` helper
  (D4's spawn rules in one place), provisioning (D2).
- `askpass.rs` — transport seam: unix socket (today) / token-gated loopback
  TCP + WSL-side wrapper script (D7). Framing (write-prompt, half-close,
  read-secret-line) is already transport-agnostic.
- `menu.rs` — `#[cfg(target_os = "macos")]` around `services/hide/hide_others/
  show_all` and the app submenu (`menu.rs:13–23`); accelerators already use
  `CmdOrCtrl`.
- New Tauri commands for the wizard (wsl status/setup/pick-distro) must join
  the **quad-lockstep**: `generate_handler!` ↔ `build.rs` ↔
  `capabilities/daemon-ui.json` ↔ `permissions/autogenerated/*.toml` (+
  `native.ts` wrappers) — per `.claude/rules/native-app.md`.
- `tauri.conf.json` — add `icons/icon.ico` to the icon list; new
  `tauri.windows.conf.json` with NSIS target (D8).

**`crates/chimaera-core`** (must keep the root daemon workspace building
identically — it's a path dep of both workspaces)
- Gate `lib.rs:6` (`std::os::unix::fs::{DirBuilderExt, PermissionsExt}`) and
  the three mode-setting call sites; `nix` →
  `[target.'cfg(unix)'.dependencies]`.
- `runtime_dir()` Windows branch (`%LOCALAPPDATA%\chimaera\run`);
  `Manifest::is_alive` behind a platform seam (the WSL shell must not call a
  Windows `kill` on a Linux pid — `daemon_wsl` owns liveness).
- `login_shell()` + `shellint.rs` → `#[cfg(unix)]` wholesale (daemon-only).

**`crates/chimaera-remote`**
- ~95% already portable; gate the one compile-blocker (`lib.rs:17`, chmod at
  `:927–929`), replace the `sha256sum`/`shasum` shell-out (`:945`) with the
  `sha2` crate (also removes a runtime dependency on Unix tools).
- Introduce the **command transport** seam (D6): direct vs wsl.exe-prefixed
  `ssh`/`scp`, with transport-owned path vocabulary (control dir, dist cache)
  and per-command env injection. The existing `RemoteOps` trait
  (`lib.rs:417–445`) covers most of the surface; `connect()`'s tunnel phase
  (`spawn_tunnel`, `:1131`) sits outside it and gets the same treatment.

**CI / site**
- `app.yml` + `release.yml` per D8; the WSL2 integration smoke per D9.
- `site/`: Windows download button + OS detection in
  `site/assets/js/site.js` (currently macOS-hardcoded at `:92–105`), beta
  label, a short "Windows runs your WSL2 environment" explainer with the
  SmartScreen note. `docs/features/native-app.md` gains the Windows section
  when it ships.

## Milestones (stacked, each CI-verified)

1. **M0 — portability groundwork.** The cfg-gates in core/remote/app; a
   windows-latest `cargo check`/build job in app.yml goes green. No behavior
   change on Unix (`just check` identical).
2. **M1 — the WSL engine.** `wsl.rs` + `daemon_wsl.rs` + provisioning + the
   first-run wizard UI + the CI WSL2 smoke (D9.1). Exit criterion: on a GitHub
   Windows runner, a fresh distro goes zero → running daemon → authenticated
   health → session created → survives `wsl --shutdown` with one-click
   recovery, all asserted.
3. **M2 — connect + askpass.** The command transport in chimaera-remote, the
   TCP/interop askpass relay, remote-host flows from Windows. CI smoke extends
   to an in-runner sshd loopback target.
4. **M3 — ship.** NSIS in release.yml (serialized tauri-action), updater
   entries, site + docs, SignPath application. First release labeled beta
   until a real-hardware pass (D9.2/3) confirms look-and-feel.

## Risk register

| # | Risk | Severity | Mitigation |
|---|---|---|---|
| R1 | NAT localhost forward dies (sleep/VPN) | High-likelihood, low-damage | Health-check on focus/resume; recovery UX; stdio channel unaffected; relay fallback as future option (D3) |
| R2 | `wsl --shutdown` kills daemon | Certain, by design | Detect + one-click respawn; reboot-equivalent semantics (D3) |
| R3 | Port collision silently routes to a stranger's service | Low, high-confusion | Token handshake before adopt — already required (D3) |
| R4 | 24H2 stub blocks 60 s when WSL absent [UNVERIFIED stub behavior] | Medium | Registry preflight + stdin-null + timeouts mean we never hit it blind (D4) |
| R5 | Interop argv fidelity for prompts [UNVERIFIED] | Low | Prompt travels via stdin, not argv (D7) |
| R6 | Interop disabled (corporate wsl.conf) | Unknown prevalence | Wrapper degrades to clean failure = today's no-askpass behavior; terminal fallback exists |
| R7 | Mirrored-networking mode: 127.0.0.1-only bind reachability [UNVERIFIED] | Low today (NAT is default) | Detect effective mode; CI runner (Server 2025) can exercise mirrored; revisit before it becomes Windows' default |
| R8 | tauri-action latest.json race | Medium | Serialize jobs + retryAttempts + pin v1 (D8) |
| R9 | SmartScreen scares users off unsigned builds | Certain until signed | Documented click-through; SignPath application (D8) |
| R10 | WSL1-only machines / Win10 | Small tier | Hard-require WSL2 (registry `Version`), point at `wsl --set-version`; Win10 best-effort (past EOL) |

**Must-verify-live list** (first Windows run, all mitigated regardless): the
24H2 stub's non-interactive behavior (R4); interop prompt fidelity (R5);
mirrored-mode loopback (R7); exec-bit via 9P writes (moot — we stream via
stdin); `wsl -u <user>` HOME-from-passwd (one-liner check).

## Explicitly out of scope

- **Native Windows daemon** (PowerShell PTY sessions, Win32 file semantics) —
  permanent double maintenance, and Win32-OpenSSH's missing ControlMaster
  breaks `connect`'s architecture anyway. The WSL model *is* the product
  story: chimaera on Windows is your Linux environment in real windows, the
  same way VS Code Remote-WSL is the accepted way to dev on Windows.
- Shipping our own imported distro (Docker's model) — agents (`claude`,
  `codex`) and dotfiles live in the **user's** distro; a private distro would
  sever chimaera from the very environment it exists to drive.
- Hyper-V-socket transport — admin-only service registration + undocumented
  VM-GUID discovery; wsl.exe stdio is the supported channel.
