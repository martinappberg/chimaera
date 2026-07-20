# Settings

One flat JSON object of dotted keys (`terminal.fontSize`, `git.path`, `daemon.scrollbackLines`,
…) at `~/.config/chimaera/settings.json` — the ground truth every surface reads. Both the UI and
a hand-editor (vim over ssh) are first-class; a change from either propagates live.

**Where it lives (shared):** UI `web-ui/src/lib/settings/` (`SettingsView.svelte`,
`AgentsSettings.svelte`, `SettingRow.svelte`, `SettingsJson.svelte`, `schema.ts`, `store.svelte.ts`,
`themes.ts`). Daemon `crates/chimaera-server/src/settings.rs`. Wire: `GET/PUT /api/v1/settings` and
a `settings` frame on `/ws/events`. Map: [settings/AGENTS.md](../../web-ui/src/lib/settings/AGENTS.md).

## The settings model

- **What & when.** The single store for user preferences — interface/chat/editor/terminal
  typography, themes, dashboard behavior, file and quick-open behavior, keybindings, runtime paths,
  daemon persistence, and update behavior.
- **How it's used.** The Settings pane (a singleton tab) edits keys through typed rows; a raw-JSON
  editor (`SettingsJson.svelte`) is available for anything the schema doesn't surface. `GET
  /api/v1/settings` returns the map; `PUT /api/v1/settings` replaces it whole (≤256 KB, 204). Changes
  broadcast on `/ws/events` so every window converges.
- **Where it lives.** `settings.rs` (`get_settings`/`put_settings`, `SettingsStore`); UI schema in
  `web-ui/src/lib/settings/schema.ts` (where defaults live).
- **Key behaviors.** Reads **re-stat the file** so external edits surface without a restart, bumping a
  content generation that `/ws/events` diffs against. Only a handful of keys are daemon-consumed
  (`git.path`, `agents.*.path`, `daemon.scrollbackLines`, `daemon.restoreSessions`, `update.autoCheck`,
  `quickOpen.ignoreDirs`); everything else is opaque and preserved verbatim (forward-compat — a newer
  UI's keys survive an older daemon). A corrupt/oversized/non-object file degrades to an empty map with
  a warning — settings must never brick the daemon. A changed `agents.*.path` triggers shim regeneration
  + a detection-cache drop.

## Settings surface and typography domains

- **The schema is the contract.** Every typed row, default, range, category, JSON completion, and
  validation message derives from `schema.ts`; default values remain sparse (choosing a default
  deletes that key). The UI covers Appearance, Agents, Environment, Dashboard, Chat, Terminal,
  Editor, Files, Quick Open, Git, Daemon, Updates, and Keyboard. Environment is the deliberate
  exception: its multiline preludes use `/api/v1/environment` and `env-profiles.json`.
- **Interface typography applies app-wide.** `appearance.interfaceFontSize` (13 px by default)
  rebuilds the shared `--text-xs`/`--text-sm`/`--text-md`/`--text-lg` scale live, so the rail,
  file tree, pane tabs, dashboard, settings, dialogs, Git, and preview chrome move together.
  `appearance.interfaceFontFamily` supplies the shared UI font stack. Small chrome uses those
  tokens rather than fixed rem/px sizes.
- **Content surfaces stay independently legible.** Chat defaults to a 13.5 px base and overrides
  the shared scale within each `ChatView` (including the dashboard's embedded Mastermind) and
  exposes font size/family, line height, and reading width. Terminal keeps its xterm-specific
  font controls. Editor typography
  covers code, diffs, and the JSON editor; rendered Markdown defaults to the same 13.5 px as Chat
  but has its own font-size and line-height settings rather than borrowing `terminal.fontSize`.
  CodeMirror views reconfigure while mounted.
- **Newer surfaces are represented.** `dashboard.landing` controls the workspace landing and
  `dashboard.cardDensity` selects automatic, comfortable, or compact agent cards. Mastermind's
  agent/mode are workspace state edited in its setup card, while Environment preludes remain scoped
  records rather than being flattened into global preferences.

## Agents settings & themes

- **Agents panel** (`AgentsSettings.svelte`) — update/uninstall managed agent runtimes and set an
  explicit binary path per agent; "re-check" also probes upstream for each agent's latest release,
  a known-newer release shows an accent "update → \<new\>" button on managed rows and a quiet
  "\<new\> available" note on your own (see [agents.md](agents.md)).
- **Theme palettes** (`themes.ts`) — each theme carries its own hand-tuned 16-color terminal ANSI
  palette alongside the UI tokens; a UI theme without a terminal palette is "half a theme". UI quality
  (curated light/dark) is an acceptance criterion, per [rules/web-ui.md](../../.claude/rules/web-ui.md).

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Why settings exist
_Captured 2026-07-09 — drafted from context, reframed by the maintainer._

- **The vision (this is the intent).** Settings isn't really a design *choice* so much as a standing
  vision: **everything should be customizable.** The store is the ground-truth expression of that —
  the UI and a hand-editor (vim over ssh) are both first-class against it because you often reach the
  daemon only over ssh.
- **Incidental (not intent).** The mechanics — which keys the daemon consumes, hand-edit re-stat +
  broadcast, never-brick-on-corrupt, defaults living in the web-ui schema — are how it's implemented
  today, not the point.
- **Do not change:** the direction — that more of the app becomes user-customizable over time.
