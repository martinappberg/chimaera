# web-ui/src/lib/settings — the settings surface

Orientation for coding agents. The client half of daemon settings: a
schema-driven form + a raw-JSON editor over `/api/v1/settings`. Parent map:
repo-root [AGENTS.md](../../../../AGENTS.md). The chat surface next door is
[`../chat`](../chat/AGENTS.md).

## The rule that governs this directory

**`schema.ts` is the single source of truth.** Every setting — its key, type,
default, label, and grouping — is declared there once; the form (`SettingRow`,
`AgentsSettings`) and the raw editor (`SettingsJson`) both derive from it. Add a
setting by adding it to the schema, not by hand-wiring a control.

`chat.codexSteering` is launch-scoped: its Agent default preserves the native
Codex configuration; the Next step / Immediate choices override only newly
started or resumed structured chat processes. It does not change queued sends.

`chat.newSessionModel` defaults to the existing remembered model/effort behavior.
Agent default omits those per-agent preferences at launch; explicit choices and
the resumed conversation's own settings still win. Permission-mode memory is independent.

**Exception — Environment.** The Environment category is store-backed, not
schema-backed: `EnvironmentSettings.svelte` edits the daemon's prelude map over
`/api/v1/environment` (persisted as `env-profiles.json`, not `settings.json`).
No schema rows, an explicit Save (fetch-merge-put — the PUT replaces the whole
map, so other workspaces' entries must round-trip), and an empty editor deletes
its scope's entry rather than persisting `{text: ""}`.

**Exception — Documents.** Also store-backed: `DocumentsSettings.svelte` drives
the opt-in "teach agents the document dialect" installs over
`/api/v1/agent-docs` (an `AGENTS.md` block, a Claude Code skill). The daemon
owns the text; the confirm dialog shows it verbatim before anything is written.

**Pro is a separate account surface.** the private optional account view is a singleton
workbench tab and Home view. When Pro is offered (`net/plan.ts` `proTier` is
not `free`: the extension is composed and the window is this computer's own or
the account gateway; decided without asking anything) Settings ends with a small
Chimaera Pro group that opens it (Pro is an optional add-on, so it follows every
working section, and its entry is neutral for every plan, never accent-tinted);
Home retains its Pro navigation. Until a plan is active (`proTier` `active`)
the entry is the short optional-benefit Get Pro card, never "checking your
plan"; paid users see Your Chimaera Pro/Max and View account. The Keyboard section carries its reference chords inside it, so a
group after it never splits them off. `../net/plan.ts` shares its existing subscription between this entry
and the badges: loading or failed/unknown entitlement stays neutral, never a
sales prompt or an active-plan claim. There is no extra poll or cloud wake. A paid workspace plan badge also
opens it, without adding a full-width sidebar row for any plan.
It now renders the finite optional `../extensions/AccountApplicationView.svelte`; the private package presents the named native account commands. The old public ProSettings/CloudSetup/MirrorSettings files are temporarily unused source candidates until actual adapter acceptance and removal.
The generic Settings form no longer embeds account/billing/onboarding controls.
An account browser on a cloud worker opens the same provider flow; other hosts
link to the account's billing page (`/account/billing`). Ordinary browser daemons
have no Pro entry. An account browser
shows a Cloud category only on the cloud machine's own page (a passive
`isCloudMachine` read); it remains host-pinned and cookie-authenticated.
On the web's Home (the account's own page, `net/base.ts` `isAccountHome`; see
[optional account host](../extensions/AGENTS.md)) there is no daemon: `SettingsView`
with `account` shows only Chimaera Pro, rendered in place by
the finite account-settings extension slot (plan, usage, the billing link and Sign out), with no
search, JSON tab or daemon settings.
The private optional project-secrets view is a separate capability-gated account control on
paid native/browser Home. It edits one name without reading existing values,
carries the shown queued batch, and separates Queue until idle from confirmed
Apply now or immediate Remove access. Project contexts navigate to account Home;
they never send a value through a daemon or remote project transport.
Credentials stay in the app's keychain, never in the schema or this UI.
Account devices group only verified installation bindings; unbound older sessions
are collapsed under Other sign-ins, retaining individual removal controls.

Account sign-in, entitlement, and cloud readiness are separate. No-plan accounts
see plan selection, not paid operation controls. Existing privacy controls remain
available in a recovery disclosure. Checkout runs in the browser through the
native authenticated bridge; confirmed account state alone activates the plan.
The signed-out page leads with Sign up and a smaller existing-account Sign in
link. Each opens the matching authentication screen, retains the chosen plan,
and returns to plan selection after authentication. Checkout requires a separate explicit action
from a confirmed free account. An active account restores its subscriber view.
Only confirmed free accounts see the illustrated introduction and plan selection.
Paid accounts see an operational overview without a sales pitch or walkthrough.
Unknown/startup/error account states stay neutral; a connection warning is not an
error. An overdue payment (`payment_due`) shows Payment needs attention with
Manage billing and never plans or checkout. An ended plan inside its return
window (`returning_until`) reads as no plan: the badge says **Plan ended**, plans
are offered, and one quiet line says when its cloud work can still be brought
home. A planned restart of the always-on cloud connection (`keeper_restart_at`)
is one quiet line under Connected machines, until the account clears it. The page renders from the last
confirmed status (private `packages/pro-client-ui/src/account/account.ts` `accountPanel`) while background reads run, so
nothing unmounts on `pro-changed` or focus; an account needing attention keeps
that panel too, and only Check again shows checking (the error bar hides
meanwhile). `service_unsupported` shows its explanation with no manual check,
since the app rechecks it itself. Checkout and the Max review re-read
the account first when an event is pending. Prices come only from `ProStatus.plans`, all four or none.
Neither a remembered selection nor a successful account refresh can trigger checkout. Usage shows percentages
computed from the account's current allowances, not fixed hour or storage totals. The static introduction shows
project/conversation continuity; it is not live setup progress or arbitrary
process migration.
Native billing attempts own their finite confirmation lifetime even if this view
is hidden or closed. The UI has no checkout polling loop; it displays native
opening/waiting/confirming/result states and refreshes cached status on events or return.
Subscriber billing feedback stays inside the account card, preserving cloud and
provider panels. Account events immediately invalidate older reads; billing
snapshots cannot roll back to an older attempt or phase. Pro subscribers near
a limit (private `account/account.ts` `nearLimit`) can choose Upgrade to Max, review the interval locally, then explicitly open the
hosted price/proration confirmation. Only server-confirmed Max changes the plan. An unconfirmed review return settles
after a bounded native check with the actual current plan; it never implies that
the user canceled or that a delayed billing update cannot arrive.
Only an authoritative active account plan unlocks paid content. Expired/failed
checkout stays neutral until an explicit Check account succeeds with no plan for
the same attempt; Return to plans then acknowledges and clears that attempt. The
review is keyed by attempt, phase and plan, so unrelated account events keep it. A targeted native
return reopens Pro, including when a new window needed time to mount.
**Exception — Plugins.** A second, declared source beside `schema.ts`:
`PluginsSettings.svelte` lists each installed plugin's `[[settings]]` (the
0.2 platform, docs/design/plugin-platform-plan.md §9) through
`plugins/PluginSettings.svelte`, the rows its card shows too. The daemon owns
the values (`GET`/`PUT /api/v1/plugins/{pid}/settings`, kept per plugin
under `<data dir>/plugins/.data/`, never `settings.json`) and checks each
against its declaration; search matches plugin names and setting labels.

**Exception — Activity.** Also store-backed and read-only: `ActivitySettings.svelte`
shows sessions, tokens and time from the session records across every workspace
(`GET /api/v1/activity`, CSV via `/api/v1/activity/csv`, both through
`../workspace/history.ts`). No dollar figures (cost is only a CSV column); unknown is
"—" with a tooltip, never zero. `jump.ts` lets another surface open Settings at a
section (`requestSettingsSection`) — or at one setting's row, given its id.

**Agents holds schema rows too.** The Agents category renders the bespoke
`AgentsSettings` panel (its `agents.<id>.path` rows are presented there, not
generically), then **Agent communication** — `agents.communication.enabled` and
`agents.communication.wakes` — as ordinary `SettingRow`s under their own
subheading (`SettingsView`). The schema has no "show only when" yet, so the
wakes row always shows.

## File map

| File | What it owns |
|---|---|
| `schema.ts` | The settings schema: keys, types, defaults, labels, groups. Ground truth. |
| `store.svelte.ts` | The reactive settings store: load/patch/persist against `/api/v1/settings`, sparse-map semantics, the `dirtySince` echo-guard, and document-wide theme/interface/editor CSS variables. |
| `themes.ts` | The curated light/dark theme definitions + `applyAppearance`. |
| `AgentsSettings.svelte` | Installed-agent cards; Advanced paths and removal. Opens the shared `../workspace/AgentSetupDialog.svelte` through `agentSetup.ts` for install/update/reinstall and recoverable progress/results; the compact dialog keeps technical output under Details and separates Sign in from Open chat. Polls only while visible and preserves unfinished edits. |
| `agentStatus.ts` | Honest update and installer status rules; unknown/failure never means up to date or installed. |
| `EnvironmentSettings.svelte` | The Environment prelude panel (bespoke, `/api/v1/environment`-backed — see the exception above). |
| `environment.ts` | Wire types + `getEnvironment`/`putEnvironment` for the prelude map. |
| Private optional `CloudSetup` | Automatic cloud status, bounded visible polling (no manual check). Only an account's very first setup reads as setup ("Getting things ready", "Setting up your cloud. This usually takes a couple of minutes."); once the cloud has been ready (`presentation.ts` `cloudReadyOnce` from the app's `cloud_ready_once`, or seen ready this session) a later `preparing` is the same "Available when you need it" as ready and idle. The check mark needs an agent on record (live from the connection panel, else the app's remembered `agents_connected`, or in a browser view its remembered rows); none connected names the step, unknown claims nothing. A ready-but-unreachable read is re-checked with the account and reported only on a second read in a row, and a failed account status read likewise (`presentation.ts` `afterFailedRead`: one miss keeps the last confirmed state and its calm copy and re-checks on the fast cadence; the second in a row shows the existing error); an asleep-or-starting answer (`cloud_asleep`) is idle, never unreachable and never words. **Agent connections** (`ProviderConnections`, compact, mounted once and kept) shows whenever the account has a cloud, fed the remembered rows (`remembered_providers`, or `pro/catalogMemory.ts` in a browser view) and `live` only while the cloud answers. Showing or opening it never wakes the cloud (it only looks, passively); there is no `start` request here. Historical project-copy summaries. Opening a repository exists only on the cloud machine's own page, as navigation; the app has none. See [provider map](../pro/AGENTS.md). |
| Private optional `MirrorSettings` | Native project-copy status and privacy; visibility-gated 15-second status refresh; the row is re-read after every change; a privacy change the account hasn't confirmed yet is a success that reads as quiet progress and is re-sent quietly (≤1/min, the first a minute after the click); each row names who runs the project (`presentation.ts` `projectPlace`), and setup reads as progress unless it failed or waits on an agent; the switch is **Keep this project on this computer**; copy work still under way and too-large files read as a muted hint, only real problems in the warning colour; `renewal_failed` reads "Reconnecting your account…"; kept-both rows name the user's versions saved beside each file; blocked-provider rows open the shared connection flow. An agent's proposed setup command (`pending_setup_command`) shows once per project, whole, with Confirm/Dismiss (private `packages/pro-client-ui/src/account/profile.ts`, over the fixed public profile read/save bridge); `profile.deferred` is a plain "Steps that need your computer" list with no run button. Setup commands are otherwise not edited here, and idle-session policy stays internal. |
| Private optional `ProSettings` | Dedicated Pro overview content: account identity, plan choices, browser checkout/portal, usage, and progressive connection/privacy/security sections; **Sign in** beside **See plans** for signed-out users; a browser sign-in that ended (`status.ts` `signInNote`) keeps the plans with one quiet line; sign-out asks first (ConfirmDialog) for **Sign out everywhere** and when a project is running elsewhere (its opaque holder never identifies cloud), and `sign_out_pending` reads as a quiet signed-out line. |
| `DocumentsSettings.svelte` | The Documents panel: the opt-in AGENTS.md / Claude skill installs (see the exception above). |
| `agentDocs.ts` | Wire types + `getAgentDocs`/`installAgentDocs` for `/api/v1/agent-docs`. |
| `PluginsSettings.svelte` | Settings → Plugins: every installed plugin's declared settings (see the exception above); slotted after Extensions. |
| `ActivitySettings.svelte` | The Activity panel (see the exception above): this week's sessions and tokens + time worked, one 14-day sessions chart, by agent and model / by workspace, Export CSV. Fetches while Settings is visible and on the history nudge. |
| `jump.ts` | `settingsJump` / `requestSettingsSection`: open Settings scrolled to a section (Quick Open "Activity", the dashboard's activity line) or to a setting's row by id (the Mastermind panel's "Agent communication is off"). |
| `UpdatesStatus.svelte` | The Updates section's status block: app / daemon / agents, each up to date, available, or couldn't check (with why), plus "check now". A `managed` daemon (the account's cloud) reads `MANAGED_UPDATES` with no "check now" and no auto-check switch (`SettingsView` hides the row). Reads `workspace/update.svelte.ts`; the auto-check switch below it is a schema row. |
| `NotificationStatus.svelte` | The Notifications section's status line: whether the OS (native) or browser will show alerts, with Allow / Open System Settings / Send test. The switches below it are ordinary schema rows. |
| `SettingRow.svelte` | One schema-driven control. |
| `SettingsJson.svelte` | The raw-JSON editor (validates against the schema). |

## Invariants / gotchas

- **Sparse map: default == delete.** A value equal to its schema default is
  *removed* from the persisted map, not stored. So "reset to default" and "delete
  the key" are the same operation — don't persist defaults.
- **Importing the store has a side effect.** `store.svelte.ts` runs
  `applyAppearance()` at module load (first-paint theme + typography), and it's imported widely —
  so importing it mutates document styles. Intentional, but be aware of import-order
  sensitivity during any restructure. Interface chrome consumes the shared `--text-*`
  scale; chat overrides it locally, while terminal/editor keep content-specific settings.
- **The `dirtySince` echo-guard** ignores our own writes coming back over the
  `/ws/events` settings-change push, so a local edit doesn't fight itself. Keep it
  when you touch the persist path.
- **UI quality is an acceptance criterion.** Use the theme tokens; light and dark
  both hold.
- **Setup survives a missed catalog refresh.** `workspace/agentSetup.ts` remembers
  each pending operation in the window's session storage before POST, replacing
  its ID when it joins existing work. Clear it only after showing the result;
  closing the dialog or a lost response must not discard a failure or update.

Idle cloud status means available on demand, not a manual Start task. Opening
Agent connections only looks (one passive read); only Connect, Disconnect and
sign-in steps wake the cloud, and passive polling never does.
The page describes the requested work without narrating machine power state,
and no sentence calls the cloud a machine (`pro/vocabulary.test.ts`).
Native project-copy status remains visible while compute is idle. Project privacy
shows the last recorded copy and actual blockers; healthy file counts and storage
quotas are not a task list. Setup commands and idle-session pins are not account
controls; the only setup decision here is confirming or dismissing a command an
agent proposed, and it applies only to the exact command shown. Generic missing-environment diagnostics are not shown
as missing credentials or instructions without a verified integration requirement.
Once shown, the provider component remains mounted (hidden only in an outage or
error) across readiness changes, so unrelated status refreshes cannot reset sign-in.
Ordinary connection rows exclude managed cloud workers; they remain available
through automatic Pro routing. Background plan checks retain the last confirmed
badge while pending; confirmed sign-out or failed account reads clear it, and a
connection warning never does.

Project copy status shows additive Git staging evidence: synced, uncaptured older
checkpoint, or conflicts with both index snapshots saved. Advanced recovery
references are relative to the actual Git directory (`chimaera-staging/<token>`),
including linked worktrees; they are plain selectable text, never file URLs.
