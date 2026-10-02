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
| `AgentsSettings.svelte` | Installed-agent cards; Advanced paths and removal. Opens the shared `../workspace/AgentSetupDialog.svelte` through `agentSetup.ts` for install/update/reinstall and recoverable progress/results. Polls only while visible and preserves unfinished edits. |
| `agentStatus.ts` | Honest update and installer status rules; unknown/failure never means up to date or installed. |
| `EnvironmentSettings.svelte` | The Environment prelude panel (bespoke, `/api/v1/environment`-backed — see the exception above). |
| `environment.ts` | Wire types + `getEnvironment`/`putEnvironment` for the prelude map. |
| `DocumentsSettings.svelte` | The Documents panel: the opt-in AGENTS.md / Claude skill installs (see the exception above). |
| `agentDocs.ts` | Wire types + `getAgentDocs`/`installAgentDocs` for `/api/v1/agent-docs`. |
| `PluginsSettings.svelte` | Settings → Plugins: every installed plugin's declared settings (see the exception above); slotted after Extensions. |
| `ActivitySettings.svelte` | The Activity panel (see the exception above): this week's sessions and tokens + time worked, one 14-day sessions chart, by agent and model / by workspace, Export CSV. Fetches while Settings is visible and on the history nudge. |
| `jump.ts` | `settingsJump` / `requestSettingsSection`: open Settings scrolled to a section (Quick Open "Activity", the dashboard's activity line) or to a setting's row by id (the Mastermind panel's "Agent communication is off"). |
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
