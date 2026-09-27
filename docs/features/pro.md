# Chimaera Pro connections

An optional account connection in the native app. It keeps remote hosts reachable
through an authenticated keeper, relays SSH login prompts, and offers the local
daemon to other signed-in devices. Ordinary SSH connections and the free daemon
continue to work without an account.

**Status: partial.** This page covers the native connection and settings surface.
Cloud setup controls are implemented; automatic handoff and browser access are being verified before acceptance. Native phone apps remain separate.

## How it is used

1. Open Settings → Chimaera Pro in the native app. Settings is available from
   Home before opening a workspace, and through the native Settings menu. An account browser workbench has a Cloud machine category; ordinary browser
   windows have no account category. A build with no configured endpoint shows only
   “Chimaera Pro isn't available in this build.”
2. With an endpoint configured, choose **Sign in**. Complete the system-browser
   sign-in; the app receives an authorization code through its loopback callback.
3. The panel shows the signed-in email, plan, hosts and devices. Toggle **Keep
   connected** for a saved SSH host. A password or Duo challenge uses the usual
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

An active Pro or Max account wears a small plan badge beside the Home wordmark and in the workspace header. The native account panel uses the same badge in its identity card. The styling follows the current theme, and an unknown, signed-out or inactive plan shows no paid badge.

The shared `web-ui/src/lib/net/plan.ts` store reads native `pro_status` and refreshes on `pro-changed` and visibility return. Account-browser windows instead make a bounded, same-origin `HEAD` request to their existing `/app/{host}/` index. Its optional `X-Chimaera-Plan` response header is `none`, `pro` or `max`, derived from the authenticated account's active or trialing subscription; an absent header or failed request leaves branding neutral. Index responses remain `Cache-Control: no-store`. The browser refreshes once per minute while visible and when returning to the page, without requesting or waking a keeper or worker. Ordinary daemon browser windows make no account request. The badge is presentation only and grants no capabilities.

`web-ui/src/lib/shared/PlanBadge.svelte` supplies the common visual treatment used by Home, the workspace header and `ProSettings.svelte`.

## Cloud setup

In the signed-in panel, **Set up cloud machine** explicitly requests a worker.
Once it is ready, the panel opens the unmodified Claude, Codex and GitHub login
flows in ordinary terminals on that worker. Credentials remain on that machine.
The worker has its own SSH public key, which can be copied from the panel.
An HTTPS Git URL clones into the worker's persistent projects folder and opens
as a workspace. Duplicate names and embedded URL credentials are rejected.
Only one clone runs at a time; incomplete clones are not registered.

Passive cloud information reads do not wake a sleeping worker. An explicit setup
or login action does. A running clone counts as active work until it finishes.
The daemon exposes `/api/v1/pro/cloud`, `/api/v1/pro/cloud/onboard`, and
`/api/v1/pro/cloud/project` only on a worker. The native `pro_cloud_request`
command keeps both account and daemon credentials out of the UI. The account
browser uses the same daemon routes through its host-pinned gateway; its account
launcher provisions the first worker.

## Where it lives

| Surface | Entry points |
| --- | --- |
| Settings and native bridge | `web-ui/src/lib/settings/ProSettings.svelte`, `SettingsView.svelte`, `web-ui/src/lib/net/native.ts` |
| Host and prompt labels | `web-ui/src/lib/workspace/HomeScreen.svelte`, `AskpassModal.svelte` |
| Cloud setup | `web-ui/src/lib/settings/CloudSetup.svelte`, `crates/chimaera-app/src/shell/cloud.rs`, `crates/chimaera-server/src/cloud.rs` |
| App account lifecycle | `crates/chimaera-app/src/shell/pro.rs` |
| App connections and prompt routing | `crates/chimaera-app/src/shell/connect.rs`, `askpass.rs` |
| Device transport and wire types | `crates/chimaera-link/src/`, [protocol](../../crates/chimaera-link/PROTOCOL.md) |

Native IPC commands: `pro_status`, `pro_sign_in`, `pro_sign_out`,
`pro_sign_out_everywhere`, `pro_hosts`, `pro_set_host_kept`, `pro_devices`.
The app broadcasts `pro-changed` when account/host state changes. The panel
refreshes while visible and catches up when shown again; it does not poll while
parked. The mirror panel also uses `pro_mirror_status`, `pro_mirror_preference` and
`pro_set_never_mirror`, backed by authenticated `/api/v1/pro/` daemon routes.

## Project mirrors and automatic handoff

The signed-in app gives its local daemon a separate, limited account credential.
That credential stays in memory; the daemon can keep publishing mirrors after
all app windows close. Signing out stops publishing on that daemon.

Settings → Chimaera Pro → **Project mirrors** shows the projects, file counts,
storage budget, exclusions, missing cloud environment names and the last copy.
**Never mirror this project** stops local publication and disables account-side
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

On macOS, the app's system sleep hook gives publication up to 25 seconds before
acknowledging sleep. A failed flush leaves the lease takeover path available.
After the laptop has been awake on AC power for five minutes, connected cloud
projects can return at an agent pause. Busy or unobservable agent states stay on
the cloud. Plain shells become paused placeholders in the cloud; arbitrary
foreground programs are not automatically relaunched.

Hand-back restores new branches and fast-forwards unchanged local branches. The
current checkout uses Git's index and ref locks; branches checked out in another
worktree are preserved separately. Diverged branch tips remain as
`<branch>@cloud-<commit>`, and Project mirrors lists them for merging. Existing
remote configuration and `FETCH_HEAD` stay intact. If a file changed on both machines,
the local file and a sibling cloud copy both remain. Saved setup commands run in
a visible **Cloud setup** terminal. Deferred steps run in a **Deferred laptop
steps** terminal when the project returns; failed steps stay listed for attention.

Projects first created in the cloud return to the folder confirmed once in
Project mirrors (default `~/chimaera`). Native conversation identifiers survive
when the local folder differs. Both repository and shadow histories are retained
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
