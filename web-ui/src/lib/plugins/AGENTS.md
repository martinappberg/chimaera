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
"plugins". Its glyph (2×2 cells, the top-right a plus) is `glyph.ts`, drawn
only through `ExtensionsGlyph.svelte` by PaneTabs, App's rail row and
QuickOpen — not in `shared/icons.ts`, which `prebuild` regenerates from Tabler.

## File map

| File | What it owns |
|---|---|
| `store.ts` | Wire types + fetchers for every plugin route (workspace plugins, `PUT …/{pid} {on}`, agent-plugins, skills, install / setup / trust-hooks; the plugin routes `POST /plugins/install`, `POST /plugins/{pid}/install` (a first-party plugin at the version chimaera pins), `/plugins/{pid}/update` · `rollback` · `check`, `DELETE /plugins/{pid}`, and the two reads before an install: `fetchPluginDetails` — `GET /plugins/{pid}/details` — and `previewPlugin` — `POST /plugins/preview {github}` —, both answering a `PluginDetails`: the card wire plus `release_url` and `download` (`{wasm_bytes}` or null), normalized by `normalizeDetails`), the active workspace's reactive plugin status (`workspacePlugins`, `knowledgeProviderActive`, `myceliumPlugin`; `changeWorkbenchPlugin` runs one change, `installWorkbenchPlugin` one install from a repository (a `version` too: the card's Reinstall) and `installFirstPartyPlugin` one pinned install, each re-syncing the cards and Knowledge; `checkedAt` — when this page last ran Check for updates per plugin; `expandedPlugins` — the available cards opened, by id, for this page's life only, never localStorage — with `toggleExpanded`, which fetches the details once into `pluginDetails` (keyed by `detailsKey`: id + the version shown; loading / ok / error, an error asked again on the next open)), `isMissingRoute` (a bare 404 — the daemon predates the route — vs a refusal it explains), and the attach-sheet request (`attachRequest` / `openAttachSheet` / `closeAttachSheet`) that App.svelte hosts as ONE modal for every surface. `WorkspacePlugin` carries the catalog wire: `version` (installed, or the pinned one for an available entry), `api` (`""` when null), `source` (`installed` / `available`), `installed`, `first_party`, `verified`, `requires` + `recommends`, and — null when absent — `description`, `homepage`, `requires_summary`, `recommends_summary`, `sha256_wasm`, `repo`, `pinned_version`, `local_path`, `path`, `previous`, `update`, `fault`. `normalizePlugin` defaults all of it for an older daemon (`installed` = `source !== "available"`). Trust (docs/plugin-platform-plan.md §2): every entry also carries `tier`, `caps`, `can` (the Can list: `{text, privileged}`), `standing` (`verified` / `trusted` / `untrusted`; an older daemon reads as verified), `hold` (why it can't run on this host: `{kind: blocked, level, reason}` / `{kind: policy, reason}` / `{kind: untrusted}`) and `skipped_version`; the workspace list carries `policy` (`pluginPolicy`). `json()` turns a 409 carrying `trust` into `TrustNeeded` (its `ask`, `normalizeTrustAsk`); install / update / rollback take the confirmed digest as `trust`; `trustChange` runs Trust (`{caps}`), Use anyway (`{allow_block}`), Withdraw trust and Skip this version, re-syncing like the other changes; `fetchPluginActivity`. |
| `PluginsView.svelte` | The tab shell: a fixed header — "Extensions" and the "on <host>" chip ("Plugins are installed per host"; a long name ellipsizes) on the left, the `Segmented` Plugins · Skills · Browse in the far corner (equal widths via a `:global(.seg)` grid sized by "Browse later"; Browse disabled "later"); at a tab width ≤ 720px the bar drops under the title — then one scroller per view below it, each view's lead line at the top of its own body; fetches agent-plugins / skills for the shown segment on show and on return. |
| `InstalledView.svelte` | The Plugins segment's list: **Chimaera plugins** (first-party first, then the daemon's order) as `PluginCard`s — "looking for plugins…" while the first list loads, "Nothing installed yet — pick one above." when nothing is installed — then the **Install from a repository** form (label, an `owner/repo` field, **Preview** (secondary) and **Install** (primary), both enabled once something is typed, one help sentence, the outcome line or the daemon's refusal — a Preview refusal too) and under it the previewed release as a `PluginCard` in `preview` mode (its Install installs the previewed repository; done, the preview gives way to the new card and the outcome line), the one Remove `ConfirmDialog`, and **Agent plugins**: one quiet group per agent (name + its version, cleaned of "(Claude Code)" / "codex-cli"; "claude has no plugins"; "codex isn't installed on this host"), each plugin one row in fixed columns — name + version · scope, what it brings (skills · hooks · tokens) or "2 hooks not trusted" + **Review** (the attach sheet of the chimaera plugin that asks for it), enabled / disabled. Owns the minute tick for the "checked …" lines (only while shown and after a check). |
| `PluginCard.svelte` | One card; **an available card expands to the fetched manifest**. Head: tile · name (a link to `homepage`, via `openInSystemBrowser`) · the check badge when `first_party` ("Verified by the Chimaera maintainers") · version (tooltip "chimaera pins x" when a first-party copy runs another) · "local build" tag · on the right ONE primary control — **Install** (available) or the `Switch` with its state in words ("active here" / "on · not set up here yet" / "off"; disabled while a catalog fault keeps it off) — and the **…** menu (the shared context menu, right-aligned under the button, keyboard-openable): Check for updates · Use previous version (x) · Set up in this workspace… · Open on GitHub · Remove…. Body: summary; the `description` clamped to two lines with "more" (only when it overflows); a `<dl>` — For you / For agents / Here (`hereLine`); the **Agent-side plugin** box per `agentSideBlocks` (the author's sentence, the marketplace page link, one row per agent: state words + at most one action, Install or Review); the update callout ("0.1.2 is available" · "what changed" · **Update**); the fault callout (**Reinstall** when `canReinstall`; "Switching it off and on starts it again." when the fault struck while on); one status line (in-flight words, the outcome, or the error) and "No newer version · checked just now" after a check. An available card is the head and the summary, and its whole top is one button (`.expander`, a chevron right of the summary whose `::before` covers the top out to the card's padded edge; `aria-expanded`, Enter/Space; the Install and "…" buttons, the name's link and the badge sit above the cover) that opens it in place, several at once: "loading…", then the same body through the same snippets (`facts`, `sides`, `faultCallout`) from `/details` — the description in full, For you / For agents, the Agent-side plugin box with each agent's state and no actions (they need the plugin installed) — then `installLine` and a second **Install**; a refusal is its plain sentence. `preview` mode (the repository form's Preview): that body always open, "not installed" where the switch would be, a close button where "…" would be, its own Install at the bottom. |
| `TrustDialog.svelte` | The trust prompt (docs/plugin-platform-plan.md §2): opened by whoever caught a `TrustNeeded` (a 409 carrying `trust`) — the card (install from a preview, update, Use previous, the switch, its own **Review and trust**) and the install form. Title and lead from `trustWords` (an update that asks for more is "Allow X to do more?", the running version keeps running), "It would also" (`grown`), then everything it can do ("It can"), a plain warning for a plugin that runs programs (confirmed by typing its name), **Skip this version** for an update. Confirming repeats the change with `ask.caps`; asked again (it changed meanwhile), the dialog shows the new list. |
| `platform.ts` | The 0.2 platform's client half (docs/plugin-platform-plan.md §3–§9): `normalizePlatform` (a plugin's `platform` key — `views`, `files`, `actions`, `settings`, `programs`, `tools` — malformed rows dropped; `WorkspacePlugin.platform`), `matchesPattern` (the daemon's `platform::matches`, mirrored), `claimsFor` / `actionsFor` / `viewsIn` (what active plugins draw where), `openWith` / `rememberOpenWith` (the Open with choice per workspace and file kind, localStorage, try/catch), the fetchers (views, actions, file actions, query, surfaces, `fetchDiagnostics`, the output folder and Save to workspace, settings, `fetchTools` / `installTool` / `removeTool`), `downloadWords` (the card's Downloads line), `onPlatformFrame` / `platformFrame` (the `view`, `surface`, `plugin` and `job` frames `net/events.ts` routes here), `setViewOpener` / `openPluginView` (App opens the `plugin` tab kind). Vitest: `platform.test.ts`. |
| `ui/` | Screens in the Chimaera format (`ui/1`). `UiNode.svelte`: every node, recursively (children keyed by position and type); semantic props only; unknown nodes draw their `fallback` or a quiet placeholder; rich nodes reuse the app's viewers lazily (`FileView` with `plugins={false}`, `ImageView`), `diagnostics` reads `diagnostics/1`, `diff` compares two texts or a file with a base (`head`, `index`, `rev:<ref>`, `output:<path>`). `PluginScreen.svelte`: one view — render, actions, the built-in actions (`open-file`, `open-view`, `open-url`, `copy`, `save-to-workspace` with its Replace question, `ask-agent` through `referenceNow`, `install-tool` through the tools route then a re-render), re-render on `view` frames, the width class, `fill` (a file view owns its pane: the root's last split grows to its height, a viewer alone atop a split pane fills it), "sent a screen chimaera could not draw" with What to fix. `PluginTab.svelte` (the `plugin` tab), `PluginFileGate.svelte` (inside `FileView`: a claimed file's view, Text, Open with, status chips, file actions). `screen.ts`: the context, tones, the node set, `diffLines`; Vitest `screen.test.ts`. |
| `editorMarks.ts` | A plugin's problems as editor marks: `pluginMarks(ws, file)` (lint gutter + wavy underlines in the app's tokens) fetches every active plugin's `diagnostics/1` items for the file on open and on each `diagnostics` surface frame; `toMarks` (pure, `editorMarks.test.ts`) keeps errors and warnings, drops lines past the end. `previews/CodeView.svelte` adds it in its own compartment while a 0.2 plugin is active in the file's workspace. |
| `PluginTools.svelte` | An installed plugin's **Tools** section on its card (docs/plugin-platform-plan.md §8): each tool it can download, what is installed here (version, size), and the one action that fits — **Install**, **Update** (the manifest names a newer version), **Remove**; "No build for this computer" instead of Install. Disabled while the plugin is held or faulted. The daemon does the download, checks, unpacking and setup; this shows the answer. |
| `PluginSettings.svelte` | One plugin's declared settings in core's row language (host / workspace scope, reset, checked by the daemon) and its output folder's use with **Clear** — in the card's **Settings** and in `settings/PluginsSettings.svelte`. |
| `ActivityDialog.svelte` | The card's "…" → **Activity…**: `GET /plugins/{pid}/activity`, newest first, in words (`activityWords`, `activityTime`). |
| `requirementsModel.ts` | Pure: a plugin's `requires` / `recommends` × the agents report (+ its fetch state) → per-agent rows (`state` words, `tone`, one `action`: "installed 0.7.2", "not installed" + install, "2 hooks not trusted" + review, "installed, disabled", "can't check on this daemon", "couldn't ask"), the unmet-requirement notice ("It needs claude or codex, and neither is installed on this host."), `agentSideBlocks` (the card's boxes: title, the author's or a fallback sentence, `marketplaceUrl`, rows, "asking claude and codex…"), `sheetText` (the sheet's required-vs-optional wording) and `hooksAwaitingTrust`. Vitest: `requirementsModel.test.ts`. |
| `installCopy.ts` | Pure: the card's words — the Install tooltip, the line under an opened card (`installLine`: "Installing downloads it from github.com/<repo> (≈ 305 KB) into this host's ~/.chimaera/plugins. It does nothing until you switch it on in a workspace.", the size only when the daemon gave one; `approxSize` with no-break spaces), the install / update outcome lines ("installed <name> <version>", "updated to <version>" — never a checksum), `stateWords`, `hereLine` ("using .living/ in this workspace · 4 findings · 4 decisions", "found … — switch it on to use it", "not set up in this workspace yet"), `checkedWords` ("checked 2 hours ago"), `canReinstall`, `repoUrl`, `tileLetters`; shared by the card and the sheet. Vitest: `installCopy.test.ts`. |
| `glyph.ts` · `ExtensionsGlyph.svelte` | The Extensions glyph, drawn for the pixel grid: a 12-unit box at exactly 12 CSS px (never scaled), 1 px strokes on half-unit centre lines, 5 px cells with 2 px gaps, the plus down column 9 / along row 2 of its cell. The component is the only way it is drawn; its caller places the box on whole pixels (PaneTabs nudges it 1 px down in the 25 px tab). |
| `SkillsView.svelte` | Every skill each agent can use here: the controls in Settings' recipes (`Segmented` All · claude · codex, a search field, the counts), groups in plain words — This project · From plugins (a section per plugin) · Yours · Built into claude · Built into codex (copyable `/name` and `$name` chips; "claude lists its built-in skills only while a claude chat runs." where claude's would be, only when no claude chat runs) — per-agent badges, and a row opened in place as a small list: Use it (each agent's invocation, copyable) · File · Problems. Its blocks sit straight in PluginsView's column, which spaces them. |
| `skillsModel.ts` | Pure grouping (built-ins split per agent by who lists them) / counting / filtering + `invokeSyntax`. Vitest: `skillsModel.test.ts`. |
| `AttachSheet.svelte` | "Use Mycelium for Knowledge" (opened from the card's "Set up in this workspace…", a hook row's Review, Knowledge's card, the Mastermind panel): (when the plugin is only available) install it first — the card's Install flow · the agent plugins it requires or recommends, for the agents installed here (the author's `requires_summary` / `recommends_summary` first, then the model's rows; a recommendation reads "optional: …"; Install → a visible terminal, opened as a session) · trust codex's hooks (each in plain words; the Stop hook's caveat; exactly the `{key, hash}` pairs shown are posted; hooks of required and recommended agent plugins alike) · set up this workspace (agent select → the plugin's own setup prompt in a new chat session, "billed to your <agent> account"). Completing also switches the plugin on here. Re-checks on every open, after the install, when the daemon skipped a hook (it changed after it was shown — nothing past trust runs then) and after a failed step. App.svelte keys it by workspace + plugin (a switch underneath starts it over) and bounds it with its own `<svelte:boundary>`. |

## Invariants / gotchas

- **The header is fixed across views.** Nothing in it depends on the shown
  view (a view's lead line lives in its body; the stacked layout is chosen
  by the tab's width alone), so a switch never moves or resizes it. Each view
  has its own scroller, both always in the DOM and the hidden one parked with
  `opacity: 0` + `inert` (Pane.svelte's layer idiom; Skills mounts on its
  first show), so a view keeps its scroll position — and Skills its filter
  and search — across switches. The header bar and the scrollers share
  `scrollbar-gutter: stable`, so a classic scrollbar showing in one view only
  can't shift the columns. Measure a change with the bounding boxes of
  `.head` and `.seg` in each view.
- **Plain words on the card.** Section headings carry their meaning without
  hints; no hashes, `SHA256SUMS`, "sandboxed", "manifest", "catalog" or file
  names in visible text (a tooltip on a button may explain what it does).
  One primary control per card state (Install, the switch, Update in its
  callout, Reinstall in the fault's); everything secondary lives in the "…"
  menu — no rows of bare links. The author's own words (`description`, the
  agent-side `summary`) are shown as given, as text, never as HTML.
- **An agent reports one row per installation.** The same plugin id can come
  back at user and project scope: rows are keyed by index + id, and
  `requirementsModel.ts` lets an enabled copy answer for the plugin.
- **Agent state comes from the agents** (the daemon's probes of `claude
  plugin list` / codex `app-server`), never re-derived here. A 404 from a
  route the daemon doesn't have yet renders an honest line, not a spinner.
  `agent_plugins` events (install completion, hook trust, reconnect) advance
  `store.ts`'s local revision. The visible Plugins/Skills segment refetches;
  hidden panes/documents catch up on return. Once the report confirms the
  requested plugin by agent + plugin id (including an installed-but-disabled copy),
  the card's continuation reads "Installed for <agent>";
  hook trust and workspace setup still require the user's action.
  Both card and sheet send that row's `agent_plugin_id` to the install route and
  retain it in the continuation; another add-on for that agent cannot complete it.
  Qualified reported ids take precedence over short names. A short name shared
  by different marketplace ids for the same agent is ambiguous: neither installed
  presence nor hook ownership can be inferred from it. Keep that row unknown.
  An unqualified manifest id does not turn a short report into exact evidence.
  The attach sheet repeats the ambiguity warning and refuses completion until
  resolved; setup choices require known enabled add-ons and all requirements
  for that agent. Use `agentsForSetup` for both the chooser and its default.
  When setup is needed, an empty eligible list blocks the primary action and
  completion before any trust/enable writes; it must never silently skip setup.
- **Never list an agent that isn't installed on this host.** A
  recommendation for it says nothing; a requirement none of the listed
  agents here can meet says so once. `requirementsModel.ts` owns this rule —
  change it there, with its test.
- **What a plugin adds is on every card before its Install is clicked**
  ("For agents" / "For you") — it is what makes opt-in honest. An installed
  card shows it always; an available one when opened, from its release's
  manifest (`/details`), and a repository through Preview (`/preview`). The
  opened body is the installed card's own snippets, never a second
  implementation; agent-side actions stay off until the plugin is installed
  (the daemon's install route needs its manifest).
- **External links** (the name → `homepage`, "what changed", the agent
  plugin's page, Open on GitHub) go through `openInSystemBrowser` and render
  only for http(s) URLs (`isWebUrl` / `marketplaceUrl`), with
  `rel="noopener noreferrer"`.
- **Keyboard:** every control is a real button or link with the global
  focus ring; the "…" menu opens on Enter, walks with the arrows, and the
  shared context menu hands focus back to the "…" button on a pick or
  Escape (any `aria-haspopup="menu"` opener gets that).
- **Hook trust is never automated.** The sheet lists what codex will run and
  posts only the hashes the user saw; the daemon writes only those whose
  current hash still matches.
- **Installs and setups of agent plugins are the CLIs' own commands** in a
  visible session; the sheet closes and opens that session. The install
  terminal keeps its result until Enter. `store.ts::agentInstallContinuation`
  remembers the latest install's workspace/plugin/agent so its card offers
  **continue setup** even when the install began in the sheet. Returns to
  Extensions with this continuation, explicit refreshes, and sheet opens
  bypass the agent-probe cache. A completed sheet or launched setup clears
  its matching continuation; an install response alone never means success.
- **Canonical vocabulary**: `/name` for claude, `$name` for codex; plugin ids
  as the agents report them.
- **Versions, sources and verification come from the daemon** (the lock, the
  installed copies, the SHA256SUMS checks); the card never compares versions
  itself beyond showing `pinned_version` in a tooltip (the "checked …" line is
  the only client-side time: when THIS page last asked, `checkedAt`). The check badge is
  `first_party` — approved by the Chimaera maintainers (the curated lock) —
  and nothing else; the integrity check (`verified`) stays silent on the card
  and only its failure shows, as the daemon's `fault` in the fault callout
  (with Reinstall). An available entry
  installs the version chimaera pins — never "the latest". The daemon never
  downloads on its own: Install, Update, Use previous and Remove are the
  user's clicks, each re-syncing the cards. The add row passes what was
  typed through (trimmed); the daemon normalizes the URL form and refuses
  bad input, so the UI never re-validates `owner/repo`.
