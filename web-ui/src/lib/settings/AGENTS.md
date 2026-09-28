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
workbench tab and Home view. Settings starts with a small Chimaera Pro group
that opens it; Home retains its Pro navigation. Confirmed free/signed-out users
see a benefit-led Get Pro entry; paid users see Your Chimaera Pro/Max and View
account. `../net/plan.ts` shares its existing subscription between this entry
and the badges: loading or failed/unknown entitlement stays neutral, never a
sales prompt or an active-plan claim. There is no extra poll or cloud wake. A paid workspace plan badge also
opens it, without adding a full-width sidebar row for any plan.
It renders `ProSettings.svelte`, which invokes the native `pro_*` commands.
The generic Settings form no longer embeds account/billing/onboarding controls.
An account browser on a cloud worker opens the same provider flow; other hosts
link to `/account`. Ordinary browser daemons have no Pro entry. Their Cloud machine category remains host-pinned and cookie-authenticated.
Credentials stay in the app's keychain, never in the schema or this UI.

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
Unknown/startup/error account states stay neutral. Neither a remembered selection
nor a successful account refresh can trigger checkout. Usage shows percentages
computed from the account's current allowances, not fixed hour or storage totals. The static introduction shows
project/conversation continuity; it is not live setup progress or arbitrary
process migration.
Native billing attempts own their finite confirmation lifetime even if this view
is hidden or closed. The UI has no checkout polling loop; it displays native
opening/waiting/confirming/result states and refreshes cached status on events or return.
Subscriber billing feedback stays inside the account card, preserving cloud and
provider panels. Account events immediately invalidate older reads; billing
snapshots cannot roll back to an older attempt or phase. Pro subscribers can
choose Upgrade to Max, review the interval locally, then explicitly open the
hosted price/proration confirmation. Only server-confirmed Max changes the plan.
Only an authoritative active account plan unlocks paid content. Expired/failed
checkout stays neutral until an explicit Check account succeeds with no plan for
the same attempt; Return to plans then acknowledges and clears that attempt. A targeted native
return reopens Pro, including when a new window needed time to mount.

## File map

| File | What it owns |
|---|---|
| `schema.ts` | The settings schema: keys, types, defaults, labels, groups. Ground truth. |
| `store.svelte.ts` | The reactive settings store: load/patch/persist against `/api/v1/settings`, sparse-map semantics, the `dirtySince` echo-guard, and document-wide theme/interface/editor CSS variables. |
| `themes.ts` | The curated light/dark theme definitions + `applyAppearance`. |
| `AgentsSettings.svelte` | Per-agent binary/model settings (paths, managed installs). |
| `EnvironmentSettings.svelte` | The Environment prelude panel (bespoke, `/api/v1/environment`-backed — see the exception above). |
| `environment.ts` | Wire types + `getEnvironment`/`putEnvironment` for the prelude map. |
| `CloudSetup.svelte` | Automatic cloud status, bounded visible polling, historical project-copy summaries and contextual provider connections; sleeping workers wake only for explicit connection management/use. Repository/key details stay secondary. See [provider map](../pro/AGENTS.md). |
| `MirrorSettings.svelte` | Native laptop-daemon mirror status, privacy, setup profile and persistent session pins; visibility-gated 15-second status refresh; blocked-provider rows open the shared connection flow. |
| `ProSettings.svelte` | Dedicated Pro overview content: account identity, plan choices, browser checkout/portal, usage, and progressive connection/privacy/security sections. |
| `DocumentsSettings.svelte` | The Documents panel: the opt-in AGENTS.md / Claude skill installs (see the exception above). |
| `agentDocs.ts` | Wire types + `getAgentDocs`/`installAgentDocs` for `/api/v1/agent-docs`. |
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
After its first connection the provider component remains mounted while hidden
across readiness changes, so unrelated status refreshes cannot reset sign-in.
Ordinary connection rows exclude managed cloud workers; they remain available
through automatic Pro routing. Background plan checks retain the last confirmed
badge while pending; confirmed sign-out or failed account reads clear it.
