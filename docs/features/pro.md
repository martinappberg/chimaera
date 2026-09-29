# Chimaera Pro connections

An optional account connection in the native app. It keeps remote hosts reachable
through an authenticated keeper, relays SSH login prompts, and offers the local
daemon to other signed-in devices. Ordinary SSH connections and the free daemon
continue to work without an account.

**Status: partial.** This page covers the native account and connection surface.
Cloud setup controls are implemented; automatic handoff and browser access are being verified before acceptance. Native phone apps remain separate.

For the combined public implementation, review entrypoints, reproducible checks,
and remaining acceptance gates, see the [integration review guide](../agent-guides/pro-integration.md).

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
   as soon as the account is active in the app; keeper provisioning and setting up
   project copying on this computer continue in the background.
   Successful sign-in returns to Pro in the initiating app window, restoring it
   if minimized. A newly opened window consumes its pending return after loading.
   On restart, saved account access shows its current phase immediately. If the
   system Keychain needs a response, the page says so while Home stays usable;
   billing and account changes wait until initialization completes.
   A temporary account check failure retains the saved session, including refresh
   token rotation, and retries after 2, 5 and 15 seconds. **Check again** retries
   after those attempts or a failed credential-store read. A fresh authenticated
   account check is still required. When the account ends the sign-in (revoked,
   expired, or replaced by **Sign out everywhere**), Pro shows “Your sign-in has
   expired. Sign in again to continue.” and stops all background retries.
   SSH hosts never hang on any of this: a saved SSH host connects directly while
   Pro starts (a host kept connected through Pro waits at most 10 seconds for it)
   and whenever Pro is unreachable.
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
5. **Sign out** removes this app's credentials, removes the local daemon's Pro
   setup and closes its link connections. If the daemon does not confirm, or the
   saved sign-in cannot be deleted from the credential store, the app revokes this
   computer's sign-in on the account instead, so nothing keeps copying projects
   or signs back in on the next launch. **Sign out everywhere** also revokes other
   devices and closes SSH logins held by the keeper.

A developer can configure `pro.endpoint` in the native app's `app.json` as
`{"pro":{"endpoint":"http://127.0.0.1:PORT"}}`. The file is under
`chimaera_core::config_dir()` (`$CHIMAERA_HOME/config` in an isolated development
run). It is separate from the daemon's settings JSON and takes effect when the app
starts. Isolated previews keep separate credentials per configuration directory.
Upgrading an older preview requires one fresh sign-in; its old shared credential
entry is left untouched. The regular app keeps its existing account session.
Tokens never belong in this file. Use the
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

Agent and GitHub connections authorize use throughout the user's personal cloud,
within the access granted by that provider. The official **Connect** action is the
authorization step; connecting again is not required for each project or handoff.
Connected provider cards offer **Disconnect** when the worker supports it. The
confirmation names that provider and explains that active cloud tasks may lose
access. Disconnection signs the official CLI out in the cloud; computer sign-in is
unchanged. It does not stop existing tasks or promise to erase their cached tokens.
GitHub removes stored github.com logins only, not enterprise connections, and does
not revoke tokens at GitHub. Revoking the GitHub CLI application at GitHub can
also affect other devices. Failed or unknown verification stays recoverable and
never reports a completed disconnection. Reconnecting is a separate user action.

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
connect/disconnect routes, connection read/cancel routes, and `/api/v1/pro/cloud/project`.
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

Native IPC commands (the `LOCAL_ACCOUNT_COMMANDS` list in
`crates/chimaera-app/src/command_manifest.rs`): `pro_status`,
`pro_refresh_account`, `pro_sign_in`, `pro_cancel_sign_in`, `pro_sign_out`,
`pro_sign_out_everywhere`, `pro_take_return`, `pro_billing_checkout`,
`pro_billing_portal`, `pro_cancel_billing`, `pro_cloud_status`,
`pro_cloud_request`, `pro_cloud_projects`, `pro_open_cloud_project`,
`pro_mirror_status`, `pro_set_never_mirror`, `pro_hosts`, `pro_set_host_kept`,
`pro_devices` and `pro_revoke_device`. They are granted only to windows showing
this computer's own daemon; a remote host's, another computer's or the cloud's UI
cannot call them. The app broadcasts `pro-changed` when account/host state
changes. The panel refreshes while visible and catches up when shown again; it
does not poll while parked. Project privacy and copy status use authenticated
`/api/v1/pro/` daemon routes.

`pro_status` carries, additively: `error` (a failure needing the user, or a fixed
code), `connection_warning` (informational fixed code: `connection_preparing`,
`connection_retrying`, `account_unreachable`; work continues and Chimaera
retries), `payment_due` (the account reports a payment problem) and `plans` (the
account's offers with amounts, when the service supplies them; prices are never
hardcoded). `error` is `service_unsupported` when the Pro service does not
support this app version; project continuity then stays off (rechecked every ten
minutes) while local work, SSH and the account itself are unaffected.

## Project mirrors and automatic handoff

The signed-in app gives its local daemon a separate, limited account credential.
That credential stays in memory; the daemon can keep publishing mirrors after
all app windows close. Only signing out removes it (see step 5 above). A lapsed
payment or a keeper still being assigned pauses new setup but leaves the daemon's
setup and local work alone; the account refuses cloud copies without a plan. A
daemon that restarts or loses its setup is set up again with a fresh credential,
immediately after an in-app daemon update and otherwise within 30 seconds.
Quitting the app keeps the daemon copying, but other devices cannot open this
computer's projects while the app is closed: offering the daemon to them belongs
to the running app.

Pro → **Projects and privacy** shows privacy, the last recorded copy, and actual
problems such as an incomplete copy or a required provider connection. Healthy
file counts, storage quotas, generic environment diagnostics and setup commands
are not account controls; Chimaera and its agents manage those details. There
is no per-session placement pin: the native shell no longer exposes one.
**Keep this project on this device** stops local publication and disables account-side
mirror access. Existing stored data is not silently deleted. Cloud setup commands
and learned laptop-only commands appear in each project's details.

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

Agents receive a current-host brief through MCP initialization; structured conversations with an interrupted turn or background work also receive it in their transfer pickup message. Finished structured conversations resume idle without starting a model turn merely because they moved or returned. Their fresh MCP context is available when the user next asks them to work. The brief identifies device or cloud execution, the registered project root, OS/architecture needed for builds, headless limitations, and fresh cached provider observations. Absent or expired observations remain unknown; generating context never probes, logs in, wakes compute or sends a turn. Both MCP initialization and read_cloud_profile use this same projection, and returning to a device replaces stale cloud assumptions. Generated context omits topology, routing IDs, raw diagnostics, hardware allocations and credentials. User-owned profile content remains untrusted project data; missing variable names do not prove a dependency is unavailable. Agents should inspect actual tools and failures, use compatible headless or lower-resource alternatives within existing permissions, preserve completed work, and explain only meaningful progress or the specific user action needed. This prompt is product guidance, not an authorization or confidentiality boundary: agents can inspect their permitted environment and may infer where they run. It does not guarantee compliance or prevent all inference. Ordinary Claude and Codex terminal sessions receive the same MCP context for configured cloud projects; their transfer still sends a positional context prompt because reliable active-versus-idle state is not yet recorded for every TUI provider.

`read_cloud_profile` and `update_cloud_profile` operate only on the authenticated session's registered project. Updates require the current revision, reject unknown fields and credential-shaped content, and are capped at 32 KiB. They keep ordinary agent permissions: saving `setup_command` schedules future cloud setup, while deferred laptop steps remain guidance. Saving a profile never runs a command or wakes a machine. Implementation: [`mcp/cloud_context.rs`](../../crates/chimaera-server/src/mcp/cloud_context.rs).

The app's system sleep hook tells the daemon how long it has before the computer
sleeps (`POST /api/v1/pro/sleep {deadline_ms}`): 23 seconds of macOS's 25-second
wait, logind's configured delay minus a margin on Linux, and under a second on
Windows. A failed flush leaves the lease takeover path available.
After the laptop has been awake on AC power for five minutes, connected cloud
projects can return at an agent pause. Busy or unobservable agent states stay on
the cloud. A return attempt follows ownership changes caused by waking an idle
worker immediately, and imports its saved conversation before local work can
resume. A lost response is checked against current ownership without repeating
the same release request. Plain shells become paused placeholders in the cloud; arbitrary
foreground programs are not automatically relaunched.

Hand-back restores new branches and fast-forwards unchanged local branches. The
current checkout uses Git's index and ref locks; branches checked out in another
worktree are preserved separately. Diverged branch tips remain as
`<branch>@cloud-<commit>`, and Projects and privacy lists them for merging. Existing
remote configuration and `FETCH_HEAD` stay intact. If a file changed on both machines,
the local file and a sibling cloud copy both remain. Saved setup commands run in
a visible **Cloud setup** terminal. Deferred steps remain guidance for the returning agent, which assesses and runs them under its normal permissions; they are never automatically replayed by the daemon.

Projects first created in the cloud appear on Home without being downloaded.
Opening one on a computer without a local copy asks where to save it through the
native picker. An empty folder becomes the project folder; choosing a folder
that already has files (such as `~/Projects`) makes a new folder named after
the project inside it. Cancellation leaves the cloud copy untouched.
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
  stored pair. A temporary credential-store failure keeps a verified session
  signed in while the app retries saving. After bounded retries, an account notice
  explains how to check the credential store and retry; an unsaved session may
  require sign-in again after restarting. The account answers a revoked, expired
  or replayed refresh token with `400 invalid_grant`; the app treats that, and
  any other refresh refusal except a timeout or rate limit, as the end of the
  sign-in and never presents that token again. A network failure retries the
  refresh once with the same token and keeps the session. Daemon bearer tokens
  remain in memory and do not enter `hosts.json`.
- SSH never hangs on Pro: a saved SSH host connects directly while Pro starts
  (a kept host waits at most 10 seconds first, so the usual Pro route needs no
  new login), and a kept host whose Pro route is unreachable falls back to a
  direct connection (its row then reads as direct). Computers reached only
  through Pro wait for it.
- Viewing a project owned elsewhere survives a failed check (account or keeper
  unreachable, owner asleep or reconnecting) for up to 150 seconds on the last
  verified route; only a definitive answer (unowned, owned here, private, a newer
  owner) switches back to the local copy.
- SSH aliases resolve on the device. Only hostname, username and port are passed
  to the keeper; local private keys and arbitrary SSH configuration are not copied.
- HTTPS/WSS is required except for literal `127.0.0.1` fixtures. `/v1/me` negotiates
  the supported protocol and keeper origin. A newly signed-in account may still
  be awaiting an assigned keeper; account and device information remains available.
- Data bridges have bounded queues, 64 KiB frames and at most 128 streams per
  computer, forward and reverse together. Closing one stream, or a failed
  accept, does not change the tunnel listener's port. The app raises its
  open-file limit at startup.
- Service responses may gain fields and values; the app ignores what it does not
  know instead of failing (see the protocol's additive rule).
- Prompt answers remain scoped to the relevant host. Cancellation and expiry
  dismiss prompts in other eligible windows too.
- The local daemon is reverse-served only while the signed-in app owns that link.
  Signing out or quitting closes the offer, so a phone or another computer cannot
  reach this computer's projects while the app is closed. Device-host rows have no
  “Keep connected” toggle because their owning device controls availability.
- Pro account commands run only from this computer's own windows.

System sleep hooks give the daemon a bounded chance to flush and pass it the
real budget as `deadline_ms`: macOS uses IOKit (25 s), Linux a logind delay
inhibitor (logind's `InhibitDelayMaxUSec`, 5 s by default) and Windows the
suspend callback's short best-effort window (1.2 s). Wake restores heartbeats.
The app reports AC power to the daemon, which applies the hand-back gate.
Platform compilation and actual physical sleep are separate verification gates;
Linux and Windows physical sleep still need their own hosts.

---

## Negotiated execution authority

The opt-in v2 contract separates the preferred home installation from the viewing
device and current execution holder. Passive viewing never grants execution.
Managed processes need a fresh bounded execution lease; expiration fences input
and stops registered chat/terminal process groups. A clean handoff publishes an
immutable, acknowledged checkpoint before releasing ownership, and hydration
selects that receipt rather than a moving mirror head. Same-installation sign-in
recovery publishes stopped local work before replacing its old device binding.

Automatic recovery has a distinct canonical-checkpoint capability. After the
server lease expires and a bounded reconnect grace passes, the cloud can continue
from the acknowledged checkpoint with a forked native conversation and the same
logical session. The agent receives uncertainty context to inspect prior effects
before repeating them; known idle chats remain idle. This does not add a routine
human review step or promise exactly-once external actions. The old strict managed
mode remains supported and is never silently changed during a live grant.

Returning home adopts canonical cloud files and sessions after stopping its
registered managed work. Unsynchronized local conflicts are retained privately
outside the mirrored project within explicit storage bounds. Persisted unclean
launch evidence still blocks unsafe same-boot restart until supervisor cleanup;
process groups alone cannot contain detached descendants or guarantee OS-resume
ordering. Local non-Pro execution is unchanged; no machine chooser or new
secret-sharing capability is introduced.

## Project views follow the current owner

Opening the same project on a phone, browser or another computer does not move
execution away from its online preferred computer. Logical browser routes and
native project views resolve the
current owner passively, carry its exact workspace/epoch and preserve the same
session identity. Files, previews and watches follow that owner while native file
tabs retain their local presentation paths. A stale connection stops input and
reconnects; it never automatically sends the same action twice. The initial
project route admits registered project resources; host-wide configuration and
unbound browser proxy/worktree operations retain their ordinary local/SSH paths.
See the [wire and resource contract](../../crates/chimaera-link/VIEWING.md).

Account startup and preview packaging are independent of this routing. A macOS
preview must retain the built Mach-O as its actual bundle executable, bind its
preview environment before signing and pass deep/strict signature verification.
Replacing that executable with a script can make Keychain reject valid saved
credentials without a useful prompt; the [development recipe](../../.claude/skills/develop/SKILL.md)
now fails before launch on invalid signatures.

Device names use the computer's friendly system name. Account and devices shows
one row per verified native installation, with earlier unbound sessions under
**Other sign-ins**. These older rows may belong to the same computer; their
hostnames are not evidence of separate devices. Each other sign-in can be
removed explicitly without signing out the current one. Grouping never merges
or revokes credentials automatically.

## Intent — human-authored ground truth

### Viewing anywhere, execution at home while available
_Captured 2026-09-28 from the maintainer's instructions in this conversation._

- **Placement:** The maintainer clarified that cloud is the sync and handoff
  fallback. Viewing from a phone or browser should still execute on the home
  computer while it is available; the viewing device must not choose the host.
- **Continuity:** If the home computer becomes unavailable, including sudden
  battery loss, work should continue from the same project, session history and
  files in the cloud, then return locally when the computer is available again.
- **Experience:** The user should not need cloud-machine chooser controls or
  backend startup decisions. The system should handle placement automatically.

This records the intended experience. Recovery depends on durable checkpoints;
it cannot promise recovery of unsaved state or exact replay of external actions.
Existing ownership and publication fences must remain intact. Routing/failover
coverage and selected-project secret isolation are separate implementation gates.

### Cloud connections — scope and experience
_Captured 2026-09-28 from the maintainer's instructions in this conversation._

- **Problem it solves:** The maintainer asked why credentials stay on a device
  when the intended experience is to continue work automatically in the cloud.
  They explicitly requested implementation of the credential proposal with a
  subagent.
- **Current direction, open to improvement:** Asked to choose between selected
  projects and the whole personal cloud for custom secrets, the maintainer chose
  selected projects: “1, I think? but GitHub for instance feels like something you
  would allow for the full cloud box ?” This records the scope decision; it does
  not claim custom-secret sharing is implemented by the connection controls above.
- **Experience:** “I dont want this to meddle with the UI / UX and become unclear
  so the user doesn't know what to do”. Named service connections and project
  secret permissions must be understandable in the flow where they are needed.
- **Extension:** The maintainer explicitly wants other providers to fit later,
  giving Grok as an example. This addition is intended to evolve; no fixed
  provider list or immutable interface was requested.

_Broader Pro intent capture remains pending. The behavior above the divider is
derived from code; the statements here record the maintainer's instructions._
