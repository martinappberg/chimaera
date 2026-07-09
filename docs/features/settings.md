# Settings

One flat JSON object of dotted keys (`terminal.fontSize`, `git.path`, `daemon.scrollbackLines`,
…) at `~/.config/chimaera/settings.json` — the ground truth every surface reads. Both the UI and
a hand-editor (vim over ssh) are first-class; a change from either propagates live.

**Where it lives (shared):** UI `web-ui/src/lib/settings/` (`SettingsView.svelte`,
`AgentsSettings.svelte`, `SettingRow.svelte`, `SettingsJson.svelte`, `schema.ts`, `store.svelte.ts`,
`themes.ts`). Daemon `crates/chimaera-server/src/settings.rs`. Wire: `GET/PUT /api/v1/settings` and
a `settings` frame on `/ws/events`. Map: [settings/CLAUDE.md](../../web-ui/src/lib/settings/CLAUDE.md).

## The settings model

- **What & when.** The single store for user preferences — terminal appearance, the git binary path,
  scrollback, quick-open ignore dirs, agent paths, update behavior, and the theme.
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

## Agents settings & themes

- **Agents panel** (`AgentsSettings.svelte`) — install/uninstall managed agent runtimes and set an
  explicit binary path per agent (see [agents.md](agents.md)).
- **Theme palettes** (`themes.ts`) — each theme carries its own hand-tuned 16-color terminal ANSI
  palette alongside the UI tokens; a UI theme without a terminal palette is "half a theme". UI quality
  (curated light/dark) is an acceptance criterion, per [rules/web-ui.md](../../.claude/rules/web-ui.md).

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

_No intent captured yet — pending the next `feat:` in this area._
