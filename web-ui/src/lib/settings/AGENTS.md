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

**Exception — Caffeinate.** `CaffeinateSettings.svelte` is a macOS native-shell,
local-window-only device panel. It reads/writes the shell IPC state persisted as
`caffeinate.json`; it is deliberately absent from the daemon schema and JSON tab,
so opening Settings against a remote daemon never writes a laptop power choice there.

## File map

| File | What it owns |
|---|---|
| `schema.ts` | The settings schema: keys, types, defaults, labels, groups. Ground truth. |
| `store.svelte.ts` | The reactive settings store: load/patch/persist against `/api/v1/settings`, the sparse-map semantics, and the `dirtySince` echo-guard. |
| `themes.ts` | The curated light/dark theme definitions + `applyAppearance`. |
| `AgentsSettings.svelte` | Per-agent binary/model settings (paths, managed installs). |
| `CaffeinateSettings.svelte` | Native Mac's device-local Caffeinate detail + control (not settings.json). |
| `CaffeinateConsent.svelte` | Versioned first-enable explanation reached from the compact rail/tray controls. |
| `EnvironmentSettings.svelte` | The Environment prelude panel (bespoke, `/api/v1/environment`-backed — see the exception above). |
| `environment.ts` | Wire types + `getEnvironment`/`putEnvironment` for the prelude map. |
| `SettingRow.svelte` | One schema-driven control. |
| `SettingsJson.svelte` | The raw-JSON editor (validates against the schema). |

## Invariants / gotchas

- **Sparse map: default == delete.** A value equal to its schema default is
  *removed* from the persisted map, not stored. So "reset to default" and "delete
  the key" are the same operation — don't persist defaults.
- **Importing the store has a side effect.** `store.svelte.ts` runs
  `applyAppearance()` at module load (first-paint theme), and it's imported widely —
  so importing it mutates document styles. Intentional, but be aware of import-order
  sensitivity during any restructure.
- **The `dirtySince` echo-guard** ignores our own writes coming back over the
  `/ws/events` settings-change push, so a local edit doesn't fight itself. Keep it
  when you touch the persist path.
- **UI quality is an acceptance criterion.** Use the theme tokens; light and dark
  both hold.
