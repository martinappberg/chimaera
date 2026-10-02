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
| `main.rs` | The 3-role argv dispatch (order is load-bearing). The GUI role raises its open-file soft limit (macOS starts GUI apps at 256; every forwarded view costs two sockets). |
| `Entitlements.plist` | macOS hardened-runtime entitlements, applied by `tauri.conf.json` to the binary that also runs `--daemon`: Wasmtime's executable plugin memory, and `audio-input` for voice dictation's microphone. |
| `Info.plist` | Merged into the bundle's by the Tauri bundler: `NSMicrophoneUsageDescription` (voice dictation records in the web view; macOS kills an app that opens the mic without it). |
| `command_manifest.rs` | Shared daemon/wizard command vocabulary for build-time permission generation and exact runtime daemon grants. |
| `shell.rs` | Module root: app-global `Shell` state, `WindowScope`, `lock`, and the Tauri `Builder` assembly (`run`). Closing the last non-Home window opens local Home; closing the last local Home exits, while explicit Quit preserves restore state. A quit (`finish_quit`) or a close that would end the app (`closing_ends_app`) first passes the unsaved-edits guard, then `quit`'s question. Re-exports `open_ui_window`. |
| `shell/quit.rs` | Quitting (⌘Q, menu, tray, Dock › Quit/logout via the terminate hook) or closing the last window when that ends the app, while the daemon's `/pro/status` has a row with `working_agents` and `cloud_handoff` whose every working agent's own provider is signed in in the cloud (`Pro::cloud_agents`: signed in, set-up plan, hours left, the agent providers remembered signed in at the last catalog read; a project with any other agent is left out, and none left means today's quit without reading the daemon): a native dialog (**Keep working here** default = today's quit, **Continue in the cloud**, **Cancel**). Otherwise nothing changes; closing any other window never asks. Continue posts `/pro/sleep {deadline_ms, park: true, workspace_ids}` (`power::sleep_body`, 25 s budget) under the `assets/handoff.html` window, quits when the daemon answers `handoff` or the budget ends, and on failure shows "The cloud couldn't take over…" with Quit (its close = quit now). A launch with a `parked` row posts `/pro/wake` (`welcome_back`). The status read at quit is bounded to 1.5 s (on macOS's terminate hook it blocks the main thread that long at most); pure decisions are unit-tested. |
| `assets/handoff.html` + `capabilities/cloud-handoff.json` | The "Sending your work to the cloud…" page: names via an initialization script, failure via `eval` after the page-load event; its one grant is `core:window:allow-close` on label `cloud-handoff` (the shell turns that close into the quit). |
| `shell/commands.rs` | The IPC command surface (`#[tauri::command]` fns wired into `generate_handler!`) — thin delegators. |
| `shell/connect.rs` | The `connect` flight state machine (one coalesced ssh attempt per host; a flight for a wedge suspect — or with no live tunnel — first clears a wedged ControlMaster, before the old tunnel's teardown — both masters for an alias routed to its daemon's login node) + the host-row wire vocabulary (`HostState` — incl. `node`, the login node a pool alias is pinned to — /`HostStatus`, and the `routing` progress phase) + `with_hosts`, the app's single path to hosts.json (serialized, off the reactor via `spawn_blocking`; the CLI writes it directly in crates/chimaera/src/connect.rs). A direct or capability-confirmed kept SSH connect that finds Slurm returns the cluster page without starting a login-node daemon; `login_serve` and `not_cluster` remain explicit policies. Launch restore only attaches an already connected keeper route; explicit Connect/Reconnect owns new authentication. Kept transport failures never silently choose direct SSH. A flight whose keeper row is the account's cloud is refused before any reconnect (nothing wakes it) and drops that alias's saved windows (an older build's), so launch restore skips them with a log line. |
| `shell/cluster.rs` | Clusters: the `cluster_*` commands behind the cluster page — overview, discovery, add/remove a workspace, the folder picker (`cluster_list_dir` → `chimaera browse --dir`), start/continue/stop/dismiss a job (`sbatch`, or an attached `srun` this app holds), open/close/move a workspace in a job (job-host's API over a plain `ssh -L`; one forward per job-host and per workspace window), startup commands (the cluster's Environment scopes), rules for agents, the login-node override, "not a cluster" (`set_not_cluster`: the host connects like any remote), the login-node terminal (a terminal-only `term=` window, `web-ui/src/lib/terminal/TerminalWindow.svelte`, on a shell in the local daemon's hidden workspace; never restored, its session ends with the window and a crash's leftover at the next launch — `sweep_terminals`). Every read lays each running job's own job-host answer over the cluster folder (`overlay_live`; the folder can show an open or a close a minute late), and a workspace window opens straight into its workspace (`ws=`). Per-cluster live state (endpoints kept Rust-side, the notification diff), `absorb` (toasts: ready / an hour / ten minutes / stopped; a window whose workspace closed, failed, or whose job ended learns why; one on its way to another job is told "moving" and reopened there), and the watcher that runs only while a job this app knows of is alive (woken at the end-of-job marks). A connect that lands on a cluster (`ClusterHost`) is a success in `shell/connect.rs` (`landed_on_cluster`), never a daemon start. |
| `shell/cloud.rs` | Passive cloud/provider readiness, explicit bounded connection/retry actions, shared-catalog authentication URL validation, memory-only Claude authorization-code submission and explicitly acknowledged cloud-provider disconnection under the account-operation fence. There is no terminal operation (an `open_provider_terminal` request fails to parse): an older cloud's GitHub login terminal is never opened, and the page says the cloud is being updated instead. Account credentials remain in Rust. Polls never wake a worker. A wake wait uses the keeper-fed worker row until it fails a live check, then reads the account's host list first (the cached row is the fallback when that read fails), so a stale row with the event stream down cannot time out every wake. `pro_cloud_status` passes through optional account-confirmed preparing phases (`keeper`, `worker`, `connecting`); these are not daemon/provider readiness. `pro_cloud_status` also carries, from `shell/pro/agents.rs`, `agents_connected` and `remembered_providers` (remembered from the last catalog read, never probed) and `cloud_ready_once` (it marks the account ready when the account says ready or sleeping or lists a registered cloud daemon, so a later `preparing` is not first-time setup), all additive. `pro_cloud_request` answers the fixed code `cloud_asleep` when the cloud machine is asleep or still starting (503 `worker_asleep`/`worker_unavailable`, a reply marked `X-Chimaera-Worker-State: sleeping`, or a wake wait that ran out), which the page shows as a quiet state, never the generic failure. |
| `shell/power.rs` | System sleep/wake notifications (macOS IOKit, Linux logind delay inhibitor, Windows power callbacks). `/pro/sleep` carries `{deadline_ms}`: the platform's real budget (25 s, logind's `InhibitDelayMaxUSec`, 1.2 s) minus a margin (`sleep_body`, also the quit handover's body). Reports AC power; the daemon applies the hand-back gate. Never waits for Pro startup. |
| `shell/pro.rs` | Optional account runtime: endpoint in app.json, OS-keychain tokens, PKCE loopback sign-in, bounded keeper cache/events, reverse local-daemon sharing, daemon setup (`configure_daemon`, `DaemonStamp`), sign-out, the fixed status codes (`code`: connection states, `service_unsupported`, the ended-sign-in codes `sign_in_timed_out`/`sign_in_incomplete`/`browser_unavailable`, and `sign_out_pending`/`sign_out_unpersisted`), and account IPC. `pro_set_never_mirror` succeeds once local copying stopped; the account's confirmation is reported by the daemon's `privacy_pending`, never as an error. No endpoint means no keychain access or network work. |
| `shell/pro/catalog.rs` | The public plan catalog (`GET /v1/plans`, no credential): display prices (and each plan's optional whole-number `cloud_time_multiple`/`storage_multiple` relative to Pro, passed through untouched; no absolute allowance ever reaches the app) for a signed-out page. Fetched in the background at app start, after a sign-out and from a status read that finds it stale (answer reused 5 min, a failure retried after 1 min, the last good list kept); never blocks the status, never sets an error or warning, no endpoint means no request. `status_snapshot` prefers a signed-in account's own `plans`. |
| `shell/pro/billing.rs` | Native-owned checkout/portal attempts, exact provider-origin validation, cancellation, and authenticated plan confirmation independent of page visibility. |
| `shell/pro/billing/callback.rs` | One-use billing return: literal loopback Host, exact nonce/outcome query, bounded request/response and credential-free browser page. |
| `shell/pro/projects.rs` | Passive synced project listing and native destination selection ("Choose where to save …"): The distinct `pro_copy_project` IPC posts `/pro/projects/copy` with `copy_version:1` and requires an exact workspace/root/name + `local_copy`/`owned_local` acknowledgment before opening. It never falls back to `/open` or an older native Open command; `pro_open_cloud_project` remains an alias to safe copy for older pages. Existing copies refresh through the saved binding; a sent request never deletes its destination on error because enrollment may already bind its inode. Passive catalog rows may omit host identity. `pro_take_over_project` is a separate local-only account command, carries the authenticated ready-copy owner epoch (or a verified Remote epoch from an older marker) and requires an exact `owned_local` acknowledgment. Both actions are serialized, account-generation fenced and return fixed codes only; their admission stays with an in-flight blocking HTTP request even if the native caller is canceled. Empty picker choices are used; nonempty parents receive a new named folder; cancellation creates no request. |
| `shell/pro/placements.rs` | Workspace-specific reconciliation from the daemon inventory. Routes retire only on a definitive answer; a failed check keeps the last verified route for `ROUTE_STALENESS` (150 s) while its transport is open. Retired projects do not discard a healthy shared transport. |
| `shell/pro/machine.rs` | Friendly device label from macOS ComputerName; never an identity or network-hostname grouping key. |
| `shell/pro/store.rs` | Credential namespace keys: isolated previews bind session entries to their canonical config directory; legacy unbound entries are never imported or deleted. |
| `shell/pro/recovery.rs` | Generation-fenced startup candidate retention and coalesced retry ownership; account probe failures do not force new authentication. |
| `shell/pro/credentials.rs` | Coalesced, generation-fenced credential-store writes with finite retries; persistence failures retain valid memory sessions, and only true revocation (the link's `AuthorizationRevoked`: any refresh 4xx except 404/408/429) signs out. |
| `shell/pro/auth.rs` | In-memory sign-in attempt lifecycle, cancellation/retry fences and bounded loopback callback parsing. A timed-out wait is `code::SIGN_IN_TIMED_OUT`; the browser's failure page names the app's **Sign in** button. |
| `shell/pro/agents.rs` | What the app remembers about an account's cloud, one account at a time in `pro-agents.json` (written only on change, 16 KiB cap): whether an agent was connected at the last provider catalog read and which agent providers were signed in (`signed_in`), that read's bounded provider rows (`providers`, shown by the Pro page until a live read answers), and whether the cloud has ever been ready (`ready_once`; an older file's catalog fact implies it). All additive: an older file reads back with none. A catalog that cannot tell forgets the agent fact, never readiness. A hint for a sleeping cloud and for which working agents the quit question may offer to move; never probed, never permission to move work (only the user's choice moves anything). |
| `shell/pro/signout.rs` | An unfinished sign-out (saved sign-in neither deleted nor revoked): the synced `pro-sign-out.json` marker keeps it from being restored, also across restarts; damaged/unreadable markers also suppress restoration until saved credentials are deleted or a new sign-in replaces them. Revocation retries with backoff (30 s → 10 min) while the account is unreachable, deletion is tried once per revocation and per launch (a locked store may prompt). A newer sign-in (generation change, saved pair) supersedes it. Failed marker persistence returns `sign_out_unpersisted`: memory stays signed out, but restart safety is not promised. |
| `assets/sign-in.html` | Credential-free browser return page; success is sent only after native account activation. |
| `shell/tunnel.rs` | App-only SSH / keeper transport wrapper. Both expose one loopback daemon endpoint; keep chimaera-link out of the daemon dependency graph. `app_host`: the account's cloud (`HostKind::Worker`) is never one of the app's hosts; `offers_daemon_update` never flags it outdated (the service updates it), while SSH hosts and other computers keep the note and the update toast. |
| `shell/restore.rs` | `open_ui_window` (every window opens through `open_shell_window`, which refuses the account's cloud: the app never shows its own page), the tunnel health monitor (a 3 s `interval` tick — a down host's probe burns its 2 s timeout inside it; 3-miss hysteresis; confirmed-down keys back off per miss up to 10 ticks, compute keys 2; a `down` edge files a wedge suspect), and launch-time window restore. |
| `daemon.rs` | Launch/adopt the local daemon; version/parity policy (unix: spawn-self; windows: delegates to `wsl.rs`). |
| `wsl.rs` | The WSL2 engine: registry-first detection + version gate, hardened wsl.exe spawns, the persisted target (distro + PINNED `-u` user — wsl.json), provision/replace/spawn/probe/stop, connect wiring (pure parts unit-tested on any host; e2e via wsl-smoke). Startup only ADOPTS; anything that provisions runs in the wizard, visibly. |
| `assets/setup.html` | The Windows first-run wizard (shell-local page — no daemon origin exists yet to serve the real UI). |
| `ssh_agent.rs` / `ssh_agent/` | Default-off `ssh-agent-prototype`: native-selected host/key policy, strict SSH session-bind and hostbound userauth parser, CA principal/type/time checks, cryptographic host and returned-signature verification, bounded per-grant connection/session/request replay fences and exact local Unix-agent exchanges. Its caller-owned control loop has a 16-frame ceiling and cancels pending touch/signing on channel loss, account cancellation or absolute expiry; it never reconnects or replays. Native selection reads a bounded local SSH configuration snapshot, copies only public known-host trust through exact OpenSSH hashed/pattern lookup, and enumerates the selected Unix agent. IdentitiesOnly intersects available agent keys with public IdentityFile siblings/certificates without loading private files. The native effective host-key, user-signature and CA-signature algorithm lists constrain selection and every bind/sign; the keeper cannot relax them. Unknown/revoked trust and unsupported routing/trust directives refuse; first-host approval and loading an empty native agent remain unfinished. Explicit Connect/Reconnect owns a short grant after native selection, waits for Ready before reconnecting, and pumps verification until the host is ready (including a cluster without a login daemon). Account replacement/sign-out cancels pending attempts; four shared permits also bound detached five-second grant cleanup across account changes. Startup restore never creates a grant. Empty/unavailable agent takes the separate existing explicit password/MFA path before any signing grant; a failed signing attempt never downgrades. Real synthetic constrained/locked OpenSSH-agent fixture is explicit/ignored; prototype remains default off and does not advertise service readiness. |
| `askpass.rs` | The `SSH_ASKPASS` ↔ shell wire protocol (in-app password/Duo prompts). Keeper prompts use the same host/window scope and bounded pending table, with an explicit source and replies confined to the original events connection. Local transport: unix socket (unix) / token-gated loopback TCP fed through a WSL-interop wrapper (windows, installed by `wsl::wire_connect`). |
| `appearance.rs` | Bounded per-host first-paint palette cache outside volatile daemon origins; persisted atomically and carried in native window URLs. |
| `shell/commands.rs::open_external` | The ONLY route a rendered link has to the user's real browser: the navigation guard admits just the daemon origin (plus iframes' inline `about:srcdoc` documents — WebKit asks the policy for subframes too, so without them slides, the live HTML preview and notebook HTML output draw blank) and nothing receives a `target="_blank"`, so an external link is otherwise swallowed. **http/https only** — hrefs are agent-authored and the platform opener would act on `file:`/app schemes. |
| `shell/unsaved.rs` | Window close / app quit never drop unsaved editor text: the page pushes its unsaved count (`report_unsaved`), so `CloseRequested`/quit decide synchronously; a held one asks the window (`unsaved-prompt` → `reply_unsaved`). Pure `Guard` (unit-tested) + glue; a 4 s hung-page timeout and a third-ask escape keep it from ever trapping the user. Also the macOS `applicationShouldTerminate:` hook (Dock › Quit, logout never reach `ExitRequested`). |
| `shell/notices.rs` | Notices → OS notifications: one long-poll watcher per open daemon (`GET /api/v1/notices`), suppression for what the focused window shows (`report_window_view`), one-alert-per-session supersede/withdraw, click routing (`focus-session` + `take_pending_focus`), the Dock badge/bounce and tray counts. Feature: [notifications.md](../../docs/features/notifications.md). |
| `shell/print_frame.rs` | macOS: a page's `window.print()` (the slides' and documents' print buttons, each in a one-off same-origin iframe). WKWebView drops it unless the UI delegate answers WebKit's `_webView:printFrame:pdfFirstPageSize:completionHandler:`; this adds that method to wry's delegate class at runtime, re-assigns the delegate (WebKit reads its hooks on assignment), and prints just that frame through the system panel as a sheet. The completion runs only when the panel is done — the page's `print()` blocks until then, and a synchronous run deadlocks. SPI with a fail-closed `respondsToSelector:` check. |
| `notify.rs` | The platform notifier: macOS `UNUserNotificationCenter` + click delegate + dock tile (needs a signed `.app` — unbundled dev builds degrade to none), `notify-rust` on Linux/Windows. Identifiers encode the click route. |
| `windows.rs` | The per-window registry (round-trips window↔workspace). |
| `update.rs` | The auto-updater intent chain (consume-once, expiry) + the kept outcome of every signed-update check (`status`/`check`, behind `app_update_status`). |
| `menu.rs` | The menu bar. Page-owned items reach the focused window as a `menu` event; Reload Window instead evaluates a fixed script (`RELOAD_WINDOW_JS`) that calls the page's reload hook, so a page that never booted (no listener) still reloads. |
| `tray.rs` | The menu-bar / system-tray status item (`tray-icon` feature). |

## Invariants / gotchas

- Logical project viewing uses passive placement and exact target scope acknowledgment.
  It follows the current owner (home device or worker), including files and watches,
  without acquiring or moving execution. Reconciliation reads the daemon’s redacted
  placement inventory, retires each workspace only on a definitive answer
  (including after native restart), keeps the last verified route through a
  failed check for at most 150 s, and keeps healthy siblings on shared
  transports. Native window layouts remain local and
  only the fixed `/project` alias crosses hosts. See the
  [viewer contract](../chimaera-link/VIEWING.md).
  `shell/pro/installation.rs` keeps the stable installation proof in an
  endpoint/account/configuration-scoped Keychain entry. It must save before bind;
  clean release/recovery acknowledgment precedes a rebind that revokes the old
  device. Recovery is attempted only for this installation's old *device*
  holder (never a cloud machine), and one project's failure is logged rather
  than aborting the rebind; the final bind decides. Neither daemon nor webview
  receives the proof.
- Device rows group only account-verified installation IDs. Legacy sign-ins remain
  individually removable; the native revoke command refreshes the roster under
  the account-operation fence and rejects removal of the current sign-in.
- Account credentials belong only in the OS keychain (macOS Keychain, Windows
  Credential Manager, Linux Secret Service). app.json stores only the endpoint;
  hosts.json stores the account preference cache `kept` and this computer's
  independent `direct_ssh` choice. `set_host_direct_ssh` persists only: existing
  tunnels/jobs stay intact until reconnect. A direct SSH choice bypasses known
  keeper rows while signed in; device identities can never fall back to SSH.
  Keeper host rows are authoritative otherwise. A 30 s bounded reconciliation discovers new
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
  manufacture UI responsiveness. Startup releases readiness on every exit path
  (a drop guard), including when a sign-in or sign-out supersedes it.
  Cloud provider credential submissions and disconnections move the owned
  account-operation guard and tunnel into the blocking HTTP sender. Canceling
  the IPC caller cannot let sign-out finish while that request is still active.
  Status never carries raw error text. `error` is a failure needing the user or
  a fixed code (`account_restore_*`, `account_credentials_unsaved`,
  `service_unsupported`); additive `connection_warning` is an informational
  code (`connection_preparing`, `connection_retrying`, `account_unreachable`)
  cleared by a live keeper host event; additive `payment_due`, `returning_until`,
  `keeper_restart_at` and `plans` come from the account's optional `/v1/me` fields (`plans` falls back to the public
  catalog in `shell/pro/catalog.rs` until an account answer carries its own). `plan` is only `none`, `pro`,
  `max` or null: a plan this client cannot name (`Plan::Unknown`) reports null,
  the neutral state, and checkout accepts only Pro or Max. IPC errors are fixed sentences.
  Ordinary unkept SSH uses `client_now()` and never waits on Pro startup.
  A kept SSH host waits at most `KEPT_STARTUP_WAIT` (10 s); an unavailable
  account/keeper leaves it disconnected with explicit advanced direct-SSH
  guidance. Only device aliases wait for readiness unbounded. Every selected
  keeper-route failure stays on that route; direct SSH requires the saved
  per-computer preference. Launch restore attaches only an already connected
  keeper host. Explicit Connect/Reconnect may start authentication, waits out
  temporary status-read failures (`KEEPER_LOGIN_WAIT`, 180 s), and probes a
  newly connected daemon for at most `KEEPER_PROBE_WAIT` (30 s).
- **Daemon setup is off the activation path.** Activation installs the account,
  keeper events and the reconcile loop and returns (the browser is answered
  then); the loop's first pass configures the daemon. `DaemonStamp` includes the
  daemon token, so a same-port restart is set up again; a matching stamp is
  confirmed against `/pro/status` `configured`. A daemon that lost its setup gets
  a freshly minted grant (minting revokes the previous one); an unchanged daemon
  keeps its grant unless it expires within two hours. `update_local_daemon`
  reconfigures immediately. A service without v2 (`ServiceUnsupported`) is
  rechecked at most every ten minutes. **Only sign-out unconfigures**: no plan or
  no keeper pauses setup and retires keeper-routed views but never sends DELETE
  `/pro/configure`. Sign-out retries that DELETE, deletes the saved pair before
  clearing memory, and revokes this device when either fails, so a daemon or a
  leftover pair can never keep a signed-out account alive. When both fail
  (`sign_out_pending`), `pro/signout.rs` finishes it: the pair is never restored
  and is revoked, then deleted, once the account answers. `pro::stop` on quit
  leaves the daemon configured by design (copying continues), but reverse serve
  ends with the app: other devices cannot open this computer's projects while
  it is closed. The keeper events consumer reopens its connection if the
  channel ever closes.
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
  serialized and cannot be interrupted by UI cancellation. The browser is
  answered right after activation; focusing the return window does not take the
  operation lock (its target is generation-bound). Successful browser
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
  be revisited after reuse. The quit handover page is the second static
  capability (`capabilities/cloud-handoff.json`): no command, only closing its
  own window.
- **Window/quit lifecycle, in order.** A window close runs `CloseRequested`:
  the handover window's close is "quit now"; otherwise the unsaved-edits guard
  (`unsaved`) holds it first; only a close that passed it and would end the
  app (`closing_ends_app`: the only window left, and not a workspace, remote
  or torn-off window, whose last close opens Home) reaches `quit`, which may
  hold it too and later `destroy()`s it (never `close()`, so neither guard
  asks twice). A quit reaches `finish_quit` only after the unsaved guard; that
  is where `quit::hold_quit` may hold it (a held quit comes back through
  `finish_quit` once the gate is `Settled`). macOS terminate (Dock › Quit,
  logout) runs the same two in `unsaved::os_quit_may_proceed`, deciding
  synchronously. The two dialogs are never merged. The updater's
  `app.restart()` asks neither. The daemon outlives every quit (daemon.rs);
  only **Continue in the cloud** moves work, and nothing moves without that
  click.
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
