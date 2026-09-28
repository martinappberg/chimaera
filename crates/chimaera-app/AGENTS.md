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
   [docs/windows-wsl-plan.md](../../docs/windows-wsl-plan.md); live gate:
   `.github/workflows/wsl-smoke.yml` (real WSL2 on a Windows runner). macOS
   cannot compile the Windows target locally — app.yml's `windows` job is the
   compile gate.

## File map

| File | What it owns |
|---|---|
| `main.rs` | The 3-role argv dispatch (order is load-bearing). |
| `command_manifest.rs` | Shared daemon/wizard command vocabulary for build-time permission generation and exact runtime daemon grants. |
| `shell.rs` | Module root: app-global `Shell` state, `WindowScope`, `lock`, and the Tauri `Builder` assembly (`run`). Re-exports `open_ui_window`. |
| `shell/commands.rs` | The IPC command surface (`#[tauri::command]` fns wired into `generate_handler!`) — thin delegators. |
| `shell/connect.rs` | The `connect` flight state machine (one coalesced ssh attempt per host; a flight for a wedge suspect — or with no live tunnel — first clears a wedged ControlMaster, before the old tunnel's teardown — both masters for an alias routed to its daemon's login node) + the host-row wire vocabulary (`HostState` — incl. `node`, the login node a pool alias is pinned to — /`HostStatus`, and the `routing` progress phase) + `with_hosts`, the app's single path to hosts.json (serialized, off the reactor via `spawn_blocking`; the CLI writes it directly in crates/chimaera/src/connect.rs). |
| `shell/cloud.rs` | Passive cloud/provider readiness, explicit bounded connection/retry actions, shared-catalog authentication URL validation, memory-only Claude authorization-code submission and explicitly acknowledged cloud-provider disconnection under the account-operation fence, and exact terminal focus only for legacy provider adapters. Account credentials remain in Rust. Polls never wake a worker. `pro_cloud_status` passes through optional account-confirmed preparing phases (`keeper`, `worker`, `connecting`); these are not daemon/provider readiness. |
| `shell/power.rs` | System sleep/wake notifications (macOS IOKit, Linux logind delay inhibitor, Windows power callbacks), bounded daemon flush, and AC-power gating for delayed hand-back. |
| `shell/pro.rs` | Optional account runtime: endpoint in app.json, OS-keychain tokens, PKCE loopback sign-in, bounded keeper cache/events, reverse local-daemon sharing, and account IPC. No endpoint means no keychain access or network work. |
| `shell/pro/billing.rs` | Native-owned checkout/portal attempts, exact provider-origin validation, cancellation, and authenticated plan confirmation independent of page visibility. |
| `shell/pro/billing/callback.rs` | One-use billing return: literal loopback Host, exact nonce/outcome query, bounded request/response and credential-free browser page. |
| `shell/pro/projects.rs` | Passive cloud project listing and per-project native destination selection; cancellation creates no import request. |
| `shell/pro/placements.rs` | Workspace-specific reconciliation from the daemon inventory; failed or retired projects do not discard a healthy shared transport. |
| `shell/pro/machine.rs` | Friendly device label from macOS ComputerName; never an identity or network-hostname grouping key. |
| `shell/pro/store.rs` | Credential namespace keys: isolated previews bind session entries to their canonical config directory; legacy unbound entries are never imported or deleted. |
| `shell/pro/recovery.rs` | Generation-fenced startup candidate retention and coalesced retry ownership; account probe failures do not force new authentication. |
| `shell/pro/credentials.rs` | Coalesced, generation-fenced credential-store writes with finite retries; persistence failures retain valid memory sessions, and only true revocation signs out. |
| `shell/pro/auth.rs` | In-memory sign-in attempt lifecycle, cancellation/retry fences and bounded loopback callback parsing. |
| `assets/sign-in.html` | Credential-free browser return page; success is sent only after native account activation. |
| `shell/tunnel.rs` | App-only SSH / keeper transport wrapper. Both expose one loopback daemon endpoint; keep chimaera-link out of the daemon dependency graph. |
| `shell/restore.rs` | `open_ui_window`, the tunnel health monitor (a 3 s `interval` tick — a down host's probe burns its 2 s timeout inside it; 3-miss hysteresis; confirmed-down keys back off per miss up to 10 ticks, compute keys 2; a `down` edge files a wedge suspect), and launch-time window restore. |
| `daemon.rs` | Launch/adopt the local daemon; version/parity policy (unix: spawn-self; windows: delegates to `wsl.rs`). |
| `wsl.rs` | The WSL2 engine: registry-first detection + version gate, hardened wsl.exe spawns, the persisted target (distro + PINNED `-u` user — wsl.json), provision/replace/spawn/probe/stop, connect wiring (pure parts unit-tested on any host; e2e via wsl-smoke). Startup only ADOPTS; anything that provisions runs in the wizard, visibly. |
| `assets/setup.html` | The Windows first-run wizard (shell-local page — no daemon origin exists yet to serve the real UI). |
| `askpass.rs` | The `SSH_ASKPASS` ↔ shell wire protocol (in-app password/Duo prompts). Keeper prompts use the same host/window scope and bounded pending table, with an explicit source and replies confined to the original events connection. Local transport: unix socket (unix) / token-gated loopback TCP fed through a WSL-interop wrapper (windows, installed by `wsl::wire_connect`). |
| `appearance.rs` | Bounded per-host first-paint palette cache outside volatile daemon origins; persisted atomically and carried in native window URLs. |
| `shell/commands.rs::open_external` | The ONLY route a rendered link has to the user's real browser: the navigation guard admits just the daemon origin and nothing receives a `target="_blank"`, so an external link is otherwise swallowed. **http/https only** — hrefs are agent-authored and the platform opener would act on `file:`/app schemes. |
| `shell/unsaved.rs` | Window close / app quit never drop unsaved editor text: the page pushes its unsaved count (`report_unsaved`), so `CloseRequested`/quit decide synchronously; a held one asks the window (`unsaved-prompt` → `reply_unsaved`). Pure `Guard` (unit-tested) + glue; a 4 s hung-page timeout and a third-ask escape keep it from ever trapping the user. Also the macOS `applicationShouldTerminate:` hook (Dock › Quit, logout never reach `ExitRequested`). |
| `shell/notices.rs` | Notices → OS notifications: one long-poll watcher per open daemon (`GET /api/v1/notices`), suppression for what the focused window shows (`report_window_view`), one-alert-per-session supersede/withdraw, click routing (`focus-session` + `take_pending_focus`), the Dock badge/bounce and tray counts. Feature: [notifications.md](../../docs/features/notifications.md). |
| `notify.rs` | The platform notifier: macOS `UNUserNotificationCenter` + click delegate + dock tile (needs a signed `.app` — unbundled dev builds degrade to none), `notify-rust` on Linux/Windows. Identifiers encode the click route. |
| `windows.rs` | The per-window registry (round-trips window↔workspace). |
| `update.rs` | The auto-updater intent chain (consume-once, expiry) + the kept outcome of every signed-update check (`status`/`check`, behind `app_update_status`). |
| `menu.rs` | The menu bar. |
| `tray.rs` | The menu-bar / system-tray status item (`tray-icon` feature). |

## Invariants / gotchas

- Logical project viewing uses passive placement and exact target scope acknowledgment.
  It follows the current owner (home device or worker), including files and watches,
  without acquiring or moving execution. Reconciliation reads the daemon’s redacted
  placement inventory, retires each no-longer-current workspace (including after
  native restart), and keeps healthy siblings on shared transports. Native window layouts remain local and
  only the fixed `/project` alias crosses hosts. See the
  [viewer contract](../chimaera-link/VIEWING.md).
  `shell/pro/installation.rs` keeps the stable installation proof in an
  endpoint/account/configuration-scoped Keychain entry. It must save before bind;
  clean release/recovery acknowledgment precedes a rebind that revokes the old
  device. Neither daemon nor webview receives the proof.
- Device rows group only account-verified installation IDs. Legacy sign-ins remain
  individually removable; the native revoke command refreshes the roster under
  the account-operation fence and rejects removal of the current sign-in.
- Account credentials belong only in the OS keychain (macOS Keychain, Windows
  Credential Manager, Linux Secret Service). app.json stores only the endpoint;
  hosts.json stores only the additive `kept` preference. Keeper host rows are
  authoritative while signed in. A 30 s bounded reconciliation discovers new
  provisioning and removals missed during an events outage; sign-out cancels
  every account-owned task and listener. The generation fence prevents pending
  refresh writes and connect flights from restoring a signed-out account.
  Startup retains the same unverified candidate Client across bounded account retries
  (2, 5 and 15 seconds), including any token rotation before a failed account read.
  Isolated builds (development or explicit `CHIMAERA_HOME`) use config-bound
  session entries and require one fresh sign-in when upgrading from the old
  endpoint-only namespace. No legacy entry is imported, deleted or revoked; the
  ordinary release app keeps its existing key.
  Account probes run outside the operation lock; installation rechecks generation
  under that lock. A denied or locked credential-store read requires explicit
  **Check again** instead of repeatedly triggering OS prompts. Exhausted account
  retries retain the candidate for an explicit retry. Neither case grants account
  access before a fresh authenticated snapshot; replacement/sign-out fences late
  results, and true revocation clears the saved session.
  Credential-store failures preserve verified in-memory sessions, including initial
  activation. A single writer retries after 2, 5, 15, 30 and 60 seconds; later token
  rotation or **Check again** starts a fresh attempt. Only exhausted retries show
  a save warning, independent of account/keeper reconciliation. Writes snapshot the
  latest token pair under the shared I/O lock without holding the token watch
  across OS calls. Revocation interrupts waiting immediately; serialized deletion
  follows any already-running OS write. Queued writes and results are generation-
  fenced, so an old account cannot overwrite a replacement or resurrect sign-out.
  `pro_status` is a synchronous cached snapshot behind async IPC: it never waits
  for the readiness watch, OS keychain or token-refresh mutex. Additive
  `initializing` and `initialization_phase` (`keychain`, `account`, `connection`)
  let the UI describe startup and withhold account mutations. Dependent commands
  still wait for readiness; keychain writes are not detached or timed out to
  manufacture UI responsiveness.
  Managed workers remain in the routing map but are excluded from `list_hosts`
  and ordinary Settings machine rows; the current physical device is also omitted
  by matching its authenticated daemon token, so it cannot duplicate local work.
  Project placement and provider connections reach managed workers automatically.
  Worker placement matches the raw account baton holder to the typed keeper
  `worker-{worker_id}` host, retaining the full host ID for routing.
  Background account reads retain confirmed cosmetic
  plan branding while pending, so reconciliation cannot move the workspace rail.
  Device-only aliases never fall back to SSH: open windows retain that source in
  windows.json, including across sign-out/restart; only SSH hosts have that fallback.
- System-browser sign-in binds an ephemeral IPv4-loopback callback, verifies
  PKCE state and Host, bounds requests and allows 15 minutes for sign-in plus
  MFA. Waiting does not hold the account operation lock. Restart/cancel closes
  the old listener and fences its result; finishing credential activation is
  serialized and cannot be interrupted by UI cancellation. Successful browser
  returns unhide/show/unminimize the originating managed window; if it closed,
  an existing Home (or a new Home while the shell is still alive) receives the
  return. `pro-return` is targeted; `pro_take_return` consumes one generation-
  fenced pending route after the page registers its listener, covering startup.
  No custom URL scheme is registered: a fully exited app must be reopened.
  Keeper availability
  is independent of successful account authentication. Remote prompt, daemon
  bearer and account-token data must never appear in Settings host rows or logs.

- `pro_sign_in` accepts an optional closed `screenHint` (`sign-up` or `sign-in`,
  default `sign-in`) that changes only the hosted account entry screen. PKCE,
  callback validation, invitation checks and MFA remain the same. Signing in
  returns to plan review; checkout requires another explicit user action.

- Billing waits at most 15 minutes for the browser, then at most two minutes
  for checkout/account confirmation. A targeted plan review instead reconciles
  for 20 seconds after return: a fresh read without the target ends unconfirmed,
  while failed reads never claim an unchanged plan. Later account updates can
  still confirm the exact target. Opening, waiting and confirming are distinct states.
  A targeted portal request opens a hosted plan-change review; it never directly
  changes a subscription. Checkout and plan changes confirm the exact requested plan
  from a fresh authenticated account response; callback outcome is a hint.
  A 15 s account check while waiting also handles a lost browser redirect;
  return accelerates it to bounded 2–5 s checks. These share the account refresh
  mutex but never hold the operation lock during network requests or waiting.
  Replacing an attempt, canceling, sign-out and shell exit close its listener;
  terminal status remains available until explicitly acknowledged, replaced or
  signed out. The ordinary
  30 s reconciliation still discovers late webhook confirmation. No payment,
  return nonce or provider URL is stored in webview localStorage or app.json.

- **The command list is a lockstep** the type system does NOT fully enforce, so a
  drift only surfaces at runtime: `shell.rs` `generate_handler!` ↔
  `command_manifest.rs` (consumed by `build.rs` + runtime grants) ↔ committed `permissions/autogenerated/*.toml`
  (plus the `native.ts` wrappers + event-name strings). Add/rename a command in all
  of them, then `npx tauri build` (the app.yml path) to confirm they agree.
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
- Signing is a release-only concern (`TAURI_SIGNING_PRIVATE_KEY*`); the PR build
  (`app.yml`) needs no key.
