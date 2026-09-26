# web-ui/src/lib/plugins — the Plugins tab + the attach sheet

Orientation for coding agents. The client half of the plugin seam (design:
[docs/timeline-knowledge-plugins-plan.md](../../../../docs/timeline-knowledge-plugins-plan.md)
§6, §6.2, §6.4, §6.6). Parent map: repo-root [AGENTS.md](../../../../AGENTS.md).
The daemon side is `crates/chimaera-server/src/plugins/`.

## File map

| File | What it owns |
|---|---|
| `store.ts` | Wire types + fetchers for every plugin route (workspace plugins, `PUT …/{pid} {on}`, agent-plugins, skills, install / setup / trust-hooks), the active workspace's reactive plugin status (`workspacePlugins`, `knowledgeProviderActive`, `myceliumPlugin`), and the attach-sheet request (`attachRequest` / `openAttachSheet` / `closeAttachSheet`) that App.svelte hosts as ONE modal for every surface. |
| `PluginsView.svelte` | The tab shell: header, the `Segmented` Installed · Skills · Browse (Browse disabled "later"), the host chip; fetches agent-plugins / skills for the shown view on show and on return. |
| `InstalledView.svelte` | Workbench cards (glyph tile · name · version · summary · `Switch` = on in THIS workspace · Here / Adds ("Would add" when off) / Needs lines; per-agent requirement chips with Install and "Review & trust →"), then the Agents section (what each CLI reports about its own plugins). |
| `SkillsView.svelte` | Every skill each agent can use here: counts, filter chips (All · claude · codex · only one agent), search, groups by origin (this project · from plugins · yours · built into the agent), per-agent chips (✓ · ◌ + reason · —), the detail aside with each agent's own invocation syntax and codex's load errors. |
| `skillsModel.ts` | Pure grouping/counting/filtering + `invokeSyntax`. Vitest: `skillsModel.test.ts`. |
| `AttachSheet.svelte` | "Use mycelium for Knowledge": 1 installed for your agents (Install → a visible terminal, opened as a session) · 2 trust codex's hooks (each in plain words; the Stop hook's caveat; exactly the `{key, hash}` pairs shown are posted) · 3 set up this workspace (agent select → the plugin's own setup prompt in a new chat session, "billed to your <agent> account"). Completing also switches the plugin on here. Re-checks on every open. |

## Invariants / gotchas

- **Agent state comes from the agents** (the daemon's probes of `claude
  plugin list` / codex `app-server`), never re-derived here. A 404 from a
  route the daemon doesn't have yet renders an honest line, not a spinner.
- **"Adds" is mandatory** on every card — it is what makes opt-in honest.
- **Hook trust is never automated.** The sheet lists what codex will run and
  posts only the hashes the user saw; the daemon writes only those whose
  current hash still matches.
- **Installs and setups are the CLIs' own commands** in a visible session;
  the sheet closes and opens that session.
- **Canonical vocabulary**: `/name` for claude, `$name` for codex; plugin ids
  as the agents report them.
