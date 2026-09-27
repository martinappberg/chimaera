# web-ui/src/lib/plugins — the Extensions tab + the attach sheet

Orientation for coding agents. The client half of the plugin seam (design:
[docs/timeline-knowledge-plugins-plan.md](../../../../docs/timeline-knowledge-plugins-plan.md)
§6, §6.2, §6.4, §6.6). Parent map: repo-root [AGENTS.md](../../../../AGENTS.md).
The daemon side is `crates/chimaera-server/src/plugins/`.

The user-facing surface is the **Extensions** tab (page title, pane tab label,
quick-open entry — "plugins" and "skills" still find it — and the dock row),
with three equal segments: **Plugins** · **Skills** · **Browse** (disabled,
"later"). Only the words changed: the layout surface id, the wire
(`{v:"plugins"}`, `/api/v1/plugins`), the store and these file names stay
"plugins". Its glyph (2×2 cells, the top-right a plus) is `glyph.ts`, drawn by
PaneTabs and App's dock row — not in `shared/icons.ts`, which `prebuild`
regenerates from Tabler.

## File map

| File | What it owns |
|---|---|
| `store.ts` | Wire types + fetchers for every plugin route (workspace plugins, `PUT …/{pid} {on}`, agent-plugins, skills, install / setup / trust-hooks; the plugin routes `POST /plugins/install`, `POST /plugins/{pid}/install` (a first-party plugin at the version chimaera pins), `/plugins/{pid}/update` · `rollback` · `check`, `DELETE /plugins/{pid}`), the active workspace's reactive plugin status (`workspacePlugins`, `knowledgeProviderActive`, `myceliumPlugin`; `changeWorkbenchPlugin` runs one change, `installWorkbenchPlugin` one install from a repository and `installFirstPartyPlugin` one pinned install, each re-syncing the cards and Knowledge), `isMissingRoute` (a bare 404 — the daemon predates the route — vs a refusal it explains), and the attach-sheet request (`attachRequest` / `openAttachSheet` / `closeAttachSheet`) that App.svelte hosts as ONE modal for every surface. `WorkspacePlugin` carries the catalog wire: `version` (installed, or the pinned one for an available entry), `api` (`""` when null), `source` (`installed` / `available`), `installed`, `first_party`, `verified`, `requires` + `recommends`, and — null when absent — `sha256_wasm`, `repo`, `pinned_version`, `local_path`, `path`, `previous`, `update`, `fault`. `normalizePlugin` defaults all of it for an older daemon (`installed` = `source !== "available"`). |
| `PluginsView.svelte` | The tab shell: the "Extensions" header with the segment's subtitle, the `Segmented` Plugins · Skills · Browse (equal widths via a `:global(.seg)` grid; Browse disabled "later"), the "on <host>" chip ("Plugins are installed per host"); fetches agent-plugins / skills for the shown segment on show and on return. |
| `InstalledView.svelte` | The Plugins segment. **Chimaera plugins** ("sandboxed, inside chimaera"): a card per catalog entry — glyph tile · name · version (tooltip "chimaera pins x" when an installed first-party copy runs another) · quiet `pill neutral small` tags (`chimaera` for first_party, `verified` with the full `plugin.wasm` sha256, `unverified · local` naming the directory) · an **Update to x.y.z** chip · summary · then the state: **Install x.y.z** for an available entry (the pinned version; outcome line with the verified sha256) instead of the `Switch` = on in THIS workspace. Under it, plain sentences, no labels: what it found here (or "nothing detected yet — set it up →"), "For agents: …" / "For you: …", the requirement rows from `requirementsModel.ts` (pills, **Install for <agent>** / **Install**, the hook-trust pill + "Review & trust →"), the installed line ("installed 0.1.1 · 0.1.0 available to go back to" + **Use previous**, **Check now**, **Remove** behind a `ConfirmDialog`), the `fault`, one outcome line per change. Then the **Install from a repository** row (`owner/repo` or its github.com URL → **Install**; the outcome line or the daemon's refusal). **Agent plugins** ("inside each agent, managed with its own plugin manager"): what each CLI reports about its own plugins. |
| `requirementsModel.ts` | Pure: a plugin's `requires` / `recommends` × the agents report (+ its fetch state) → the rows and sentences the card and the sheet render ("Requires the claude plugin x" + pill; "For claude: mycelium ✓ 0.7.2"; "For claude: install the x plugin so it can record knowledge"; "checking the agents…" once; the "neither is installed on this host" notice; the "can't check on this daemon" pill), `sheetText` (the sheet's required-vs-optional wording) and `hooksAwaitingTrust`. Vitest: `requirementsModel.test.ts`. |
| `installCopy.ts` | Pure: the Install button's version + tooltip and the install / update outcome lines, shared by the card and the sheet. Vitest: `installCopy.test.ts`. |
| `glyph.ts` | The Extensions glyph's path + stroke width (16×16, the hand-made surface glyphs' grid). |
| `SkillsView.svelte` | Every skill each agent can use here: counts, filter chips (All · claude · codex · only one agent), search, groups by origin (this project · from plugins · yours · built into the agent), per-agent chips (✓ · ◌ + reason · —), the detail aside with each agent's own invocation syntax and codex's load errors. |
| `skillsModel.ts` | Pure grouping/counting/filtering + `invokeSyntax`. Vitest: `skillsModel.test.ts`. |
| `AttachSheet.svelte` | "Use mycelium for Knowledge": (when the plugin is only available) install it first — the card's Install flow · the agent plugins it requires or recommends, for the agents installed here (the model's rows; a recommendation reads "optional: …"; Install → a visible terminal, opened as a session) · trust codex's hooks (each in plain words; the Stop hook's caveat; exactly the `{key, hash}` pairs shown are posted; hooks of required and recommended agent plugins alike) · set up this workspace (agent select → the plugin's own setup prompt in a new chat session, "billed to your <agent> account"). Completing also switches the plugin on here. Re-checks on every open and after the install. |

## Invariants / gotchas

- **Agent state comes from the agents** (the daemon's probes of `claude
  plugin list` / codex `app-server`), never re-derived here. A 404 from a
  route the daemon doesn't have yet renders an honest line, not a spinner.
- **Never list an agent that isn't installed on this host.** A
  recommendation for it says nothing; a requirement none of the listed
  agents here can meet says so once. `requirementsModel.ts` owns this rule —
  change it there, with its test.
- **What a plugin adds is on every installed card** ("For agents" / "For
  you") — it is what makes opt-in honest. An available entry has no
  manifest yet, so it shows its summary and its Install button only.
- **Hook trust is never automated.** The sheet lists what codex will run and
  posts only the hashes the user saw; the daemon writes only those whose
  current hash still matches.
- **Installs and setups of agent plugins are the CLIs' own commands** in a
  visible session; the sheet closes and opens that session.
- **Canonical vocabulary**: `/name` for claude, `$name` for codex; plugin ids
  as the agents report them.
- **Versions, sources and verification come from the daemon** (the lock, the
  installed copies, the SHA256SUMS checks); the card never compares versions
  itself beyond showing `pinned_version` in a tooltip. An available entry
  installs the version chimaera pins — never "the latest". The daemon never
  downloads on its own: Install, Update, Use previous and Remove are the
  user's clicks, each re-syncing the cards. The add row passes what was
  typed through (trimmed); the daemon normalizes the URL form and refuses
  bad input, so the UI never re-validates `owner/repo`.
