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
| `main.rs` + `lib.rs` | Thin free executable and reusable original shell assembly. Fixed daemon/askpass roles run before GUI initialization. `run_with_account` accepts one optional typed native owner factory; the free executable supplies none. `run_with_context` accepts a fixed build-time context factory called only after helper/headless dispatch; the default keeps the original public context. `run_with_context_and_assembly` additionally accepts inert local daemon identity; original wrappers supply None. |
| `Entitlements.plist` | macOS hardened-runtime entitlements, applied by `tauri.conf.json` to the binary that also runs `--daemon`: Wasmtime's executable plugin memory, and `audio-input` for voice dictation's microphone. |
| `Info.plist` | Merged into the bundle's by the Tauri bundler: `NSMicrophoneUsageDescription` (voice dictation records in the web view; macOS kills an app that opens the mic without it). |
| `account/` | Fixed typed optional owner interface, unchanged named IPC commands/presentation DTOs (no sign-in URL or billing timing policy) and finite host effects. The free absent path performs no account effects. |
| `command_manifest.rs` | Shared daemon/wizard command vocabulary for build-time permission generation and exact runtime daemon grants. The exported finite vocabulary also verifies selected assembly permission parity without changing dispatch or scope. |
| `shell.rs` | Module root: app-global `Shell` state, `WindowScope`, `lock`, and the Tauri `Builder` assembly (`run`). Closing the last non-Home window opens local Home; closing the last local Home exits, while explicit Quit preserves restore state. A quit (`finish_quit`) or a window close first passes the unsaved-edits guard; nothing else holds it. Quitting never moves work: the daemon outlives the app, so anything kept running or reachable is the daemon's (or its extension's) job, never the shell's. Re-exports `open_ui_window`. |
| `shell/commands.rs` | The IPC command surface (`#[tauri::command]` fns wired into `generate_handler!`) — thin delegators. |
| `shell/connect.rs` | The `connect` flight state machine (one coalesced ssh attempt per host; a flight for a wedge suspect — or with no live tunnel — first clears a wedged ControlMaster, before the old tunnel's teardown — both masters for an alias routed to its daemon's login node) + the host-row wire vocabulary (`HostState` — incl. `node`, the login node a pool alias is pinned to — /`HostStatus`, and the `routing` progress phase) + `with_hosts`, the app's single path to hosts.json (serialized, off the reactor via `spawn_blocking`; the CLI writes it directly in crates/chimaera/src/connect.rs). A direct or capability-confirmed kept SSH connect that finds Slurm returns the cluster page without starting a login-node daemon; the persisted scheduled-jobs/login-host choice remains explicit, with legacy `not_cluster` migrated to the login-host choice without suppressing scheduler facts. Launch restore only attaches an already connected keeper route; explicit Connect/Reconnect owns new authentication. The default-off native prototype starts its original authentication deadline before capability negotiation, cancels that HTTP wait on account retirement, and rechecks the original account before key/trust selection; later grant, Ready and signing phases spend that same deadline. Kept transport failures never silently choose direct SSH. A flight whose keeper row is the account's cloud is refused before any reconnect (nothing wakes it) and drops that alias's saved windows (an older build's), so launch restore skips them with a log line. |
| `shell/cluster/kept.rs` | Fixed delegate facade for an optional account-owned cluster. Free Direct cluster operations retain their original host implementation; unavailable selected kept routes refuse without fallback. |
| `shell/cluster.rs` | Clusters: the `cluster_*` commands behind the cluster page — overview, discovery, add/remove a workspace, the folder picker (`cluster_list_dir` → `chimaera browse --dir`), start/continue/stop/dismiss a job (direct `sbatch`/app-held `srun`, or capability-confirmed keeper job control), open/close/move a workspace in a job (job-host's API over a plain `ssh -L`; one forward per job-host and per workspace window; kept hosts use exact resource Link listeners), startup commands (the cluster's Environment scopes), rules for agents, the persisted scheduled-jobs/login-host choice (first-setup completion in `hosts.json`; legacy `set_not_cluster` maps to direct-host mode without suppressing scheduler facts), the login-node terminal (a terminal-only `term=` window, `web-ui/src/lib/terminal/TerminalWindow.svelte`, on a shell in the local daemon's hidden workspace; never restored, its session ends with the window and a crash's leftover at the next launch — `sweep_terminals`). Direct reads lay each running job's own job-host answer over the cluster folder (`overlay_live`; the folder can show an open or a close a minute late), and a workspace window opens straight into its workspace (`ws=`). Per-cluster live state (endpoints kept Rust-side, the notification diff), `absorb` (toasts: ready / an hour / ten minutes / stopped; a window whose workspace closed, failed, or whose job ended learns why; one on its way to another job is told "moving" and reopened there), and the passive watcher that runs only while a job this app knows of is alive (woken at the end-of-job marks; free and Pro alike). The watcher's look (`watch_overview`) never recovers a wedged ControlMaster: a failed look backs off to five minutes and the watcher stops after five failures in a row, so a new login, and any password/two-factor prompt, waits for the user. User-driven reads (`fresh_overview`) clear a wedged master once (`chimaera_remote::clear_wedged_master`) and retry. A connect that lands on a cluster (`ClusterHost`) is a success in `shell/connect.rs` (`landed_on_cluster`), never a daemon start. |
| `shell/pro.rs` | Optional account lifecycle facade and saved Direct choice. No credential, Client, billing, installation or account task implementation remains in this crate. |
| `shell/tunnel.rs` | App-only SSH / keeper transport wrapper. Both expose one loopback daemon endpoint; keep chimaera-link out of the daemon dependency graph. `app_host`: the account's cloud (`HostKind::Worker`) is never one of the app's hosts; `offers_daemon_update` never flags it outdated (the service updates it), while SSH hosts and other computers keep the note and the update toast. |
| `shell/restore.rs` | `open_ui_window` (every window opens through `open_shell_window`, which refuses the account's cloud: the app never shows its own page), the tunnel health monitor (a 3 s `interval` tick — a down host's probe burns its 2 s timeout inside it; 3-miss hysteresis; confirmed-down keys back off per miss up to 10 ticks, compute keys 2; a `down` edge files a wedge suspect), and launch-time window restore. |
| `daemon.rs` | Launch/adopt the local daemon; version/parity policy (unix: spawn-self; windows: delegates to `wsl.rs`). A typed build-selected stateless runtime factory reuses the same fixed headless config and four-worker/128-blocking runtime; the free entrypoint supplies none. Unix native startup observes the actual optional assembly in its original authenticated health request (2s, 16KiB cap), separately from Core parity. Identity-selected account construction waits for positive extension presence and matching private identity; legacy wrappers keep presence-only behavior. An optional bounded `daemon_assembly` is read in that same health request; free callers ignore it. Same-SDK private mismatch or missing legacy identity uses the original idle/defer/explicit replacement matrix, without changing Core or remote source matching. Selected readiness withholds owner construction while replacement is deferred; idle replacement and explicit graceful stop retain the original session rules. Free reuse accepts a compatible selected daemon but refuses downgrading a known extension with the free executable. WSL adoption and the Direct SSH deployment path retain their free behavior; source tests cover the finite matrix and a synthetic HTTP probe. Original corrupt-record/dead-PID recovery is retained; only a raw IO-confirmed refused original loopback connection makes an alive-PID record stale (after a transport refusal hint only, at most 100 ms within the original 2 s health budget), while reachable malformed/auth/identity and unknown transport failures refuse replacement. |
| `wsl.rs` | The WSL2 engine: registry-first detection + version gate, hardened wsl.exe spawns, the persisted target (distro + PINNED `-u` user — wsl.json), provision/replace/spawn/probe/stop, connect wiring (pure parts unit-tested on any host; e2e via wsl-smoke). Startup only ADOPTS; anything that provisions runs in the wizard, visibly. A copy target receipt binds the healthy manifest PID/start/token/port to the exact distro/user/home; only an unchanged positive adoption can retain it. Manifest and folder-conversion stdout/stderr are capped while reading, with fixed deadlines and launcher cleanup on overflow/timeout. The real WSL smoke exercises native-drive and same-distro UNC folder conversion plus restart refusal; Mac tests cover only parsing/receipt logic, not WSL runtime acceptance. |
| `assets/setup.html` | The Windows first-run wizard (shell-local page — no daemon origin exists yet to serve the real UI). |
| `askpass.rs` | Original local Direct SSH askpass and trusted window scope/pending table. An injected account owner may contribute guarded typed prompts and answer sinks; the host retains modal publication, timeout, answers and cleanup. A cancelled prompt gets an explicit `cancelled` reply (an answer always ends in `\n`, so an empty reply is a relay fault, not a cancel); the helper exits `CANCELLED_EXIT` and the shim/wrapper ends the asking ssh — OpenSSH would otherwise treat any failed askpass as an empty password and re-ask. |
| `appearance.rs` | Bounded per-host first-paint palette cache outside volatile daemon origins; persisted atomically and carried in native window URLs. |
| `shell/commands.rs::open_external` | The ONLY route a rendered link has to the user's real browser: the navigation guard admits just the daemon origin (plus iframes' inline `about:srcdoc` documents — WebKit asks the policy for subframes too, so without them slides, the live HTML preview and notebook HTML output draw blank) and nothing receives a `target="_blank"`, so an external link is otherwise swallowed. **http/https only** — hrefs are agent-authored and the platform opener would act on `file:`/app schemes. |
| `shell/commands.rs::copy_file_to_clipboard` / `reveal_in_file_manager` | The page names a path and the shell hands it to the OS: the file itself on the clipboard (arboard's file list — the clipboard plugin has no such write), or selected in Finder / the freedesktop `FileManager1` file manager. **Local windows only** (`local_file_path`): remote hosts' pages hold the same command grants, and their paths would name whatever sits at that path on this machine. Refused on Windows, where the local daemon's paths are WSL2's. |
| `shell/unsaved.rs` | Window close / app quit never drop unsaved editor text: the page pushes its unsaved count (`report_unsaved`), so `CloseRequested`/quit decide synchronously; a held one asks the window (`unsaved-prompt` → `reply_unsaved`). Pure `Guard` (unit-tested) + glue; a 4 s hung-page timeout and a third-ask escape keep it from ever trapping the user. Also the macOS `applicationShouldTerminate:` hook (Dock › Quit, logout never reach `ExitRequested`). |
| `shell/notices.rs` | Notices → OS notifications: one long-poll watcher per open daemon (`GET /api/v1/notices`), suppression for the window you're in (the focused workspace window covers every session of its workspace; a torn-off window only what it shows via `report_window_view`), one-alert-per-session supersede/withdraw, click routing (`focus-session` + `take_pending_focus`), the Dock badge/bounce and tray counts. Feature: [notifications.md](../../docs/features/notifications.md). |
| `shell/print_frame.rs` | macOS: a page's `window.print()` (the slides' and documents' print buttons, each in a one-off same-origin iframe). WKWebView drops it unless the UI delegate answers WebKit's `_webView:printFrame:pdfFirstPageSize:completionHandler:`; this adds that method to wry's delegate class at runtime, re-assigns the delegate (WebKit reads its hooks on assignment), and prints just that frame through the system panel as a sheet. The completion runs only when the panel is done — the page's `print()` blocks until then, and a synchronous run deadlocks. SPI with a fail-closed `respondsToSelector:` check. |
| `notify.rs` | The platform notifier: macOS `UNUserNotificationCenter` + click delegate + dock tile (needs a signed `.app` — unbundled dev builds degrade to none), `notify-rust` on Linux/Windows. Identifiers encode the click route. |
| `windows.rs` | The per-window registry (round-trips window↔workspace). |
| `update.rs` | The auto-updater intent chain (consume-once, expiry) + the kept outcome of every signed-update check (`status`/`check`, behind `app_update_status`). |
| `menu.rs` | The menu bar. Page-owned items reach the focused window as a `menu` event; Reload Window instead evaluates a fixed script (`RELOAD_WINDOW_JS`) that calls the page's reload hook, so a page that never booted (no listener) still reloads. Settings… is enabled for any focused daemon window, Home included (`shell::focused_daemon_open`); the WSL setup wizard and a login-node terminal window (no settings surface) disable it. |
| `tray.rs` | The menu-bar / system-tray status item (`tray-icon` feature). |

## Invariants / gotchas

- The free assembly never initializes an account owner, credential store, account client,
  event task or Pro SSH signer. Direct SSH, its local askpass, system configuration,
  Slurm operations, managed agent installations and window persistence remain host-owned.
  A saved kept host requires an explicit Direct choice when no extension is installed;
  account device identities never fall back to SSH.
- An injected native account owner uses fixed typed `account::AccountExtension`
  operations. The host still owns original windows, prompts, connect flights, tunnels,
  unsaved guards and the serialized hosts.json writer. The private assembly retains
  account generations, credentials, operation admission and transport ownership
  (the reverse-serve link belongs to the daemon). Named operations carry optional original-account receipts. The free absent path has no lifetime or account owner.

- **The command list is a lockstep** the type system does NOT fully enforce, so a
  drift only surfaces at runtime: `shell.rs` `generate_handler!` ↔
  `command_manifest.rs` (consumed by `build.rs` + runtime grants) ↔ committed `permissions/autogenerated/*.toml`
  (plus the `native.ts` wrappers + event-name strings). Add/rename a command in all
  of them, then `npx tauri build` (the app.yml path) to confirm they agree.
  `command_manifest::tests` fails when the lists overlap or the committed
  permission files drift; a removed command's `.toml` must be deleted by hand.
  **Account commands (`pro_*`) are local-only**: they live in
  `LOCAL_ACCOUNT_COMMANDS` and `authorize_daemon_origin` grants them only to a
  window whose scope has no host alias (this computer's own daemon). A remote
  host's, another computer's or the cloud's UI runs code this shell does not
  control and must never sign out, remove devices, open billing or submit
  provider codes; its `pro_*` calls are rejected. Runtime grants are additive,
  so a window that later shows a remote UI on a recycled former local port
  would keep them; the navigation guard makes that the only residual.
  **Documented exception:** the `wsl_*` commands have NO `native.ts` wrappers on
  purpose — only the shell-local wizard (`assets/setup.html`, direct
  `window.__TAURI__` invokes) may call them, and they are granted by
  `capabilities/wsl-setup.json` (window label `wsl-setup*`, local-only), NEVER by
  the runtime daemon-window capability: `wsl_install` pops UAC and
  daemon-served pages include REMOTE hosts' UIs. Each daemon window instead
  receives a capability for its exact volatile label + current loopback port;
  the navigation guard advances with reconnects so stale granted ports cannot
  be revisited after reuse.
- **Window/quit lifecycle, in order.** A window close runs `CloseRequested`:
  the unsaved-edits guard (`unsaved`) may hold it. A quit reaches `finish_quit`
  only after the unsaved guard. macOS terminate (Dock › Quit, logout) runs the
  same guard in `unsaved::os_quit_may_proceed`, deciding synchronously. The
  updater's `app.restart()` asks nothing. The daemon outlives every quit
  (daemon.rs); with Pro composed in, nothing about a quit reaches the account:
  the daemon's own lease and reverse link keep the work where it is.
- **The single-instance plugin must stay first** in `shell.rs`'s builder. The
  shell owns process-global window persistence, tunnels, and askpass state; two
  app processes would race and corrupt that ownership. A repeated launch raises
  an existing window instead.
- **Version stamping matches the literal `0.0.1` sentinel** via `sed` across
  `tauri.conf.json` + `crates/chimaera-app/Cargo.toml` + root `Cargo.toml` (release
  reads `chimaera-core::VERSION` to fetch the matching remote daemon). If any of
  those holds a non-`0.0.1` value, the sed silently no-ops → ships the wrong version.
- Updater signing is release-only (`TAURI_SIGNING_PRIVATE_KEY*`); the PR build
  (`app.yml`) needs no key. macOS code signing still runs on PRs (ad hoc today)
  with the hardened runtime. Keep `Entitlements.plist` wired into the bundle:
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
