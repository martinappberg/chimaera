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
No schema rows, and an explicit Save that PATCHes only the scopes whose editor
changed (`planEnvironmentSave`): an unchanged scope stays out of the body, so a
value another window saved meanwhile survives, and the daemon serializes
concurrent patches behind the store's disk gate (deliberately not its mutex,
which is never held across I/O). An empty editor sends `null`, deleting
its scope's entry rather than persisting `{text: ""}`. The editors stay live
during a save; each sent scope's baseline becomes the text that was sent, so
keystrokes typed meanwhile remain unsaved.

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
generically), then **Agent communication** — `agents.communication.enabled`,
`agents.communication.wakes` and `agents.communication.freeSubagents` — as ordinary
`SettingRow`s under their own subheading (`SettingsView`, every
`agents.communication.*` row). The schema has no "show only when" yet, so the
wakes row always shows.

## File map

| File | What it owns |
|---|---|
| `SettingsView.svelte` | The surface itself, loaded lazily (`layout/lazyViews.ts`). Mounted in two places: a singleton layout tab in a workspace, and a full page over Home (`App.svelte` `homeSettingsOpen`) where no workspace is open, so it must not assume one. |
| `schema.ts` | The settings schema: keys, types, defaults, labels, groups. Ground truth. |
| `store.svelte.ts` | The reactive settings store: load/patch against `/api/v1/settings`, sparse-map semantics, the echo-guard, `settingsSaveState()`, and document-wide theme/interface/editor CSS variables. |
| `settingsWriter.ts` | The single writer behind `PUT /api/v1/settings`: `idle → pending → inflight → idle`, `failed` (no answer; bounded, visibility-floored backoff via `net/reconnect.ts` `retryDelayMs`) or `rejected` (refused, with the daemon's reason). The map is the queue: one body, the current one. |
| `themes.ts` | The curated light/dark theme definitions + `applyAppearance`. |
| `AgentsSettings.svelte` | Installed-agent cards; Advanced paths and removal. A card the login shell couldn't check (`unchecked`) reads "Couldn't check" with the shell's reason and a Retry (`recheckAgent`, the patient re-detect) instead of Install; a second miss says "Still couldn't check." Opens the shared `../workspace/AgentSetupDialog.svelte` through `agentSetup.ts` for install/update/reinstall and recoverable progress/results; the compact dialog keeps technical output under Details and, after an install lands for a signed-out agent, shows the in-app sign-in card (`../workspace/AgentSignInCard.svelte`), never the agent's terminal. A signed-out card's **Sign in** opens that same card under its row (`signingIn`; the card a chat shows), which follows the login and reloads the catalog once the CLI confirms it. Each Claude / Codex / Grok / Antigravity card has a **Plan usage** section beside Advanced, collapsed by default (`agentUsage.ts`, read when first opened and on Refresh, drawn with `../shared/UsageBars.svelte`). Polls only while visible and preserves unfinished edits. |
| `agentUsage.ts` | `getAgentUsage` for `GET /api/v1/agents/{id}/usage` (the account's plan windows, read live from the agent's CLI) and `USAGE_AGENTS` (the agents that have such a read; Cursor doesn't). `ACCOUNT_USAGE_IN_CHAT` names the agents whose chat stream reports no limits (Antigravity), so their chat header shows this read instead; the others' streams report their own, and a chat never spawns a CLI ahead of them. |
| `agentStatus.ts` | Honest update and installer status rules; unknown/failure never means up to date or installed. |
| `EnvironmentSettings.svelte` | The Environment prelude panel (bespoke, `/api/v1/environment`-backed — see the exception above). |
| `environment.ts` | Wire types + `getEnvironment`/`patchEnvironment` (and the whole-map `putEnvironment`) for the prelude map; `planEnvironmentSave`/`baselinesAfterSave`, the panel's save logic. |
| `DocumentsSettings.svelte` | The Documents panel: the opt-in AGENTS.md / Claude skill installs (see the exception above). |
| `agentDocs.ts` | Wire types + `getAgentDocs`/`installAgentDocs` for `/api/v1/agent-docs`. |
| `PluginsSettings.svelte` | Settings → Plugins: every installed plugin's declared settings (see the exception above); slotted after Extensions. |
| `ActivitySettings.svelte` | The Activity panel (see the exception above): this week's sessions and tokens + time worked, one 14-day sessions chart, by agent and model / by workspace, Export CSV. Fetches while Settings is visible and on the history nudge. |
| `jump.ts` | `settingsJump` / `requestSettingsSection`: open Settings scrolled to a section (Quick Open "Activity", the dashboard's activity line) or to a setting's row by id (the Mastermind panel's "Agent communication is off"); `settingsOpen` / `openSettingsAt` for a surface that can't reach App's callbacks (a chat's usage popover → "Agents"; App watches it). |
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
  Defaults: interface 13.5 px (`--text-xs/sm/md/lg` = 11.5/12.5/13.5/15.5, mirrored as
  `app.css` fallbacks), chat 14 px at line height 1.57, rendered Markdown 14 px.
- **Reading Width (`chat.contentWidth`) is the column width**, not a cap on 48em:
  `ChatView`'s `--chat-column` and `MarkdownView`/`mdLive`'s `--doc-measure` use it
  directly (default 680, the desktop app's column). Wrapping it in a `min()` with an em measure
  again makes every wider value a no-op — the bug #284 fixed.
- **Every key needs a consumer.** A new schema row is read at its consumption site
  (`getSetting`, or `settings.rs` for daemon keys) and applies live; the PR that adds
  it says where. Numeric rows can carry a display `unit`; a slider shows only when the
  range has ≤120 steps.
- **The echo-guard** (`pending`, `inflight` or `failed`) ignores settings frames
  from the first local edit until the daemon accepts it, so neither our own echo nor an
  older broadcast reverts what the user is typing. Keep it when you touch the
  persist path.
- **A failed write is retried, never dropped.** The writer retries a transient
  failure (no answer, 5xx, 408, 429) on its backoff; a settings frame while
  `failed` means the events socket reconnected, so it re-sends at once instead
  of being ignored. A refused map (`rejected`, any other 4xx such as 413) is
  never re-sent unprompted and does not guard: re-sending cannot succeed, and
  guarding would deafen the window to every other window, so the next frame
  replaces it. An older PUT's answer never clears newer edits: only an accepted
  body equal to the current map returns to `idle`. `flushSettings()` resolves
  `true` once the daemon holds every edit made before the call; it aborts an
  outstanding older PUT and its waiters follow the newer one. The abort cannot
  recall a request already delivered, so the daemon may still apply it.
  Settings shows **unsaved** while `failed` or `rejected` (the tooltip says
  which, with the daemon's reason); clicking it re-sends now.
- **UI quality is an acceptance criterion.** Use the theme tokens; light and dark
  both hold.
- **Setup survives a missed catalog refresh.** `workspace/agentSetup.ts` remembers
  each pending operation in the window's session storage before POST, replacing
  its ID when it joins existing work. Clear it only after showing the result;
  closing the dialog or a lost response must not discard a failure or update.
