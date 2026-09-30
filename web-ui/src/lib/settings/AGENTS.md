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

**Pro is a separate account surface.** `../pro/ProView.svelte` is a singleton
workbench tab and Home view. When Pro is offered (`net/plan.ts` `proOffered`;
a build without an account endpoint offers none) Settings ends with a small
Chimaera Pro group that opens it (Pro is an optional add-on, so it follows every
working section, and its entry is neutral for every plan, never accent-tinted);
Home retains its Pro navigation. Confirmed free/signed-out users see a short
optional-benefit Get Pro entry; paid users see Your Chimaera Pro/Max and View
account. The Keyboard section carries its reference chords inside it, so a
group after it never splits them off. `../net/plan.ts` shares its existing subscription between this entry
and the badges: loading or failed/unknown entitlement stays neutral, never a
sales prompt or an active-plan claim. There is no extra poll or cloud wake. A paid workspace plan badge also
opens it, without adding a full-width sidebar row for any plan.
It renders `ProSettings.svelte`, which invokes the native `pro_*` commands.
The generic Settings form no longer embeds account/billing/onboarding controls.
An account browser on a cloud worker opens the same provider flow; other hosts
link to the account's billing page (`/account/billing`). Ordinary browser daemons
have no Pro entry. An account browser
shows a Cloud category only on the cloud machine's own page (a passive
`isCloudMachine` read); it remains host-pinned and cookie-authenticated.
On the web's Home (the account's own page, `net/base.ts` `isAccountHome`; see
[pro](../pro/AGENTS.md) `AccountHome.svelte`) there is no daemon: `SettingsView`
with `account` shows only Chimaera Pro, rendered in place by
`pro/BrowserAccount.svelte` (plan, usage, the billing link and Sign out), with no
search, JSON tab or daemon settings.
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
home. The page renders from the last
confirmed status (`pro/account.ts` `accountPanel`) while background reads run, so
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
a limit (`nearLimit`) can choose Upgrade to Max, review the interval locally, then explicitly open the
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
0.2 platform, docs/plugin-platform-plan.md §9) through
`plugins/PluginSettings.svelte`, the rows its card shows too. The daemon owns
the values (`GET`/`PUT /api/v1/plugins/{pid}/settings`, kept per plugin
under `<data dir>/plugins/.data/`, never `settings.json`) and checks each
against its declaration; search matches plugin names and setting labels.

**Exception — Activity.** Also store-backed and read-only: `ActivitySettings.svelte`
shows sessions, tokens and time from the session records across every workspace
(`GET /api/v1/activity`, CSV via `/api/v1/activity/csv`, both through
`../workspace/history.ts`). No dollar figures (cost is only a CSV column); unknown is
"—" with a tooltip, never zero. `jump.ts` lets another surface open Settings at a
section (`requestSettingsSection`).

## File map

| File | What it owns |
|---|---|
| `schema.ts` | The settings schema: keys, types, defaults, labels, groups. Ground truth. |
| `store.svelte.ts` | The reactive settings store: load/patch/persist against `/api/v1/settings`, sparse-map semantics, the `dirtySince` echo-guard, and document-wide theme/interface/editor CSS variables. |
| `themes.ts` | The curated light/dark theme definitions + `applyAppearance`. |
| `AgentsSettings.svelte` | Per-agent binary/model settings (paths, managed installs). |
| `EnvironmentSettings.svelte` | The Environment prelude panel (bespoke, `/api/v1/environment`-backed — see the exception above). |
| `environment.ts` | Wire types + `getEnvironment`/`putEnvironment` for the prelude map. |
| `CloudSetup.svelte` | Automatic cloud status, bounded visible polling (no manual check); the check mark needs an agent on record (live from the connection panel, else the app's remembered `agents_connected`), none connected reads as the next step and unknown claims nothing; a ready-but-unreachable read is re-checked with the account and reported only on a second read in a row; an asleep-or-starting answer (`cloud_asleep`) is idle, never unreachable: in a browser view it keeps "Available when you need it" with the asleep line, and a wake (opening Agent connections) that finds the machine still starting shows "waking up" in that section, with no error, on the fast check cadence for at most two minutes; historical project-copy summaries and contextual provider connections; sleeping workers wake only for explicit connection management/use. Opening a repository exists only on the cloud machine's own page, as navigation; the app has none. See [provider map](../pro/AGENTS.md). |
| `MirrorSettings.svelte` | Native project-copy status and privacy; visibility-gated 15-second status refresh; the row is re-read after every change; a privacy change the account hasn't confirmed yet is a success that reads as quiet progress and is re-sent quietly (≤1/min, the first a minute after the click); each row names who runs the project (`presentation.ts` `projectPlace`), and setup reads as progress unless it failed or waits on an agent; the switch is **Keep this project on this computer**; copy work still under way and too-large files read as a muted hint, only real problems in the warning colour; `renewal_failed` reads "Reconnecting your account…"; kept-both rows name the user's versions saved beside each file; blocked-provider rows open the shared connection flow. An agent's proposed setup command (`pending_setup_command`) shows once per project, whole, with Confirm/Dismiss (`pro/profile.ts`); `profile.deferred` is a plain "Steps that need your computer" list with no run button. Setup commands are otherwise not edited here, and idle-session policy stays internal. |
| `ProSettings.svelte` | Dedicated Pro overview content: account identity, plan choices, browser checkout/portal, usage, and progressive connection/privacy/security sections; **Sign in** beside **See plans** for signed-out users; a browser sign-in that ended (`status.ts` `signInNote`) keeps the plans with one quiet line; sign-out asks first (ConfirmDialog) for **Sign out everywhere** and when a project is running in the cloud, and `sign_out_pending` reads as a quiet signed-out line. |
| `DocumentsSettings.svelte` | The Documents panel: the opt-in AGENTS.md / Claude skill installs (see the exception above). |
| `agentDocs.ts` | Wire types + `getAgentDocs`/`installAgentDocs` for `/api/v1/agent-docs`. |
| `PluginsSettings.svelte` | Settings → Plugins: every installed plugin's declared settings (see the exception above); slotted after Extensions. |
| `ActivitySettings.svelte` | The Activity panel (see the exception above): this week's sessions and tokens + time worked, one 14-day sessions chart, by agent and model / by workspace, Export CSV. Fetches while Settings is visible and on the history nudge. |
| `jump.ts` | `settingsJump` / `requestSettingsSection`: open Settings scrolled to a section (Quick Open "Activity", the dashboard's activity line). |
| `UpdatesStatus.svelte` | The Updates section's status block: app / daemon / agents, each up to date, available, or couldn't check (with why), plus "check now". Reads `workspace/update.svelte.ts`; the auto-check switch below it is a schema row. |
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

Idle cloud status means available on demand, not a manual Start task. Opening
Agent connections explicitly requests access; passive polling never wakes compute.
The page describes the requested work without narrating machine power state.
Native project-copy status remains visible while compute is idle. Project privacy
shows the last recorded copy and actual blockers; healthy file counts and storage
quotas are not a task list. Setup commands and idle-session pins are not account
controls; the only setup decision here is confirming or dismissing a command an
agent proposed, and it applies only to the exact command shown. Generic missing-environment diagnostics are not shown
as missing credentials or instructions without a verified integration requirement.
After its first connection the provider component remains mounted while hidden
across readiness changes, so unrelated status refreshes cannot reset sign-in.
Ordinary connection rows exclude managed cloud workers; they remain available
through automatic Pro routing. Background plan checks retain the last confirmed
badge while pending; confirmed sign-out or failed account reads clear it, and a
connection warning never does.
