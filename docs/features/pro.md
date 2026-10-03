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
   windows have no Pro entry. A build with no configured endpoint has no Pro
   entry on Home or in Settings and no plan badge; if the Pro page is reached
   anyway it shows only “Chimaera Pro isn't available in this build.”
2. With an endpoint configured, choose **Sign up**, or **Sign in** (beside
   **See plans** at the top, and as the smaller **Already have an account? Sign
   in** link under the plans). Each opens its corresponding
   identity-provider screen. Complete the system-browser authentication; the app receives an authorization code through its loopback callback.
   The app waits up to 15 minutes for sign-in and verification. While waiting,
   **Start again** opens a fresh sign-in and **Cancel sign-in** closes the request.
   A request that timed out, could not finish or could not open the browser
   leaves the person signed out: the plans stay, with one quiet line such as
   “Sign-in timed out. Start again when you’re ready.” (the app's fixed
   `sign_in_timed_out`, `sign_in_incomplete` and `browser_unavailable` codes,
   never an account failure). A failure page in the browser tells the person to
   return and choose **Sign in**, the same button. The browser confirms success
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
   Ordinary SSH hosts connect without waiting for Pro startup. A host kept
   through Pro waits at most ten seconds, then reports that its keeper route is
   unavailable. **Connect directly from this computer** in the host's advanced
   settings is the explicit alternative; outages never silently switch routes.
   Launch restore attaches an existing keeper connection. A new authentication
   exchange requires **Connect** or **Reconnect**.
3. Signed-out and confirmed no-plan accounts see an illustrated introduction:
   start a session on your computer, continue a supported agent in the cloud,
   then access the same sessions, files and conversation on another device.
   Work continues locally when you’re back. The three distinct static scenes
   show an open laptop, an agent working above a closed laptop, and a shared
   project across devices; they are explanatory, not setup indicators.
   Local projects, agents and ordinary SSH remain free. Plan cards show prices
   only when the service supplies them (optional `ProStatus.plans`); otherwise
   they name the plans and say prices are shown at checkout. Before sign-in the
   prices come from the service's public catalog (`GET /v1/plans`, no
   credential), read in the background at app start and again after a sign-out
   or when its five-minute answer has gone stale; a signed-in account's own list
   takes precedence, and an older service without the route just shows no
   prices. The Max card's last line says how many times more cloud time and
   storage it gives than Pro, as whole numbers from the service (optional
   `cloud_time_multiple` and `storage_multiple` on the plan's entries, the same
   on both intervals, so the monthly/yearly toggle never changes it: "5× the
   cloud time and storage of Pro" when the two are equal, each number named
   when they differ, only the one the service states otherwise;
   `maxCapacityNote`). Pro's card and an older service without the multiples
   keep the generic line under each plan. No absolute allowance is shown or
   built into the app, and no price is either. **See plans** jumps directly to
   the comparison.
   Active subscribers see **Your Chimaera Pro** or **Your Chimaera Max** with
   account, cloud and project controls. They see no sales introduction or plan
   comparison. **Usage and plan details** shows the percentage of cloud work and
   project storage used, calculated from the service's current limits.
   Loading, unavailable and unknown account states stay neutral; a remembered
   plan selection never starts a purchase. The page keeps showing the last
   confirmed account while background reads run (every `pro-changed`, window
   focus), so plans, the introduction and the overview never unmount or lose
   scroll; a purchase re-reads the account first when an update is pending. A
   connection that is still coming up, retrying, or cannot reach the account
   just now (optional `ProStatus.connection_warning`, or the two informational
   messages older shells put in `error`) is one quiet line for that state, with
   no button, never an account failure. An account whose payment needs attention
   (optional `ProStatus.payment_due`) sees **Payment needs attention** with
   **Manage billing**, and is never offered plans or checkout. An account whose
   plan has ended shows the plan badge as **Plan ended** (never a paid one, even
   if the account still names the plan) and, while it lasts, one quiet line: **Your
   plan has ended. Bring your work home from the cloud by** the date the account
   gives (optional `ProStatus.returning_until`). After that date the account
   refuses with `return_window_ended`, which reads as **The time to bring this
   work home has passed. Contact support.** No dialog, no warning colour.
   The page shows the signed-in email and current plan. Without a plan, choose
   Pro or Max and monthly or yearly billing, then continue to checkout in the
   system browser. Both **Sign up** and **Sign in** remember that
   selection and returns to the plan page after account creation or sign-in.
   Checkout opens only after a separate, explicit purchase action from the
   signed-in account. An existing active plan restores the subscriber view.
   Existing subscribers can open **Manage billing**. Pro subscribers near a limit
   (80 % of an allowance, a used-up allowance, or exhausted cloud hours) also see
   **Upgrade to Max**: choose monthly or yearly, then **Review upgrade in browser**
   to review the final price, proration and timing before confirming. Opening the
   review leaves the current plan unchanged and preserves any existing trial.
   When the service explicitly allows attended cloud actions after the monthly
   allowance, agent connections remain available and the page explains that
   cloud work pauses when interaction stops. Older services keep their existing
   allowance block. Storage, spending and subscription restrictions still apply;
   passive refreshes neither wake work nor count as interaction.
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
   request before another checkout can start; unrelated account updates do not
   withdraw it. A checkout refused because the account already has a plan re-reads
   the account and shows it. Older shells without native attempt status refresh
   the account when the window regains focus.
   With an active plan, toggle **Keep connected** for a saved SSH host. A password
   or Duo challenge uses the usual
   host-scoped prompt, with “Asked by your Pro connection” underneath its title.
   When the account plans a restart of the always-on cloud connection to update
   it (optional `ProStatus.keeper_restart_at`), one quiet line under **Connected
   machines** says so: **Your cloud connection restarts** tonight at 02:00 (the
   time in the person's own format: today, tonight, tomorrow, a weekday within
   the week, else a date) **to update. You’ll be asked to sign in to your cluster
   again when you next use it.**, naming the cluster instead ("sign in to
   Sherlock again") when exactly one login is kept. A time that has passed reads
   **restarts shortly**: the account restarts it as soon as no Git transfer runs.
   The restart drops the kept cluster logins, hence the sign-in. The line goes
   away when the account stops announcing the restart. No dialog, no warning
   colour.
4. Open that host from Home. Its **via Pro** label identifies the connection;
   workspaces still open through a local loopback port with the existing daemon UI.
5. **Sign out** removes this app's credentials, removes the local daemon's Pro
   setup and closes its link connections. Signing out doesn't stop anything on
   this computer. If a project is running in the cloud, a confirmation names it
   first: it stays there until the next sign-in. If the daemon does not confirm,
   or the saved sign-in cannot be deleted from the credential store, the app
   revokes this computer's sign-in on the account instead, so nothing keeps
   copying projects or signs back in on the next launch. When neither deletion
   nor revocation works (a refused credential store while offline), the person is
   still signed out: the page says “You’re signed out on this computer. The saved
   sign-in is cleared automatically next time you’re online.” A small marker
   (`pro-sign-out.json`) keeps that saved sign-in from being restored, also after
   a restart; the app revokes it once the account answers, then deletes it
   (`shell/pro/signout.rs`). The marker is synced before reporting that automatic
   completion is safe across restarts. If the app cannot save that marker either,
   it stays signed out in memory and explains that the saved sign-in could return
   after restarting; unlock the credential store, check the connection and free
   disk space, keeping the app open while cleanup retries. **Sign out everywhere** always asks first: it also
   revokes other sign-ins and closes the SSH logins held by the keeper.

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

## On the web

Signing in at the account's address in a browser lands in the same workbench the desktop app shows, not on an account page. The account serves this UI at `/` with a `chimaera-surface` marker (`net/base.ts` `isAccountHome`), and `main.ts` mounts `pro/AccountHome.svelte` there instead of the workbench, because no daemon stands behind that page. Home has the desktop's navigation and heading and lists **Cloud projects** (`pro/CloudProjects.svelte` in its browser mode): every project the account keeps in the cloud, by name. **Open project** goes to its project view (`/workspace/{id}/`, which follows the project to whichever machine runs it) or, for a project only a cloud machine has, to that machine's page. There is no This Mac section, no remote machines and no **Open folder**: those need a computer of your own. Nothing about plans or billing shows on Home.

Settings there holds only **Chimaera Pro** (`SettingsView` with `account`, which renders `pro/BrowserAccount.svelte` in place): the plan and its badge, usage as percentages of the account's own allowances, **Manage plan and billing** (**See plans** without a plan), which opens the account's billing page in the same tab, and **Sign out**, which signs out this browser only. Devices, other sign-ins and **Sign out everywhere** are on that billing page. Both reads are passive, same-origin and cookie-authenticated (`pro/accountHome.ts`; routes in the [protocol](../../crates/chimaera-link/PROTOCOL.md#account-home-in-a-browser)), and neither wakes a cloud machine. Home rereads its list every 30 seconds while visible (every 10 while part of it is still connecting); a session that ended returns to the sign-in page. Project views and host views are unchanged; their Pro page links to the same billing page.

In a browser project view, **Home** in the workspace header returns to the
account's project list. It stays available in the focus-mode strip on a phone
and does not wake or move work.

## Subscriber branding

An active Pro or Max account wears a small plan badge beside the Home wordmark and in the workspace header. The dedicated Pro page uses the same badge. The workspace badge opens the dedicated Pro page. There is no full-width Pro row in the workspace sidebar; free and signed-out users can still find Pro at the end of Settings and from Home. Pro is an optional add-on, so its Settings group follows every working section and its entry uses the same neutral style for everyone: **Get Pro** with one short optional-benefit line for confirmed free or signed-out accounts. Paid accounts see **Your Chimaera Pro** or **Your Chimaera Max** and **View account**. Loading or unknown account state, and an account whose payment needs attention without an active plan, stay neutral. A connection warning never clears the badge. The styling follows the current theme, and an unknown, signed-out or inactive plan shows no paid badge.

The shared `web-ui/src/lib/net/plan.ts` store exposes confirmed free/paid, loading, unknown and unavailable (no endpoint, or an ordinary browser) account state to Settings, derives the existing paid badge from the same subscription, and `proOffered` gates every Pro entry point (null until the first answer, so an endpoint-less build never flashes one). It reads native `pro_status` and refreshes on `pro-changed` and visibility return. Account-browser windows instead make a bounded, same-origin `HEAD` request to their workbench index (`/app/{host}/`, `/workspace/{id}/`, or `/` on the web's Home). Its optional `X-Chimaera-Plan` response header is `none`, `pro` or `max`, derived from the authenticated account's active or trialing subscription; an absent header or failed request leaves branding neutral. Index responses remain `Cache-Control: no-store`. The browser refreshes once per minute while visible and when returning to the page, without requesting or waking a keeper or worker. Ordinary daemon browser windows make no account request. The badge is presentation only and grants no capabilities.

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

Only an account's very first setup reads as setup: **Getting things ready**,
“Setting up your cloud. This usually takes a couple of minutes.” Once the
account's cloud has been ready (the app remembers it per account as
`cloud_ready_once`; the account listing a cloud daemon counts too, so a new
computer knows), a later `preparing`, such as a service update, reads exactly
like ready and idle: **Available when you need it** with the agents line. The
user never has to think about a machine: no sentence in Settings → Chimaera Pro
calls the cloud a machine or says it sleeps or wakes. Words about waking belong
only to the user's own action: a pressed button (“Connecting Claude Code…”) and
the chat and terminal lines for a sleeping project (“Asleep in the cloud. Send a
message to wake it.”). `pro/vocabulary.test.ts` scans the Pro and cloud settings
copy for it.

A disabled service says cloud work isn't available yet and that work on this
computer continues; an uninvited preview account says access is by invitation.
Neither shows an activity animation or claims files are synchronizing. A failed
read or unavailable connection says so and keeps checking on its own; there is no
manual check. One failed account status read in a row (the account service being
updated, say) is not reported: the page keeps the last confirmed state and its
calm copy and checks again within five seconds; only a second failed read in a
row shows “Cloud availability couldn’t refresh”. Project status
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
The provider list comes from a shared catalog: Claude Code and Codex, plus GitHub
under the optional **Repository connections**.
Installation alone never shows a provider as connected. An unavailable or timed-out
authentication check remains unknown.

**Connect** prepares a missing agent and then guides its supported sign-in flow.
Codex uses a one-time device code and the provider's secure browser page. Claude
Code opens its own browser sign-in page and returns a one-time code to the same
connection panel. Its official CLI completes authentication; no temporary project
or terminal window opens. The code is used once and never saved by Chimaera; it
must be pasted whole (Claude shows `code#state`), and half of one keeps the
sign-in waiting with its own error (`authorization_code_incomplete`).
Every sign-in shows inside its own row (the agent's card, or GitHub's line under
**Repository connections**), never in place of the other rows, so nothing else on
the page moves while it runs: the one-time code with **Copy** and one **Open
GitHub** (or **Open Codex**) button, a short "Waiting for you…" line and a small
**Cancel**; Claude's row shows **Open Claude Code sign-in** and the paste field.
The row itself then reports how it ended (connected, or the reason in plain words
with **Try again** as its button).
Connecting GitHub (it lets the user's cloud pull and push their
repositories) works like Codex, with no terminal: the row shows GitHub's
one-time code, **Open GitHub** opens GitHub's device page
(`https://github.com/login/device`), the user enters the code there and approves
access, and the row confirms the connection by itself. Behind it the cloud
machine runs the official GitHub CLI's web sign-in with piped I/O (never a
terminal or a browser there), reads only its code and page from complete output
lines, waits while the CLI polls GitHub, then makes the CLI Git's credential helper
for github.com (`gh auth setup-git`) and confirms with a fresh `gh auth status`.
Each ending reads in plain words: no code shown (`sign_in_unavailable`), a code
declined or left to expire (`sign_in_failed`), Git not set up (`git_setup_failed`,
where **Try again** repeats only that step), or the request's own time limit. On
success the panel says GitHub is connected and the user's cloud can now pull and
push their repositories.
A cloud the service has not updated yet still answers GitHub's **Connect** with a
login terminal on the cloud. Chimaera never opens it, in the app or in a browser:
that attempt ends quietly and the GitHub row says “Your cloud is being updated.
GitHub sign-in is available again in a few minutes.” with **Try again** in place of
**Connect**. Try again only looks (one passive catalog read, never waking the
cloud); **Connect** comes back once the cloud offers the one-time code, whether
Try again or the section's own catalog check sees it first. A catalog read that
finds such a cloud offering only its terminal sign-in reads the same way before
anything is pressed (`pro/providers.ts` `olderCloudSignIn`, `awaitingCloudUpdate`).
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

A conversation that waits for an agent sign-in names that agent (its paused row
carries an additive `blocked_provider`); the project and every other conversation
and terminal continue meanwhile. The paused conversation or terminal itself offers
**Connect <agent> to continue** (the name from the provider catalog) in the app or
an account browser view, where Chimaera Pro can open; a plain browser tab on a
daemon keeps the sign-in hint instead. Projects and privacy offers **Connect agents
to continue**; both open the same flow with that project context. Each waiting agent
must be confirmed by a fresh catalog before its conversations continue
automatically. The UI tries each workspace/epoch once, sequentially, and offers
**Try again** after a failure. The daemon verifies ownership and provider state
again; the UI cannot release the setup fence or infer a new move. A canceled
connection leaves those conversations paused. A successful continuation returns to the
originating project only if that context is still current.

Status reads never wake a sleeping worker. The app remembers, per account, what
its last catalog read showed (`pro-agents.json`, written only on change):
whether an agent was connected, the provider rows themselves, and whether the
cloud has ever been ready. `pro_cloud_status` carries them additively as
`agents_connected`, `remembered_providers` and `cloud_ready_once`. A browser view
of the cloud's own page keeps the rows in its local storage per cloud address
(`pro/catalogMemory.ts`, never credentials) and forgets them all on sign-out.
The page answers from that memory while the cloud sleeps: the check mark beside
“Available when you need it” appears only when an agent is on record; none
connected reads “Connect an agent below to start cloud work”; unknown claims
nothing. **Agent connections** shows whenever the account has a cloud, with the
remembered rows at once (Claude Code connected, Codex not connected, GitHub not
connected), and a live read replaces them silently. While a live read is pending
or finds the cloud asleep or starting, the rows simply stay; one muted
“Checking…” beside the section title is the most that shows, and only after five
seconds without a live answer. With nothing remembered yet (a new computer, say)
the rows are neutral placeholders until the look answers; if it finds the cloud
idle, the section names the catalog's providers (Claude Code, Codex, GitHub)
without claiming any state, each with its **Connect**, under “Connect an agent to
use it in the cloud. Agents you connected before stay connected.” A passive read that finds a cloud the account called ready
unreachable is re-checked with the account first (it usually just went idle)
and is reported as unavailable only on a second read in a row. A cloud that
answers that it is asleep or still starting (503 `worker_asleep`/
`worker_unavailable`, or a reply marked sleeping; both clients carry it as the
fixed code `cloud_asleep`) is a state: never an error, never counted as
unreachable, and never words on a passive path. Real failures keep their error
copy. Opening **Agent connections** is looking, not acting: it never wakes the
cloud. It makes one passive catalog read (`pro/cloudTransport.ts`
`peekCatalog`); a cloud that happens to be awake refreshes the rows silently,
and an idle one changes nothing. Only **Connect**, **Disconnect** and the
sign-in steps carry wake intent. Pressing **Connect** says “Connecting Claude
Code…” (or “Opening GitHub’s sign-in…”) on its button while the cloud comes up
behind it, asking again for at most two minutes before the usual failure copy. Connect
works from remembered rows; **Disconnect** waits for a live read. There is no
separate cloud-start action. Connecting a provider, or opening a repository on
the cloud machine's own page, also acquires access as part of that user request. Catalog checks are
single-flight and visibility-gated; they poll only while the cloud answers, and
opening the section adds one look. Active sign-in checks run sequentially every
two seconds, stop while hidden, and end at the attempt's finite deadline. Pending
connection operations keep the worker active only until they finish or expire.

Repository-provider connections remain optional. On the cloud machine's own page
(an account browser), an HTTPS Git URL clones into its persistent projects folder
and navigates to the new project (`/workspace/{id}/` in a project tab). The desktop
app opens synced local copies and offers **Take over** separately; it has no
cloud-page clone control. Duplicate names
and embedded URL credentials are rejected. Only one clone runs at a time;
incomplete clones are not registered. The cloud machine's SSH public key is under
advanced connections. In an account browser, Settings shows a **Cloud** section
only on the cloud machine's own page; other daemons have no cloud status to show.

The worker exposes `/api/v1/pro/cloud`, `/api/v1/pro/cloud/providers`, provider
connect/disconnect routes, connection read/cancel/input routes, and `/api/v1/pro/cloud/project`.
Agent sign-in only runs through those provider connection jobs (the older
login-terminal route `/api/v1/pro/cloud/onboard` is gone).
The native `pro_cloud_request` command keeps account and daemon credentials out
of the UI and opens only a server-owned connection's validated browser URL, in
the user's own browser. It has no terminal operation: an older cloud's terminal
sign-in is never opened, and the panel says the cloud is being updated instead
(above). Browser clients use the same routes through the host-pinned gateway.
The app never opens the cloud's own page: nothing connects to the cloud as a
host (`shell/connect.rs`), no window is created on it (`shell/restore.rs`), a
window on it saved by an older build is dropped at launch instead of restored,
and it never reads as outdated or gets an update offer, since the service
updates its daemon (`shell/tunnel.rs` `offers_daemon_update`). That daemon
never checks for its own releases either: every view of it says “Updates for
your cloud are managed for you.”, with nothing to check or install
([update awareness](lifecycle-and-persistence.md#update-awareness-daemon-side)).
Its agents are the cloud's too: they come with its image and are updated with
it, so the cloud never checks their releases or offers an agent update
(`launcher.rs`, `agent_updates.rs`), and Claude Code's own updater is off there
(`spawn.rs`).
A shared provider catalog bounds external authentication origins in both clients.

## Where it lives

| Surface | Entry points |
| --- | --- |
| Pro account surface and native bridge | `web-ui/src/lib/pro/ProView.svelte`, `web-ui/src/lib/settings/ProSettings.svelte`, `web-ui/src/lib/net/native.ts` |
| The web's Home and its Settings account section | `web-ui/src/lib/pro/AccountHome.svelte`, `BrowserAccount.svelte`, `accountHome.ts`, `web-ui/src/main.ts` |
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
`pro_cloud_request`, `pro_cloud_projects`, `pro_copy_project`,
`pro_open_cloud_project` (the compatibility alias),
`pro_take_over_project`,
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
retries), `payment_due` (the account reports a payment problem),
`returning_until` (an RFC 3339 time, or null: an ended plan's cloud work can be
brought home until then), `keeper_restart_at` (an RFC 3339 time, or null: the
always-on cloud connection restarts to update then, or shortly when the time has
passed) and `plans` (the
offers with amounts and each plan's optional whole-number `cloud_time_multiple`
and `storage_multiple` relative to Pro, when the service supplies them: the
signed-in account's own list, else the public catalog from `GET /v1/plans`;
prices and multiples are never hardcoded, and the list carries no absolute
allowance). `error` is `service_unsupported` when the Pro service does not
support this app version; project continuity then stays off (rechecked every ten
minutes) while local work, SSH and the account itself are unaffected.

## Project mirrors and automatic handoff

The signed-in app gives its local daemon a separate, limited account credential.
That credential stays in memory; the daemon can keep publishing mirrors after
all app windows close. Only signing out removes it (see step 5 above). A lapsed
payment or a keeper still being assigned pauses new setup but leaves the daemon's
setup and local work alone; the account refuses cloud copies without a plan. A
daemon that restarts or loses its setup is set up again with a fresh credential,
immediately after an in-app daemon update and otherwise within 30 seconds. While
the daemon's credential has lapsed (`renewal_failed` on `/pro/status`), Projects
and privacy reads "Reconnecting your account…" with nothing to press; the app
renews it on its own.
Quitting the app keeps the daemon copying, but other devices cannot open this
computer's projects while the app is closed: offering the daemon to them belongs
to the running app. Quitting while an agent is working can instead continue
that work in the cloud (see [Quitting](#quitting)).

Pro → **Projects and privacy** shows privacy, the last recorded copy, and actual
problems such as an incomplete copy or a required provider connection. Healthy
file counts, storage quotas and generic environment diagnostics are not account
controls, and setup commands are not edited there; Chimaera and its agents manage
those details. The two exceptions are decisions only the user makes. A setup
command an agent proposed (`profile.pending_setup_command`) shows once per
project, whole, in monospace, as "Your agent proposed a setup command for the
cloud" with **Confirm** (it becomes the project's `setup_command`) and
**Dismiss** (the proposal is cleared). Steps kept for this computer
(`profile.deferred`) are listed under **Steps that need your computer**, a plain
list with no run button. There
is no per-session placement pin: neither the native shell nor the daemon (the old `PUT /pro/keep-running` route is gone) offers one.
**Keep this project on this computer** stops local publication and disables account-side
mirror access. Existing stored data is not silently deleted. A command that
needs your computer is never run on a cloud machine: the agent is told it was not
run there, and it is kept as a pending step for the project (`profile.deferred` on
its status row), listed for you under **Steps that need your computer**; nothing
runs it later by itself.
If the account side
has not confirmed a privacy change yet, the change still succeeds (copying already
stopped here; the status row carries `privacy_pending`): the switch stays on, the
project says quietly that it now stays on this computer while Chimaera confirms it
with the account, and the page re-sends the change on its own (at most once a
minute while visible). Each project row names who runs it: "This project is
running in the cloud right now" or "…on your computer right now". A project whose
saved setup command is running reads as progress ("Setting up the project in the
cloud…" on the cloud's own page); only a setup that failed (`cloud_setup_failed`)
or one waiting for an agent connection asks for attention.

Repository history and working files are separate Git mirrors. Snapshot commits
use an independent index under the daemon's data directory; they never make WIP
commits on the user's branches. A project folder does not have to be a Git
repository: a plain folder is copied as its working files alone, which the
daemon's log notes once (at info level, not as a problem); if the folder later
becomes a repository, its history travels from the next snapshot on. `.gitignore` and `.chimaeraignore` restrict the
working snapshot. Dependency and cache folders that are rebuilt wherever the project runs (`node_modules`, `target`, `.venv`,
`__pycache__`, `.cache`, at any depth; `policy.rs` `REBUILT_DIRS`) never travel as untracked content either, also where no
`.gitignore` says so, as in a plain folder; a file the project tracks in Git under one of those names still does. A single
file over 100 MB stays on the computer it is on (Settings says "Some files are too large for the cloud copy. They stay on
this computer."), and a project is limited to 100,000 copied paths and to the plan's storage. The copy runs for every
project every two minutes, and for a project right after one of its agents finishes a turn (within about five seconds, at
most every 20 seconds; `engine.rs` `TurnEnds`), so after a sudden loss of this computer the cloud continues from the end of
the last turn rather than from up to two minutes before it. Credential filenames, private keys, agent login stores and
secret configuration fields are excluded. Git configuration carries the
user's name, email and simple Git aliases with known flags. Free-form or shell
aliases, helpers, hooks, includes and signing credentials stay on their host.
Safe remote URLs, refspecs and branch tracking follow the repository. Conversations are complete native archives: text the user
or agent put in a conversation remains part of that archive.

The daemon renews a workspace ownership lease independently of mirror jobs: account requests have their own small budget, and installing a returned checkpoint or stopping agents runs as its own task. Laptop first: account unreachability, signing out, a lapsed plan, **Keep this project on this computer** or a daemon restart never stop or lock a computer's own agents and terminals; they only stop publication. Subscribing never interrupts running work either: a project's first enrollment takes its lease around the agents already running, which keep their processes and get no pick-up message. A verified other owner refuses local input at once, and the computer's agents stop at their next safe pause (at most five minutes later); plain terminals are never stopped. After a restart, previous sessions resume once this computer's ownership is verified, or after one minute when the account cannot be reached; a project another owner took over meanwhile keeps them paused. Re-acquiring its own released or lapsed ownership continues local work without reinstalling files or forking conversations. A cloud takeover after an abrupt loss forks native conversations; a clean handoff resumes their existing identities. Imported sessions remain suspended while the complete handoff is staged.

Agents running in the cloud receive a current-host brief through MCP initialization. On the user's own computer an agent gets no brief at all, unless its project came back from the cloud while this daemon was running; then the brief says work runs on the computer again and replaces the earlier cloud assumptions. Structured conversations with an interrupted turn or background work also receive it in their transfer pickup message. That pickup says in plain words where the conversation now runs (in the cloud, or on the user's computer), whether it is the same conversation or a copy continuing from the last saved point because the other machine stopped responding, that the project files were installed and may differ, and to re-check tools and paths; a recovery adds how to treat work of uncertain state. It is tagged `UserMessage.origin` `moved` (now in the cloud), `home` (back on the user's computer) or `recovered` (either way, continuing from the last saved point after the other machine stopped responding), and the chat view keys its divider on that tag. Finished structured conversations resume idle without starting a model turn merely because they moved or returned. Their fresh MCP context is available when the user next asks them to work. The brief identifies device or cloud execution, the registered project root, OS/architecture needed for builds, headless limitations, and fresh cached provider observations. Guidance about the cloud machine's resource capacity appears only in a cloud brief. Absent or expired observations remain unknown; generating context never probes, logs in, wakes compute or sends a turn. Both MCP initialization and read_cloud_profile use this same projection, and returning to a device replaces stale cloud assumptions. Generated context omits topology, routing IDs, raw diagnostics, hardware allocations and credentials. User-owned profile content remains untrusted project data; missing variable names do not prove a dependency is unavailable. Agents should inspect actual tools and failures, use compatible headless or lower-resource alternatives within existing permissions, preserve completed work, and explain only meaningful progress or the specific user action needed. This prompt is product guidance, not an authorization or confidentiality boundary: agents can inspect their permitted environment and may infer where they run. It does not guarantee compliance or prevent all inference. Ordinary Claude and Codex terminal sessions receive the same MCP context under the same rule; a terminal session whose turn was cut off by the move starts with one short "Continuing here" line, and an idle one resumes without starting a turn.

`read_cloud_profile` and `update_cloud_profile` operate only on the authenticated session's registered project. Updates require the current revision, reject unknown fields and credential-shaped content, and are capped at 32 KiB. They keep ordinary agent permissions, and deferred laptop steps remain guidance. An agent can keep or clear the confirmed `setup_command`, but a new command it saves is only a proposal (`pending_setup_command`, additive on `GET /api/v1/pro/profile`): it never runs until the user confirms it in Projects and privacy, which saves it as `setup_command` through `PUT /api/v1/pro/profile`. That route replaces the whole profile. The page ([`pro/profile.ts`](../../web-ui/src/lib/pro/profile.ts)) re-reads it, applies **Confirm** or **Dismiss** only to the proposal shown, and carries the read's `ETag` in `If-Match` with every field. The daemon compares that revision atomically with replacement, including the account generation; a concurrent update is refused with 412. A 409 during transfer triggers bounded re-reads only while the revision remains unchanged. A 412 or changed revision requires another explicit decision, preserving concurrent guidance and never carrying approval into a replacement account with identical command text. Older unconditional PUT clients remain supported, but the current confirmation UI refuses to save without an ETag. The accepted durable write retains its reservations if the window closes. An update that leaves the command alone keeps an earlier proposal waiting, and the tool's result says `awaiting_confirmation`. Saving a profile never runs a command or wakes a machine. Implementation: [`mcp/cloud_context.rs`](../../crates/chimaera-server/src/mcp/cloud_context.rs).

The app's system sleep hook tells the daemon how long it has before the computer sleeps (`POST /api/v1/pro/sleep {deadline_ms}`): 23 seconds of macOS's 25-second wait, logind's configured delay minus a margin on Linux, and under a second on Windows. The daemon's flush fits the deadline it is given. It preempts the periodic mirror pass, stops agents and publishes every project in parallel (projects with running agents first), and never leaves a half transfer: a flush that outlives the deadline finishes or recovers on its own. The publication fence is not waited out past the deadline; an unreleased lease lapses and the cloud continues from the acknowledged checkpoint. Waking before a flush finishes keeps the project on the computer. A failed flush leaves the lease takeover path available.

Moving live cloud work home waits until the laptop has been awake on AC power for five minutes (in both protocol versions) and happens at an agent pause; busy or unobservable agent states stay on the cloud. Work the cloud is no longer running returns at once: a project the cloud released, or one whose cloud lease lapsed (the computer takes it from the last acknowledged checkpoint). A return that did not finish is retried with backoff (two minutes, doubling to thirty) rather than every pass. A return attempt follows ownership changes caused by waking an idle worker immediately, and imports its saved conversation before local work can resume. A lost response is checked against current ownership without repeating the same release request; a refusal is final for that pass. Plain shells become paused placeholders in the cloud; arbitrary foreground programs are not automatically relaunched.

Hand-back restores new branches and fast-forwards unchanged local branches. The
current checkout uses Git's index and ref locks; branches checked out in another
worktree are preserved separately. Diverged branch tips remain as
`<branch>@cloud-<commit>`, and Projects and privacy lists them for merging; only branches are preserved this way (divergent remote-tracking refs and tags from the cloud are not copied). An untracked local file identical to one the cloud committed no longer blocks the fast-forward; a differing one keeps the cloud branch separate. Existing
remote configuration and `FETCH_HEAD` stay intact. Files merge three ways against the last snapshot this computer actually published (a snapshot whose upload failed never counts): a file only one side changed takes that side's version, so edits made here while the cloud worked are kept. If a file changed on both machines, the cloud's version takes the path and your version is saved right beside it as `<name>.mine-<yyyymmdd-hhmm>` (a file that stays on this computer and is never copied to the cloud); a local edit to a file the cloud deleted is saved the same way. The project's row reports how many files kept both versions and names the saved copies (Projects and privacy: "Kept both versions of 3 files. Your version is saved beside each file"), and that report survives a daemon restart until the next return replaces it or every file is settled in the review (see [When both sides changed a file](#when-both-sides-changed-a-file)). The same return raises one notification, in the app and from the OS: "Kept both versions of 3 files", the project's name, and the first saved copies by name (a `kept_both` notice, see [notifications.md](notifications.md)); a return that kept nothing says nothing. A file missing from the cloud's snapshot is removed only when that snapshot's inventory shows it was deleted, never because the mirror left it out (a large file, a symlink, a credential-looking file, `.chimaeraignore`). A saved setup command runs in
the background on the cloud machine (the user's login shell, in the project folder, at most ten minutes) before any conversation continues there; no terminal opens for it. If it fails, the project shows one line (`cloud_setup_failed`) and its output is kept in the project's setup log. Deferred steps remain guidance for the returning agent, which assesses and runs them under its normal permissions; they are never automatically replayed by the daemon.

A project on Home whose work the cloud holds carries a quiet line in its muted meta
text — "In the cloud · 9m ago", "Coming home… · 9m ago" while a return installs, "Moving
to the cloud…" while this computer hands it over — read from this daemon's own
`GET /pro/status` ownership (`workspace/placementHints.ts`); it appears only when Pro is
configured and the project is owned elsewhere, and never in the accent colour.

### Manually parked conversations

Active-idle application of custom cloud secrets remains disabled pending its
supervisor process-census and real-runtime acceptance gates. Its daemon and UI
support a separate manual parking reason, `project_secrets_idle`, which preserves
the original conversation, session ID, journal and delivery receipts. Opening
that conversation shows a neutral paused explanation; it does not claim that
secret values were applied. **Resume** explicitly restores that same session
through `POST /api/v1/sessions/{id}/resume` with no body. Input stays blocked until
the new process confirms the exact native conversation and the journal and
ledger are durable. A lost response can be retried without another spawn.
Unknown parking reasons require updated support and remain blocked. Bulk
workspace recovery, ordinary Recents/native resume and transfer pickup do not
automatically restart or replace a manually parked conversation. Source:
[`api/manual_resume.rs`](../../crates/chimaera-server/src/api/manual_resume.rs)
and [`ledger_manual.rs`](../../crates/chimaera-server/src/ledger_manual.rs).

### When both sides changed a file

When the work comes home and the cloud and this Mac both changed the same file while apart, both versions are kept: the file holds the cloud's version and this Mac's version sits right beside it as `<name>.mine-<yyyymmdd-hhmm>`. Nobody has to go looking for those names. The chat marks where the work came back with one quiet line, "Back on this Mac. The cloud and this Mac both changed 3 files while apart.", and a **Review** button; the same review opens from the "Kept both versions" line in Settings → Chimaera Pro (its **Review** link), from the "Kept both versions" notification, and from a kept copy's right-click menu in the file tree, where every such copy carries a small "from this Mac" badge that explains the name on hover. (Off a Mac the words say "this computer", and a browser view of the project says "your computer".)

The review is a workbench tab, "Both versions": the files on the left, and for the selected one this Mac's version and the cloud's side by side with the differences highlighted. Each file offers **Use this Mac's** (it replaces the file and the copy beside it goes away), **Use the cloud's** (the copy beside it moves to the Trash, so it can still be taken back), and **Keep both** (nothing moves; the file just stops asking); the header offers **Use the cloud's for all** and **Use this Mac's for all**, each confirmed first ("This Mac's versions of 3 files will be moved to the Trash. The cloud's versions stay."). The copy goes to the Trash on its own drive: the Mac's Trash (`~/.Trash`) for a folder on the Mac itself, the drive's own Trash for a folder on an external drive that has one, and on Linux the desktop's trash (`~/.local/share/Trash`, restorable from the file manager). A folder on a drive with no Trash (a network share, say) says so before you choose, and there the copy is deleted instead; if a Trash refuses a copy the review promised it would take, the review says it was deleted. Moving to the Trash never replaces anything already there: a second copy of the same name becomes "<name> 2". A picture or other non-text file, or one larger than 512 KB, shows both sizes and the same choices. A file the cloud deleted says so: this Mac's version brings it back, the cloud's leaves it deleted. Branches the cloud kept beside yours (`<branch>@cloud-<commit>`) are listed under "Branches from the cloud" for merging, with no actions. A return names at most 32 kept copies; when it kept more, the review says that some aren't listed and that they keep their `.mine-` names in the folder. Choices apply only while the project is on this Mac. Once nothing is left to choose, the chat line and the Settings line go away. Implementation: [`pro/kept.rs`](../../crates/chimaera-server/src/pro/kept.rs) (routes in [`pro/AGENTS.md`](../../crates/chimaera-server/src/pro/AGENTS.md), "Reviewing both versions") and [`web-ui/src/lib/pro/KeptReviewView.svelte`](../../web-ui/src/lib/pro/KeptReviewView.svelte).

### Quitting

Quitting the app (⌘Q, the menu, the tray, Dock › Quit, logging out), or closing its last window, while an agent is working in a project on this Mac asks first, but only when the cloud could take that work over right now: you are signed in with a plan whose cloud is set up and has time left, the project is copied to the cloud and runs on this Mac, and every agent working in it is connected in the cloud (Claude or Codex signed in there, as the Pro page last saw it). A project whose agent isn't connected in the cloud is left out of the question, since its work would only wait there; when no project is left, quitting is exactly as before. The question names the agent and the project ("Claude is still working in atlas", or "Agents are still working in 2 projects") and asks "Keep working on this Mac, or continue in the cloud?". **Keep working here** is the default and quits as before: the work keeps running on this Mac with the app closed. **Cancel** keeps the app open. **Continue in the cloud** hands just those projects to the cloud the same way sleep does, while a small window says "Sending your work to the cloud…" and lists them; the app quits when the cloud has them, or after about 25 seconds while the last steps finish on their own. A copy that reached the cloud but whose handover the account couldn't confirm in time still counts as handed over: the project stays parked, and the cloud continues from that copy within a couple of minutes, exactly as after a lost connection. Only a project whose copy couldn't be made at all stays here: the same window says "The cloud couldn't take over. Your work continues on this Mac." (naming the project when only some of them stayed), and **Quit** closes the app with that work still running here. Work handed over this way stays in the cloud until you open the app again, however long this Mac stays awake; from then on it comes home by the usual rules above. Unsaved edits are asked about first, in their own dialog. Closing a window that isn't the last one never asks, and neither does an app update's restart. On Linux and Windows the question says "this computer".

Implementation: [`shell/quit.rs`](../../crates/chimaera-app/src/shell/quit.rs) asks and posts `POST /api/v1/pro/sleep {deadline_ms, park: true, workspace_ids}`; each `/pro/status` project row carries additive `working_agents`, `cloud_handoff` and `parked`; the app posts `/pro/wake` at launch while anything is parked ([`pro/AGENTS.md`](../../crates/chimaera-server/src/pro/AGENTS.md), "Quit handover").
### After a reinstall or reset

A project folder records its workspace identity inside its Git directory (or a
small `.chimaera-workspace` marker for a non-Git folder). Opening an already
synced project on another computer refreshes a local copy; execution remains
with its current owner. **Take over** separately moves execution through the
existing safe handoff. A duplicate folder on the same computer becomes a
separate project; a plain Git clone is separate until selected from synced
projects. Read-only folders carry no persistent marker. Identity lives in
[`workspaces/identity.rs`](../../crates/chimaera-server/src/workspaces/identity.rs).

Synced projects without a local copy appear on Home. **Open project** asks where
to save the copy: an empty folder is used directly; a nonempty parent receives a
new folder named after the project. Cancellation leaves saved work untouched.
Later opens refresh the bound destination immediately from the latest published
checkpoint, preserving local conflicts. Open never acquires execution, resumes
agents or runs setup. A missing/moved folder or unrelated nonempty destination
fails safely. An older daemon without the exact copy acknowledgment requires an
update; there is no fallback to its old transfer-on-open route.

A copied workspace's placement strip offers **Take over** only after a ready
copy and current ownership epoch are verified. Sessions continue on the current
owner until that explicit action completes. Copied-local edits are retained but
are not automatically published. **Project copies** in Settings also reports Git
staging: synchronized, an older snapshot without staging, or conflicts. Conflicts
keep the local staged version for conflicting paths and preserve both index
snapshots for recovery, including staged-only content. Its advanced recovery
reference is relative to the actual Git directory, so linked worktrees work too.
Source and shadow histories remain within account quotas; initial transfers have
a bounded 16-minute deadline.
Implementation: [`pro/`](../../crates/chimaera-server/src/pro/AGENTS.md),
[`MirrorSettings.svelte`](../../web-ui/src/lib/settings/MirrorSettings.svelte),
[`power.rs`](../../crates/chimaera-app/src/shell/power.rs), and the
[session bundle contract](../../crates/chimaera-server/BUNDLE.md).

## Continuing and viewing a remote session

A transferred session keeps its public ID, pinned title, linked terminals and
workspace tabs. The local workbench merges remote session rows under those IDs
and forwards their chat and terminal connections through the signed-in app. A
routed session is labelled "In the cloud" or "On another computer" ("·
reconnecting" while its owner cannot be reached and this view's own connection
to it is down); opening it never silently starts another local agent.

**Explicit opens and input may wake; background views never do.** An explicit
project open may wake cloud without moving execution. Passive chat or terminal
attaches, roster polls, refreshes and file watches never wake a paused project.
Real input may wake it too: a keystroke, a chat message, a stop or a permission answer. On a
computer, the daemon keeps the chat/terminal connection open while the owner is
paused or reconnecting, holds that first input (a little typing, up to four
chat commands, within one daemon-wide budget) and delivers it once after the
owner answers. Only what you do counts as input: the thinking preference a
chat sends by itself and usage and MCP reads never wake a machine or move
work. A setting you change while the owner is away (the model, the
permission mode, the effort) does not wake it either. It is kept and applied
in front of whatever you send next from that window, so a stricter mode
picked before a message is in force when that message runs; until then the
old value keeps showing. It is never applied by itself: if you send nothing
within ten minutes it is dropped with a short note, so a window left open
cannot change the mode of work someone starts later from another device.
The message shows at once as a "sending…" bubble and
the chat says "Waking the cloud machine…"; a terminal says the same over the
pane and echoes nothing until the owner answers. While it wakes, a second
message or more typing is not held: it is refused with a short note (the
message comes back into the composer). A browser
view reconnects once with wake intent when the user types or sends into a
dropped connection; the action itself is not queued (the composer keeps its
text; a terminal says it is waking). There is no reconnect or wake button: the
chat says "Reconnecting…" (only for a viewed project, after a short grace) or
"Asleep in the cloud. Send a message to wake it." while the cloud machine is
asleep, and its header reads "In the cloud · asleep"; a terminal says "Asleep in
the cloud. Press a key to wake it." Asleep holds across dropped connections
until something wakes it, and nothing retries against a sleeping cloud
machine: a chat or terminal whose connection drops while it sleeps waits with
no timer until you send or type (which wakes it) or the project answers again
(its row becomes reachable, or a browser view's placement reads it awake). A
browser view of a sleeping project still opens: the account reports it as
`suspended`, which is routed like any owner and never woken by reading.

**When the service keeps the cloud's connections.** The account service may
keep a sleeping cloud machine's chat, terminal and event connections itself;
it marks each connection it keeps (`X-Chimaera-Sockets: kept`), and the app
works with a service of either kind. The keeper supports these retained
connections; the paragraph above describes fallback behavior with an older
service. To learn which kind it talks to, the daemon makes one connection
attempt to a sleeping cloud machine that carries no wake request (a service
that keeps none refuses it), at most once every five minutes per machine.
Against a service that keeps them, a connection opens as usual while the
machine sleeps and stays open when it sleeps again. What you send or type
goes out at once (on a computer the daemon passes it straight through and
holds nothing), the service wakes the machine and delivers it once, in order,
and more messages or typing during the wake join the same queue; a model,
mode or effort you pick meanwhile is kept too and applied in the order you
made it. Each message
shows as its own "sending…" bubble until the agent has it; while the machine
wakes, a terminal says "Waking the cloud machine…" over the pane (as does a
chat that had not connected before) and echoes nothing until the machine
answers. A connection that is open and quiet is not "Reconnecting…". A
conversation opened for the first time while its machine sleeps says "The
conversation shows once the cloud wakes. Sending a message wakes it." When
the machine wakes, the same connections carry on: a chat receives exactly
what it missed, a terminal repaints, the window's file and Git updates
resume. If the wake does not arrive, every message it was holding comes back
into the composer, in order.

**A message runs once, and is not lost quietly.** Every message you send
carries its own id, and the machine that runs the agent accepts an id once,
however many copies of the message reach it. The app uses that in three ways.
A message that is refused returns to the composer with its pictures, exactly
the one that was refused (several come back in the order they were sent), and
lands above whatever you are writing there. Only a *message* comes back: a
refused stop or permission answer says so without resurrecting a message
that may already have been delivered. A message the agent has not confirmed
shows as "sending…": at once when the conversation is not live, after a few
seconds otherwise. And whenever the conversation answers again (a reconnect,
a machine waking, work arriving on this computer), the app sends each
unconfirmed message again under its id: if the first copy had arrived, or
arrives a moment later from whoever was holding it, the second is dropped,
and if it had not, this one is delivered. A message that has waited more than
two minutes is not sent again; the app withdraws it instead, and it returns
to the composer with the note "not delivered" unless it had arrived after
all, in which case it shows in the transcript. A message that comes back
with pictures comes back whole: if the composer has no room for them yet, it
waits until it does. Withdrawal and dispatch evidence are stored independently
of the conversation journal and survive agent replacement, daemon restart and
project transfer within the bounded retention contract. A crash after dispatch
but before its echo leaves delivery uncertain: automatic resend is refused and
the composer asks the user to check the conversation. This prevents blind
replay; it does not promise exactly-once external effects by the agent.
A machine that runs an older version of Chimaera has no ids, so nothing is sent
to it twice and a message whose connection ended before it confirmed stays
"sending…" there; a message sent to a machine in the instant it fell
asleep waits, shown as "sending…", until something wakes that machine.
Typing that is refused is said in a small note over the terminal; typing that
was still on its way when a connection ended is not replayed.

**Opening and ordinary input leave execution with its owner.** Opening a synced project creates or
updates its local copy. Chat messages and terminal input still route to the
current owner; they do not implicitly acquire execution. **Take over** asks that
owner to finish its current step, save and release, then establishes execution
here. A failed or stale action leaves the current owner authoritative and asks
for a fresh explicit decision. Explicit opens may wake the cloud; background
views, polls and refreshes never wake or move work.
From a phone the same holds, with one addition:
when the cloud is asleep with the project and one of your computers is
online with the app open (preferably the one the project last ran on, and
one on power), a message or typing from the phone brings the work to that
computer instead of waking the cloud, and the phone says "Bringing the work
to your computer…". Only if no computer takes it within about twenty seconds
is the cloud woken as before. For this, the cloud saves each project it holds
as it goes to sleep. Implementation: [`pro/moves.rs`](../../crates/chimaera-server/src/pro/moves.rs)
and the viewer relay in [`session_proxy.rs`](../../crates/chimaera-server/src/session_proxy.rs)
(the contract is in [HANDOFF](../../crates/chimaera-link/HANDOFF.md#explicit-take-over-moves-execution)).

**Moving between devices is not an exit.** When a conversation moves between
this computer and the cloud, every open view is told it *moved*: the chat stays
mounted with its transcript and scroll position, says "Continuing in the cloud…"
or "Continuing on your computer…" (composer disabled for that reason), and
reconnects to wherever it runs next — at once when its row comes back, not after
a retry delay. Only a real transfer says so. Signed out, a conversation the cloud
still holds reads "This conversation is in the cloud. Sign in to Chimaera Pro to
bring it back." instead. Where the agent was told about the move, the transcript
shows one line — "Continued in the cloud · 5m ago", "Back on your computer", or
"Continued in the cloud after this computer stopped responding" — with the
message itself behind "Show what the agent was told". A session that is only paused says
why instead: "Picking up where you left off…" (a daemon restart on the machine
that owns the project, whatever its cause), "Waiting for Claude Code in the cloud" (its
agent is not signed in there yet; when the row names the agent with
`blocked_provider`, the view offers **Connect Claude Code to continue**, which
opens that sign-in in Chimaera Pro), or "Opening…" (its transfer is starting it).
A conversation waiting on a permission or question is marked wherever an agent
waiting for approval is (the approval count, the attention lane, its dot),
whether its row says so through `agent_state` or the chat row's additive
`needs_permission`. A paused plain terminal reads "This terminal stays on
your computer". A new tunnel or credential for the same owner is not a move:
views just reconnect.

**A window's own daemon stays in charge of the window.** For a project running
elsewhere, the window's event stream merges only that project's sessions, file
changes, Git and Timeline updates from the owner; the owner's settings, recents,
notices and update status never replace this computer's. While a window has the
project open, a conversation there that finishes its turn, waits on a
permission or a question, or sends you a message notifies on this computer too, once, through this
computer's own notification settings, and counts as an approval here until it is
answered on either machine. An owner change never
closes the window's connection. The project's own paths go to its owner. A
file outside the project is saved on this computer when its folder exists here.
Reading one asks the owner first: a conversation's pasted images come from the
owner; a path where the owner has nothing is this computer's own file (your home
folder, a download) and opens from here; and a path where the owner has a
different file it may not show (a report an agent wrote to `/tmp` on the cloud
machine, say) reads "This file is in the cloud" rather than showing this
computer's file of the same name. The owner never shows anything outside the
project but the project's own pasted images, and says only whether it has
something at such a path, never what.

The terminal toolbar (shown only for a viewed project) supports **Just
watching**. A phone-width browser view starts there with the sidebar collapsed
(that collapse is never saved into the shared layout): the existing server grid
stays at its original size, readable with horizontal scrolling, and the viewer
cannot type or resize it. **Take control** reconnects and fits the terminal to
the current pane; taking control is not itself interaction, typing is. Native
windows of any width keep full control, and free users never see this toolbar.

## Constraints and edge cases

- Default endpoint is unset. The Pro transport starts no sockets until explicitly
  configured and signed in; the existing SSH route remains the signed-out fallback.
- Account credentials live in the OS keychain. Refresh-token rotations replace the
  stored pair. A temporary credential-store failure keeps a verified session
  signed in while the app retries saving. After bounded retries, an account notice
  explains how to check the credential store and retry; an unsaved session may
  require sign-in again after restarting. The account answers a revoked, expired
  or replayed refresh token with `400 invalid_grant`; the app treats that, and
  any other refresh refusal except a missing route (404, as during a deploy), a
  timeout or a rate limit, as the end of the sign-in and never presents that
  token again. Those, server errors and network failures keep the session. Only
  a connection that never reached the account retries at once with the same
  token; after a timeout or a lost reply the account may already have rotated
  it, so the app waits for the next refused request instead. Daemon bearer tokens
  remain in memory and do not enter `hosts.json`.
- SSH never hangs on Pro: a saved SSH host connects directly while Pro starts
  (a kept host waits at most 10 seconds first, so the usual Pro route needs no
  new login), and a kept host whose Pro route is unreachable falls back to a
  direct connection (its row then reads as direct). Once the always-on
  connection has started signing in to the host (it may be showing a password
  or Duo prompt), a brief account or connection hiccup is waited out instead,
  so the user is never asked to log in twice. Computers reached only
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
On a cloud machine, managed agents need a fresh bounded execution lease;
expiration fences input and stops registered agent process groups. A computer is
never fenced by lease expiry (laptop first): it stops publishing until the lease
is renewed, and only a verified other owner stops its agents. Publication and
forwarded viewers need a live lease everywhere. A clean handoff publishes an
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
registered managed work. Unsynchronized local file conflicts remain in excluded
`.mine-…` siblings; Git staging conflicts retain both index snapshots and their
objects in the local recovery area, within explicit storage bounds. Launch evidence
records the process groups of live managed agents: a graceful daemon stop (an
update or restart) clears it once they exit, and a successor after a crash waits
only for recorded groups that still exist. Process groups alone cannot contain
detached descendants or guarantee OS-resume ordering. Local non-Pro execution is
unchanged. Explicit takeover between computers is separate from this automatic
recovery policy; selected-project custom secrets have their own acceptance gate.

A cloud machine suspends only after draining: the supervisor asks the daemon to
refuse new transfers and wait until every transfer, Git helper and project cache
is idle and state is on disk (`POST /api/v1/pro/drain`; see
[HANDOFF](../../crates/chimaera-link/HANDOFF.md)). Waking it retains the saved
processes and files. The viewer waits for fresh execution authority before its
input is admitted; nothing is reinstalled over the owner's work or told again
that it moved. Ownership state, the
execution latch and handoff ledger writes are synced to stable storage, and an
unreadable ownership state fails closed instead of silently forgetting fences, for
the affected projects only: they keep running on your computer and resume saving
cloud copies once the account confirms them again.

## Project views follow the current owner

Opening the same project on a phone, browser or another computer does not move
execution away from its current computer; a native local copy requires explicit
**Take over** to move execution. Logical browser routes and
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

## Native SSH authentication under development

**Status: partial; `ssh-agent-prototype` is off by default.** Explicit Connect
captures the Mac's effective SSH destination, authentication methods, public host
trust and selected agent keys for each supported route leg. Common ProxyJump
routes support at most three jump hosts. The keeper receives scoped signing
authority and public policy; private keys remain on the Mac. Arbitrary local
ProxyCommand programs require the explicit Advanced Direct option.

First-use trust is owned by the original Connect attempt. A native OpenSSH probe
asks for approval and verifies the candidate before an append to the selected
native known-hosts file. Key mode requires a signed key-exchange receipt;
interactive mode requires a positively authenticated private connection. Cancel,
account replacement, changed configuration or failed proof refuses a grant.
Cancellation before the append leaves trust unchanged; a failure after an approved
append can retain that verified entry while still refusing the connection.

The native implementation lives in
[`ssh_agent/trust`](../../crates/chimaera-app/src/ssh_agent/trust/mod.rs), with
attempt ownership in
[`ssh_agent/lifecycle.rs`](../../crates/chimaera-app/src/ssh_agent/lifecycle.rs)
and scoped prompts in
[`askpass.rs`](../../crates/chimaera-app/src/askpass.rs).
First-use UI, configured but unloaded keys and complete keeper-held route/Slurm
journeys still require integration acceptance. The prototype does not change
ordinary direct SSH or advertise completed keeper support.

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

### Acting brings the work to you
_Captured 2026-09-30 from the maintainer's decision, relayed with the task that implemented it._

- **The rule:** "acting brings the work to you; looking doesn't". Sending a
  message or typing on another of your computers moves the work there;
  opening, reading and watching never do.
- **Phone:** "if you are on your phone, and the laptop is connected, it should
  still run on the computer and not the cloud (to minimize cloud usage)".

### Sync a local copy; take over separately
_Captured 2026-10-02 from the maintainer's direct answers in this conversation._

- **Open on another Mac:** “Yes—sync a local copy on open; take over separately”.
  This supersedes automatic takeover on ordinary input between computers in the
  earlier “Acting brings the work to you” decision. Opening copies files; the
  separate takeover action moves execution.
- **Wake:** “Explicit opens may wake the cloud; polls and refreshes never do
  (latest recorded decision)”. The phone-to-available-computer policy remains.
- **Custom secrets:** “Include selected-project custom secrets in this completion”.
  For changes while work is active: “Queue until idle; offer Apply now”.
  Named provider connections and custom project permissions remain distinct.

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

### Keeper SSH and Slurm continuity
_Captured 2026-10-03 from the maintainer's direct answers in this conversation._

- **Continuity:** “Yes, keeper should keep interactive jobs connected too!”
  The maintainer also explicitly requires the app-created compute-node forward
  to remain held for the length of the Slurm job.
- **HPC policy:** “infinite ssh we can have to a remote but NOT a HPC login
  node, unless the user enables this”, using the existing opt-in behavior.
- **Experience and compatibility:** “you dont want to have to set up the keeper
  to have ssh keys etc.” Asked about jump hosts, the maintainer answered:
  “I have no idea but I feel like we should support most things”. The requested
  experience is broad compatibility with the Mac's existing SSH setup; the exact
  implementation remains open to improvement.
- **Explicit fallback:** The maintainer requested a direct connection choice in
  Advanced settings for workspaces or sites where keeper access is not allowed.
  This is a deliberate user choice, not a silent fallback.

_Broader Pro intent capture remains pending. The behavior above the divider is
derived from code; the statements here record the maintainer's instructions._
