# chimaera-app — the Tauri 2 native shell

Orientation for coding agents. A native wrapper (macOS + Linux; Windows via the
WSL2 engine) around the *same* daemon + web UI the browser loads: it opens real
OS windows pointed at `http://127.0.0.1:{port}/#…` (a local daemon, or an
ssh-tunnelled remote one) and adds native affordances (remote-host management,
in-app SSH askpass, a signed auto-updater). Parent map: repo-root
[AGENTS.md](../../AGENTS.md).

## The three things to know first

1. **This is its OWN standalone cargo workspace.** Tauri is deliberately kept out
   of the daemon workspace so musl/HPC builds stay lean. Consequence: the daemon
   crates compile **twice** (once per workspace) — an intentional cost. Do NOT
   "fix" it by folding the app into the root workspace. Root `cargo` never builds
   this; use `just app-dev` / `just app-check` / `just app-build`.
2. **One binary, three roles, selected by argv in `main.rs`** (before any init):
   default = the Tauri shell; `--daemon` = a headless `chimaera_server::run` (the
   .app is self-contained — the daemon IS the app binary); `--askpass <prompt>` =
   the tiny `SSH_ASKPASS` relay (must stay lightweight — never spawn a daemon/window).
3. **Windows is different by design: the daemon is NEVER this exe there** — it is
   the Linux musl release binary inside the user's WSL2 distro (`wsl.rs` owns
   detect/provision/spawn/adopt; `--daemon` and `chimaera-server` are cfg'd out of
   that build). No WSL yet → startup opens the shell-local wizard
   (`assets/setup.html`) instead of failing. Design + research evidence:
   [docs/design/windows-wsl-plan.md](../../docs/design/windows-wsl-plan.md); live gate:
   `.github/workflows/wsl-smoke.yml` (real WSL2 on a Windows runner). macOS
   cannot compile the Windows target locally — app.yml's `windows` job is the
   compile gate.

## File map

| File | What it owns |
|---|---|
| `main.rs` | The 3-role argv dispatch (order is load-bearing). |
| `Entitlements.plist` | macOS hardened-runtime entitlements, applied by `tauri.conf.json` to the binary that also runs `--daemon`: Wasmtime's executable plugin memory, and `audio-input` for voice dictation's microphone. |
| `Info.plist` | Merged into the bundle's by the Tauri bundler: `NSMicrophoneUsageDescription` (voice dictation records in the web view; macOS kills an app that opens the mic without it). |
| `command_manifest.rs` | Shared daemon/wizard command vocabulary for build-time permission generation and exact runtime daemon grants, plus `COMMAND_AUTHORITY`: every daemon command classed once as any-window, own-host (`shell::require_target`) or local-only (`shell::require_local`). |
| `shell.rs` | Module root: app-global `Shell` state, `WindowScope`, `lock`, and the Tauri `Builder` assembly (`run`). Closing the last non-Home window opens local Home; closing the last local Home exits, while explicit Quit preserves restore state (quit cancels in-flight connects and closes every tunnel at once under one 5 s deadline). Re-exports `open_ui_window`. Also the host boundary: the exact core grants (`DAEMON_UI_CORE_PERMISSIONS`, never `core:default`), per-window event delivery (`deliver_to`, `emit_host_scoped` — the token-bearing `host-status` copy only to that host's windows, a tokenless one to local windows and Home) and the caller checks (`caller_may_target`, `require_target`, `require_local`). |
| `shell/commands.rs` | The IPC command surface (`#[tauri::command]` fns wired into `generate_handler!`) — thin delegators; each own-host or local-only command checks its caller first (see `COMMAND_AUTHORITY`). `update_local_daemon` hands the new token to local windows only; `begin_update` runs one install at a time and re-checks unsaved edits before the relaunch. |
| `shell/connect.rs` | The `connect` flight state machine (one coalesced ssh attempt per host; a flight for a wedge suspect — or with no live tunnel — first clears a wedged ControlMaster, before the old tunnel's teardown — both masters for an alias routed to its daemon's login node) + the host-row wire vocabulary (`HostState` — incl. `node`, the login node a pool alias is pinned to — /`HostStatus`, and the `routing` progress phase) + `with_hosts`, the app's single path to hosts.json (serialized, off the reactor via `spawn_blocking`; the CLI writes it directly in crates/chimaera/src/connect.rs). A flight is cancellable (`cancel_connect`: Disconnect and Quit); a cancelled flight never installs a tunnel or reopens windows — the switch is re-read under the tunnels lock. Every `host-status` goes through `emit_host_status` (token only to that host's windows). |
| `shell/cluster.rs` | Clusters: the `cluster_*` commands behind the cluster page — overview, discovery, add/remove a workspace, the folder picker (`cluster_list_dir` → `chimaera browse --dir`), start/continue/stop/dismiss a job (`sbatch`, or an attached `srun` this app holds), open/close/move a workspace in a job (job-host's API over a plain `ssh -L`; one forward per job-host and per workspace window), startup commands (the cluster's Environment scopes), rules for agents, the persisted scheduled-jobs/login-host choice (first-setup completion in `hosts.json`; legacy `set_not_cluster` maps to direct-host mode without suppressing scheduler facts), the login-node terminal (a terminal-only `term=` window, `web-ui/src/lib/terminal/TerminalWindow.svelte`, on a shell in the local daemon's hidden workspace; never restored, its session ends with the window and a crash's leftover at the next launch — `sweep_terminals`). Every read lays each running job's own job-host answer over the cluster folder (`overlay_live`; the folder can show an open or a close a minute late), and a workspace window opens straight into its workspace (`ws=`). Per-cluster live state (endpoints kept Rust-side, the notification diff), `absorb` (toasts: ready / an hour / ten minutes / stopped; a window whose workspace closed, failed, or whose job ended learns why; one on its way to another job is told "moving" and reopened there), and the watcher that runs only while a job this app knows of is alive (woken at the end-of-job marks). A connect that lands on a cluster (`ClusterHost`) is a success in `shell/connect.rs` (`landed_on_cluster`), never a daemon start. Start estimates ride the overview's queue read; an unclear start is recovered from the job's folder (`recover_start`), never resubmitted; accounting is asked again (≥60 s apart, ≤10 tries) until it settles. |
| `shell/restore.rs` | `open_ui_window`, the tunnel health monitor (a 3 s `interval` tick — a down host's probe burns its 2 s timeout inside it; 3-miss hysteresis; confirmed-down keys back off per miss up to 10 ticks, compute keys 2; a `down` edge files a wedge suspect), and launch-time window restore. |
| `daemon.rs` | Launch/adopt the local daemon; version/parity policy (unix: spawn-self; windows: delegates to `wsl.rs`). The daemon is spawned without the app's askpass variables (`daemon_command`); the user's own `SSH_ASKPASS*` from before `askpass::install` goes back on it. An explicit update that stopped the old daemon but could not start the new one retries `ensure_local_daemon` once, then fails visibly (`recover_update`); a failed stop keeps the old one. |
| `wsl.rs` | The WSL2 engine: registry-first detection + version gate, hardened wsl.exe spawns, the persisted target (distro + PINNED `-u` user — wsl.json), provision/replace/spawn/probe/stop, connect wiring (pure parts unit-tested on any host; e2e via wsl-smoke). Startup only ADOPTS; anything that provisions runs in the wizard, visibly. |
| `assets/setup.html` | The Windows first-run wizard (shell-local page — no daemon origin exists yet to serve the real UI). |
| `askpass.rs` | The `SSH_ASKPASS` ↔ shell wire protocol (in-app password/Duo prompts). Transport: unix socket (unix) / token-gated loopback TCP fed through a WSL-interop wrapper (windows, installed by `wsl::wire_connect`). A cancelled prompt gets an explicit `cancelled` reply (an answer always ends in `\n`, so an empty reply is a relay fault, not a cancel); the helper exits `CANCELLED_EXIT` and the shim/wrapper ends the asking ssh — OpenSSH would otherwise treat any failed askpass as an empty password and re-ask. Prompts and done events are delivered per window (`shell::deliver_to`) only to `askpass_targets`; a cancelled connect drops its host's prompts (`discard_alias`). A request is capped in size and time; the `/tmp` socket-dir fallback must be private and ours at both levels (`chimaera_core::ensure_private_dir`). |
| `appearance.rs` | Bounded per-host first-paint palette cache outside volatile daemon origins; persisted atomically and carried in native window URLs. |
| `shell/commands.rs::open_external` | The ONLY route a rendered link has to the user's real browser: the navigation guard admits just the daemon origin (plus iframes' inline `about:srcdoc` documents — WebKit asks the policy for subframes too, so without them slides, the live HTML preview and notebook HTML output draw blank) and nothing receives a `target="_blank"`, so an external link is otherwise swallowed. **http/https only** — hrefs are agent-authored and the platform opener would act on `file:`/app schemes. |
| `shell/commands.rs::copy_file_to_clipboard` / `reveal_in_file_manager` | The page names a path and the shell hands it to the OS: the file itself on the clipboard (arboard's file list — the clipboard plugin has no such write), or selected in Finder / the freedesktop `FileManager1` file manager. **Local windows only** (`require_local`): remote hosts' pages hold the same command grants, and their paths would name whatever sits at that path on this machine. Refused on Windows, where the local daemon's paths are WSL2's. |
| `shell/unsaved.rs` | Window close / app quit never drop unsaved editor text: the page pushes its unsaved count (`report_unsaved`), so `CloseRequested`/quit decide synchronously; a held one asks the window (`unsaved-prompt` → `reply_unsaved`). Pure `Guard` (unit-tested) + glue; a 4 s hung-page timeout and a third-ask escape keep it from ever trapping the user. A remote window whose tunnel is down or missing is at risk (its drafts can't be journaled): the hung-page timeout never closes it; the user's third ask still does. Also the macOS `applicationShouldTerminate:` hook (Dock › Quit, logout never reach `ExitRequested`). |
| `shell/notices.rs` | Notices → OS notifications: one long-poll watcher per open daemon (`GET /api/v1/notices`), suppression for the window you're in (the focused workspace window covers every session of its workspace; a torn-off window only what it shows via `report_window_view`), one-alert-per-session supersede/withdraw, click routing (`focus-session` + `take_pending_focus`), the Dock badge/bounce and tray counts. Feature: [notifications.md](../../docs/features/notifications.md). |
| `shell/print_frame.rs` | macOS: a page's `window.print()` (the slides' and documents' print buttons, each in a one-off same-origin iframe). WKWebView drops it unless the UI delegate answers WebKit's `_webView:printFrame:pdfFirstPageSize:completionHandler:`; this adds that method to wry's delegate class at runtime, re-assigns the delegate (WebKit reads its hooks on assignment), and prints just that frame through the system panel as a sheet. The completion runs only when the panel is done — the page's `print()` blocks until then, and a synchronous run deadlocks. SPI with a fail-closed `respondsToSelector:` check. |
| `notify.rs` | The platform notifier: macOS `UNUserNotificationCenter` + click delegate + dock tile (needs a signed `.app` — unbundled dev builds degrade to none), `notify-rust` on Linux/Windows. Identifiers encode the click route. |
| `windows.rs` | The per-window registry (round-trips window↔workspace). Repeated ids load once (the first record wins). |
| `update.rs` | The auto-updater intent chain (consume-once, expiry) + the kept outcome of every signed-update check (`status`/`check`, behind `app_update_status`). |
| `menu.rs` | The menu bar. Page-owned items reach the focused window as a `menu` event; Reload Window instead evaluates a fixed script (`RELOAD_WINDOW_JS`) that calls the page's reload hook, so a page that never booted (no listener) still reloads. Settings… is enabled for any focused daemon window, Home included (`shell::focused_daemon_open`); the WSL setup wizard and a login-node terminal window (no settings surface) disable it. |
| `tray.rs` | The menu-bar / system-tray status item (`tray-icon` feature). |

## Invariants / gotchas

- **The command list is a lockstep** the type system does NOT fully enforce, so a
  drift only surfaces at runtime: `shell.rs` `generate_handler!` ↔
  `command_manifest.rs` (consumed by `build.rs` + runtime grants) ↔ committed `permissions/autogenerated/*.toml`
  (plus the `native.ts` wrappers + event-name strings). Add/rename a command in all
  of them and class it in `COMMAND_AUTHORITY`; `cargo test` (`shell::surface_tests::
  the_command_lockstep_holds`) checks handler ↔ manifest ↔ toml files ↔ `native.ts` ↔
  wizard grant, and `npx tauri build` (the app.yml path) remains the full check.
  **Documented exception:** the `wsl_*` commands have NO `native.ts` wrappers on
  purpose — only the shell-local wizard (`assets/setup.html`, direct
  `window.__TAURI__` invokes) may call them, and they are granted by
  `capabilities/wsl-setup.json` (window label `wsl-setup*`, local-only), NEVER by
  the runtime daemon-window capability: `wsl_install` pops UAC and
  daemon-served pages include REMOTE hosts' UIs. Each daemon window instead
  receives a capability for its exact volatile label + current loopback port;
  the navigation guard advances with reconnects so stale granted ports cannot
  be revisited after reuse.
- **The single-instance plugin must stay first** in `shell.rs`'s builder. The
  shell owns process-global window persistence, tunnels, and askpass state; two
  app processes would race and corrupt that ownership. A repeated launch raises
  an existing window instead.
- **Version stamping matches the literal `0.0.1` sentinel** via `sed` across
  `tauri.conf.json` + `crates/chimaera-app/Cargo.toml` + root `Cargo.toml` (release
  reads `chimaera-core::VERSION` to fetch the matching remote daemon). If any of
  those holds a non-`0.0.1` value, the sed silently no-ops → ships the wrong version.
- Updater signing is release-only (`TAURI_SIGNING_PRIVATE_KEY*`); the PR build
  (`app.yml`) needs no key. macOS code signing runs ad hoc on PRs and uses
  Developer ID + notarization on releases, both with the hardened runtime.
  `scripts/build-macos-release.sh` is tauri-action's build command: it checks
  the certificate/team, stapled ticket, Gatekeeper, and plugin execution on
  the finished bundle before the action can publish it. Credential setup:
  [releases.md](../../docs/agent-guides/releases.md#macos-signing-and-notarization).
  Keep `Entitlements.plist` wired into the bundle:
  Wasmtime 49 uses mmap/mprotect, so `allow-jit` alone cannot authorize its code
  pages. Missing `allow-unsigned-executable-memory` kills the daemon with
  `CODESIGNING / Invalid Page` on the first plugin call, even when compilation
  succeeds. Verify plugin execution in the signed bundle with
  `node scripts/smoke-macos-plugins.mjs` after `bash scripts/build-plugins.sh`.
  Both app PR CI and the macOS release job run it before publishing artifacts.
  `app.yml` is not a required check, so a red smoke there does not block a
  merge; the release job then fails at it and publishes nothing. Its expected
  answer is the daemon's pinned Knowledge response
  (`chimaera-server/src/tests/fixtures/knowledge/mycelium.json`), so a fixture
  change that re-blesses that file needs no edit to the smoke.
  Ordinary `cargo test` binaries cannot catch hardened-runtime kills of the
  app's `--daemon` process.
