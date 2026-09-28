# Chimaera Pro connections

An optional account connection in the native app. It keeps remote hosts reachable
through an authenticated keeper, relays SSH login prompts, and offers the local
daemon to other signed-in devices. Ordinary SSH connections and the free daemon
continue to work without an account.

**Status: partial.** This page covers the native account and connection surface.
Cloud setup controls are implemented; automatic handoff and browser access are being verified before acceptance. Native phone apps remain separate.

## How it is used

1. Open **Pro** from Home or the first **Chimaera Pro** group in Settings.
   Paid accounts can also click the compact plan badge beside the workspace name.
   It opens a dedicated account page; ordinary app settings stay separate.
   In an account browser, the cloud machine opens the same agent-connection
   flow; other machines link to the account surface. Ordinary daemon browser
   windows have no Pro entry. A build with no configured endpoint shows only
   “Chimaera Pro isn't available in this build.”
2. With an endpoint configured, choose **Sign up**, or the smaller
   **Already have an account? Sign in** link. Each opens its corresponding
   identity-provider screen. Complete the system-browser authentication; the app receives an authorization code through its loopback callback.
   The app waits up to 15 minutes for sign-in and verification. While waiting,
   **Start again** opens a fresh sign-in and **Cancel sign-in** closes the request.
   An expired or failed request offers **Try again**. The browser confirms success
   only after the account is active in the app; keeper provisioning can finish later.
   Successful sign-in returns to Pro in the initiating app window, restoring it
   if minimized. A newly opened window consumes its pending return after loading.
   On restart, saved account access shows its current phase immediately. If the
   system Keychain needs a response, the page says so while Home stays usable;
   billing and account changes wait until initialization completes.
3. Signed-out and confirmed no-plan accounts see an illustrated introduction:
   start a session on your computer, continue a supported agent in the cloud,
   then access the same sessions, files and conversation on another device.
   Work continues locally when you’re back. The three distinct static scenes
   show an open laptop, an agent working above a closed laptop, and a shared
   project across devices; they are explanatory, not setup indicators.
   Local projects, agents and ordinary SSH remain free. Plan cards show pricing
   before checkout; **See plans** jumps directly to the comparison.
   Active subscribers see **Your Chimaera Pro** or **Your Chimaera Max** with
   account, cloud and project controls. They see no sales introduction or plan
   comparison. **Usage and plan details** shows the percentage of cloud work and
   mirrored-project capacity used, calculated from the service's current limits.
   Loading, unavailable and unknown account states stay neutral; a remembered
   plan selection never starts a purchase.
   The page shows the signed-in email and current plan. Without a plan, choose
   Pro or Max and monthly or yearly billing, then continue to checkout in the
   system browser. Both **Sign up** and **Sign in** remember that
   selection and returns to the plan page after account creation or sign-in.
   Checkout opens only after a separate, explicit purchase action from the
   signed-in account. An existing active plan restores the subscriber view.
   Existing subscribers can open **Manage billing**. Pro subscribers also see
   **Upgrade to Max**: choose monthly or yearly, then **Review upgrade in browser**
   to review the final price, proration and timing before confirming. Opening the
   review leaves the current plan unchanged and preserves any existing trial.
   Checkout and billing return
   to Pro automatically. The native shell waits up to 15 minutes for the browser,
   then checks checkout/account confirmation for up to two minutes, even if Pro
   is hidden or closed. A plan-review return instead checks for 20 seconds, then
   shows the current plan if fresh account reads have not confirmed the change.
   Failed reads remain unknown; a later confirmed update can still change the
   displayed plan. Billing-interval-only changes use **Manage billing**.
   Subscriber billing feedback stays in the current-plan card while
   cloud and provider panels remain in place. The page distinguishes opening,
   waiting, confirming and confirmed account states;
   a browser return alone never activates a plan. **Stop waiting** ends the local
   request without closing the browser page or canceling a payment or subscription.
   A stopped request can be dismissed or replaced by reopening billing. A timeout or failed
   check offers **Check account** and keeps plan selection hidden. If that fresh
   check confirms no active plan, **Return to plans** explicitly closes the old
   request before another checkout can start. Older shells
   without native attempt status retain an explicit account-refresh fallback.
   With an active plan, toggle **Keep connected** for a saved SSH host. A password
   or Duo challenge uses the usual
   host-scoped prompt, with “Asked by your Pro connection” underneath its title.
4. Open that host from Home. Its **via Pro** label identifies the connection;
   workspaces still open through a local loopback port with the existing daemon UI.
5. **Sign out** removes this app's credentials and closes its link connections.
   **Sign out everywhere** also revokes other devices and closes SSH logins held
   by the keeper.

A developer can configure `pro.endpoint` in the native app's `app.json` as
`{"pro":{"endpoint":"http://127.0.0.1:PORT"}}`. The file is under
`chimaera_core::config_dir()` (`$CHIMAERA_HOME/config` in an isolated development
run). It is separate from the daemon's settings JSON and takes effect when the app
starts. Tokens never belong in this file. Use the
[loopback fixture](../../crates/chimaera-link/PROTOCOL.md#fixture-and-conformance)
for a local integration run.

## Subscriber branding

An active Pro or Max account wears a small plan badge beside the Home wordmark and in the workspace header. The dedicated Pro page uses the same badge. The workspace badge opens the dedicated Pro page. There is no full-width Pro row in the workspace sidebar; free and signed-out users can still find Pro at the top of Settings and from Home. Settings offers **Get Pro** with a short cross-device benefit for confirmed free or signed-out accounts. Paid accounts see **Your Chimaera Pro** or **Your Chimaera Max** and **View account**. Loading or unknown account state stays neutral. The styling follows the current theme, and an unknown, signed-out or inactive plan shows no paid badge.

The shared `web-ui/src/lib/net/plan.ts` store exposes confirmed free/paid, loading and unknown account state to Settings, and derives the existing paid badge from the same subscription. It reads native `pro_status` and refreshes on `pro-changed` and visibility return. Account-browser windows instead make a bounded, same-origin `HEAD` request to their existing `/app/{host}/` index. Its optional `X-Chimaera-Plan` response header is `none`, `pro` or `max`, derived from the authenticated account's active or trialing subscription; an absent header or failed request leaves branding neutral. Index responses remain `Cache-Control: no-store`. The browser refreshes once per minute while visible and when returning to the page, without requesting or waking a keeper or worker. Ordinary daemon browser windows make no account request. The badge is presentation only and grants no capabilities.

`web-ui/src/lib/shared/PlanBadge.svelte` supplies the common visual treatment used by Home, the workspace header and `ProSettings.svelte`.

## Cloud readiness

An eligible subscription prepares its first cloud machine automatically. The Pro
page shows availability and the status of the user's work, rather than a checklist
or machine lifecycle. Ready and idle compute have the same available state.
Progress describes loading agent connections or preparing a named provider's
sign-in; it does not narrate starting or stopping infrastructure. Initial
preparation continues without a setup button; visible checks run
sequentially every five seconds, slowing to thirty seconds after five minutes.
Hidden views stop checking. Healthy phases do not ask the user to refresh.

A disabled service or uninvited preview account shows that preparation is waiting,
without an activity animation or a claim that files are synchronizing. A failed
read or unavailable connection offers a secondary **Check again**. Project status
appears only when mirror metadata exists: completed copies, handoff in progress,
restoration, or setup that needs attention. Saved-copy counts describe completed
copies, never active synchronization or a promise that every file is current.
No project or an unavailable check invents a project task or completion state.

Machine readiness and provider sign-in are separate. Once the cloud is reachable
and reports that an agent connection is needed, **Connect an agent to start cloud
work** presents the provider choice inline. One agent is enough to start; others
are optional. Connected users see a quiet **Agent connections** disclosure. A
required project connection, active sign-in, or connection error stays visible.
Collapsing management keeps the existing status poll and sign-in state intact.
The provider list comes from a shared catalog, initially Claude Code and Codex.
Installation alone never shows a provider as connected. An unavailable or timed-out
authentication check remains unknown.

**Connect** prepares a missing agent and then guides its supported sign-in flow.
Codex uses a one-time device code and the provider's secure browser page. Claude
Code opens its own browser sign-in page and returns a one-time code to the same
connection panel. Its official CLI completes authentication; no temporary project
or terminal window opens. The code is used once and never saved by Chimaera.
A new attempt brings its guide into view; background status updates never scroll
or reload the page. Installation remains automatic and separate from sign-in.
Users can cancel or retry an expired request in place. Sign-in is confirmed by the provider CLI on the cloud
machine, not by a local checkbox or simply opening a browser. This confirms the
configured account; it does not promise available provider quota or model access.
Provider credentials remain on the cloud machine and are never copied from other devices.

A paused handoff names the specific providers its sessions need. **Connect agents
to continue** opens the same flow with that project context. Every required agent
must be confirmed by a fresh catalog before that existing staged transfer continues
automatically. The UI tries each workspace/epoch once, sequentially, and offers
**Try again** after a failure. The daemon verifies ownership and provider state
again; the UI cannot release the setup fence or infer a new move. A canceled
connection leaves the project paused. A successful continuation returns to the
originating project only if that context is still current.

Status reads never wake a sleeping worker. Opening the optional **Agent connections**
disclosure loads those connections and acquires access automatically. There is no
separate cloud-start action. Connecting a provider or opening a repository also
acquires access as part of that user request. Catalog checks are
single-flight and visibility-gated. Active sign-in checks run sequentially every
two seconds, stop while hidden, and end at the attempt's finite deadline. Pending
connection operations keep the worker active only until they finish or expire.

Repository-provider connections remain optional. An HTTPS Git URL clones into the
worker's persistent projects folder and opens as a workspace. Duplicate names and
embedded URL credentials are rejected. Only one clone runs at a time; incomplete
clones are not registered. The cloud machine's SSH public key is under advanced
connections.

The worker exposes `/api/v1/pro/cloud`, `/api/v1/pro/cloud/providers`, provider
connect routes, connection read/cancel routes, and `/api/v1/pro/cloud/project`.
The native `pro_cloud_request` command keeps account and daemon credentials out
of the UI and opens only a server-owned connection's validated browser URL or
terminal. Browser clients use the same routes through the host-pinned gateway.
A shared provider catalog bounds external authentication origins in both clients.

## Where it lives

| Surface | Entry points |
| --- | --- |
| Pro account surface and native bridge | `web-ui/src/lib/pro/ProView.svelte`, `web-ui/src/lib/settings/ProSettings.svelte`, `web-ui/src/lib/net/native.ts` |
| Billing and local project adoption | `crates/chimaera-app/src/shell/pro/billing.rs`, `projects.rs`, `web-ui/src/lib/pro/CloudProjects.svelte` |
| Host and prompt labels | `web-ui/src/lib/workspace/HomeScreen.svelte`, `AskpassModal.svelte` |
| Cloud setup | `web-ui/src/lib/settings/CloudSetup.svelte`, `web-ui/src/lib/pro/ProviderConnections.svelte`, `crates/chimaera-app/src/shell/cloud.rs`, `crates/chimaera-server/src/cloud.rs` |
| App account lifecycle | `crates/chimaera-app/src/shell/pro.rs` |
| App connections and prompt routing | `crates/chimaera-app/src/shell/connect.rs`, `askpass.rs` |
| Device transport and wire types | `crates/chimaera-link/src/`, [protocol](../../crates/chimaera-link/PROTOCOL.md) |

Native IPC commands: `pro_status`, `pro_refresh_account`, `pro_billing_checkout`,
`pro_billing_portal`, `pro_cloud_status`, `pro_cloud_projects`,
`pro_open_cloud_project`, `pro_sign_in`, `pro_sign_out`,
`pro_sign_out_everywhere`, `pro_hosts`, `pro_set_host_kept`, `pro_devices`.
The app broadcasts `pro-changed` when account/host state changes. The panel
refreshes while visible and catches up when shown again; it does not poll while
parked. The mirror panel also uses `pro_mirror_status`, `pro_mirror_preference` and
`pro_set_never_mirror`, backed by authenticated `/api/v1/pro/` daemon routes.

## Project mirrors and automatic handoff

The signed-in app gives its local daemon a separate, limited account credential.
That credential stays in memory; the daemon can keep publishing mirrors after
all app windows close. Signing out stops publishing on that daemon.

Pro → **Projects and privacy** shows privacy, the last recorded copy, and actual
problems such as an incomplete copy or a required provider connection. Healthy
file counts, storage quotas, generic environment diagnostics, setup commands and
idle-session pins are not account controls; Chimaera and its agents manage those
details. Internal setup/profile and session-pin APIs remain available for their
scoped workflows.
**Keep this project on this device** stops local publication and disables account-side
mirror access. Existing stored data is not silently deleted. Cloud setup commands
and learned laptop-only commands appear in each project's details. A session's
**Keep running when idle** pin persists across transfer and daemon restart.

Repository history and working files are separate Git mirrors. Snapshot commits
use an independent index under the daemon's data directory; they never make WIP
commits on the user's branches. `.gitignore` and `.chimaeraignore` restrict the
working snapshot. Credential filenames, private keys, agent login stores and
secret configuration fields are excluded. Git configuration carries the
user's name, email and simple Git aliases with known flags. Free-form or shell
aliases, helpers, hooks, includes and signing credentials stay on their host.
Safe remote URLs, refspecs and branch tracking follow the repository. Conversations are complete native archives: text the user
or agent put in a conversation remains part of that archive.

The daemon renews a workspace ownership lease independently of mirror jobs.
A connectivity failure leaves local work running. A verified other owner fences
local input and stops its agents. On restart, previously owned projects wait for
ownership verification before old conversations can resume. A cold cloud takeover
forks native conversations; a clean handoff resumes their existing identities.
Imported sessions remain suspended while the complete handoff is staged.

Agents receive a current-host brief through MCP initialization; structured conversations with an interrupted turn or background work also receive it in their transfer pickup message. Finished structured conversations resume idle without starting a model turn merely because they moved or returned. Their fresh MCP context is available when the user next asks them to work. The brief identifies the OS and architecture, reports bounded runtime CPU/memory observations without confusing them with subscription quotas, and lists missing variable names and saved setup/deferred guidance as untrusted project data. Agents must check local resources and tools, use their own CLI sign-in, and reassess background work before restarting it. Ordinary Claude and Codex terminal sessions receive the same MCP context for configured cloud projects; their transfer still sends a positional context prompt because reliable active-versus-idle state is not yet recorded for every TUI provider.

`read_cloud_profile` and `update_cloud_profile` operate only on the authenticated session's registered project. Updates require the current revision, reject unknown fields and credential-shaped content, and are capped at 32 KiB. They keep ordinary agent permissions: saving `setup_command` schedules future cloud setup, while deferred laptop steps remain guidance. Saving a profile never runs a command or wakes a machine. Implementation: [`mcp/cloud_context.rs`](../../crates/chimaera-server/src/mcp/cloud_context.rs).

On macOS, the app's system sleep hook gives publication up to 25 seconds before
acknowledging sleep. A failed flush leaves the lease takeover path available.
After the laptop has been awake on AC power for five minutes, connected cloud
projects can return at an agent pause. Busy or unobservable agent states stay on
the cloud. Plain shells become paused placeholders in the cloud; arbitrary
foreground programs are not automatically relaunched.

Hand-back restores new branches and fast-forwards unchanged local branches. The
current checkout uses Git's index and ref locks; branches checked out in another
worktree are preserved separately. Diverged branch tips remain as
`<branch>@cloud-<commit>`, and Projects and privacy lists them for merging. Existing
remote configuration and `FETCH_HEAD` stay intact. If a file changed on both machines,
the local file and a sibling cloud copy both remain. Saved setup commands run in
a visible **Cloud setup** terminal. Deferred steps remain guidance for the returning agent, which assesses and runs them under its normal permissions; they are never automatically replayed by the daemon.

Projects first created in the cloud appear on Home without being downloaded.
Opening one on a computer without a local copy asks for an empty destination
folder through the native picker. Cancellation leaves the cloud copy untouched.
The confirmed destination is remembered for that project on that computer;
existing local projects retain their original folders. A missing destination or
an unrelated nonempty folder fails safely instead of overwriting data. Native
conversation identifiers survive when the local folder differs. The old global
projects-root preference no longer authorizes automatic imports. Both repository and shadow histories are retained
within the account quota; source history is never silently pruned. Initial Git
transfers have a bounded 16-minute deadline and remain cancelable.

Implementation: [`pro/`](../../crates/chimaera-server/src/pro/AGENTS.md),
[`MirrorSettings.svelte`](../../web-ui/src/lib/settings/MirrorSettings.svelte),
[`power.rs`](../../crates/chimaera-app/src/shell/power.rs), and the
[session bundle contract](../../crates/chimaera-server/BUNDLE.md).

## Continuing and viewing a remote session

A transferred session keeps its public ID, pinned title, linked terminals and
workspace tabs. The local workbench merges remote session rows under those IDs
and forwards their chat and terminal connections through the signed-in app.
An unavailable host stays visibly unavailable; opening its session cannot
silently start another local agent. Chat offers an explicit **Reconnect and
wake** action when the host is asleep or unreachable.

The terminal toolbar supports **Just watching**. Phones start there with the
sidebar collapsed: the existing server grid stays at its original size, readable
with horizontal scrolling, and the viewer cannot type or resize it. **Take
control** explicitly reconnects and fits the terminal to the current pane.
Passive roster polls, reconnects and watchers carry no cloud wake intent.

## Constraints and edge cases

- Default endpoint is unset. The Pro transport starts no sockets until explicitly
  configured and signed in; the existing SSH route remains the signed-out fallback.
- Account credentials live in the OS keychain. Refresh-token rotations replace the
  stored pair. Daemon bearer tokens remain in memory and do not enter `hosts.json`.
- SSH aliases resolve on the device. Only hostname, username and port are passed
  to the keeper; local private keys and arbitrary SSH configuration are not copied.
- HTTPS/WSS is required except for literal `127.0.0.1` fixtures. `/v1/me` negotiates
  the supported protocol and keeper origin. A newly signed-in account may still
  be awaiting an assigned keeper; account and device information remains available.
- Data bridges have bounded queues, 64 KiB frames and at most 128 streams.
  Closing one stream does not change the tunnel listener's port.
- Prompt answers remain scoped to the relevant host. Cancellation and expiry
  dismiss prompts in other eligible windows too.
- The local daemon is reverse-served only while the signed-in app owns that link.
  Signing out or quitting closes the offer. Device-host rows have no “Keep
  connected” toggle because their owning device controls availability.

---

## Intent — human-authored ground truth

_No intent captured yet — pending the maintainer's feature-intent review. The
implemented behavior above is derived from code, not a replacement for intent._

System sleep hooks give the daemon a bounded chance to flush: macOS uses IOKit,
Linux uses a logind delay inhibitor, and Windows uses the suspend callback's short
best-effort window. Wake restores heartbeats. Automatic hand-back waits for AC
power. Platform compilation and actual physical sleep are separate verification
gates; Linux and Windows physical sleep still need their own hosts.
