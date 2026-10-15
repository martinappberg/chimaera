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
- **How it's used.** The Settings pane (a singleton tab in a workspace; a full page over Home, see
  [native-app.md › Home launcher](native-app.md#home-launcher)) edits keys through typed rows; a raw-JSON
  editor (`SettingsJson.svelte`) is available for anything the schema doesn't surface. `GET
  /api/v1/settings` returns the map; `PUT /api/v1/settings` replaces it whole (≤256 KB, 204). Changes
  broadcast on `/ws/events` so every window converges.
- **Where it lives.** `settings.rs` (`get_settings`/`put_settings`, `SettingsStore`); UI schema in
  `web-ui/src/lib/settings/schema.ts` (where defaults live).
- **Key behaviors.** Reads **re-stat the file** so external edits surface without a restart, bumping a
  content generation that `/ws/events` diffs against. Daemon-consumed keys include
  `git.path`, `agents.*.path`, `agents.communication.*`, `notifications.*`,
  `daemon.scrollbackLines`, `daemon.restoreSessions`, `chat.remoteControlAtStart`,
  `chat.toolSummaries`, `chat.codexSteering`, `chat.resumeAfterRestart`, `update.autoCheck` and
  `quickOpen.ignoreDirs`; unknown keys are opaque and preserved verbatim (forward-compat — a newer
  UI's keys survive an older daemon). A corrupt/oversized/non-object file degrades to an empty map with
  a warning — settings must never brick the daemon. A changed `agents.*.path` triggers shim regeneration
  + a detection-cache drop.

## Settings surface and typography domains

- **The schema is the contract.** Every typed row, default, range, category, JSON completion, and
  validation message derives from `schema.ts`; default values remain sparse (choosing a default
  deletes that key). The UI covers Appearance, Agents, Environment, Dashboard, Chat, Terminal,
  Editor, Files, Quick Open, Git, Extensions, Notifications, Daemon, Updates, and Keyboard.
  Bespoke sections use their own stores: Environment's multiline preludes use
  `/api/v1/environment` and `env-profiles.json`; Plugins exposes each plugin's declared
  settings; Activity reads session records. Documents offers opt-in writes of the documents dialect into the workspace's `AGENTS.md` or a
  Claude Code skill over `/api/v1/agent-docs`
  ([agents.md](agents.md#documents-the-portable-dialect-check_document-and-the-issues-chip)).
- **Interface typography applies app-wide.** `appearance.interfaceFontSize` (13.5 px by default;
  11.5/12.5/13.5/15.5 px for xs/sm/md/lg)
  rebuilds the shared `--text-xs`/`--text-sm`/`--text-md`/`--text-lg` scale live, so the rail,
  file tree, pane tabs, dashboard, settings, dialogs, Git, and preview chrome move together.
  `appearance.interfaceFontFamily` supplies the shared UI font stack. Small chrome uses those
  tokens rather than fixed rem/px sizes.
- **Content surfaces stay independently legible.** Chat defaults to a 14 px base at line height
  1.57 and overrides the shared scale within each `ChatView` (panes, the subagent read-only view,
  and the dashboard's embedded Mastermind) and exposes font size/family and line height. Terminal
  keeps its xterm-specific font controls. Editor typography
  covers code, diffs, and the JSON editor; rendered Markdown defaults to the same 14 px as Chat, so
  an agent's reply and the document it wrote read alike,
  but has its own font-size and line-height settings rather than borrowing `terminal.fontSize`.
  CodeMirror views reconfigure while mounted.
- **Reading Width is the column width.** `chat.contentWidth` (Appearance › Reading Width, 680 px by
  default, the Claude desktop app's column) sets the transcript column, its composer and trays, and the
  rendered Markdown column (plus its padding) on every chat surface; a narrower pane still fits its
  space. It used to cap a 48em column, so any value above 48em did nothing.
- **Newer surfaces are represented.** `dashboard.landing` controls the workspace landing and
  `dashboard.cardDensity` selects automatic, comfortable, or compact agent cards. Mastermind's
  agent/mode are workspace state edited in its setup card, while Environment preludes remain scoped
  records rather than being flattened into global preferences.

## Updates status

- **Updates section** (`UpdatesStatus.svelte`, above the `update.autoCheck` switch) — the one place that
  answers "is there an update?": a line each for the app (native), the daemon serving this window
  ("chimaera" in a browser, "local daemon" / "daemon on \<host\>" in the app) and the agent CLIs (one
  summary line, "Agents ↓" jumps there). Each says up to date / \<new\> available / couldn't check (with
  the reason) / not checked yet, plus when it last checked and the cadence; "check now" asks every source
  at once. Agents read "up to date" only when every installed agent has comparable versions and a successful latest check. Failed re-checks preserve the known release while displaying the failure; unknown versions never count as current. The Agents and Updates sections share the catalog, so installation and path changes refresh both.

## Activity

- **Activity section** (`ActivitySettings.svelte`, store-backed like Environment and Documents —
  no schema rows) — sessions, tokens and time from the session records across every workspace
  (no dollars), with a CSV export; see [session-history.md](session-history.md#activity).

## Agents settings & themes

- **Agents panel** (`AgentsSettings.svelte`) — one card per built-in agent (Claude Code, Codex,
  Antigravity, Grok Build, Cursor CLI) shows version, chat/terminal readiness,
  and whether Chimaera manages the installation. Install, Set up chat, or Update is offered when
  applicable; personal installations link to official update instructions. An installed agent whose
  CLI reports no login reads "Installed · Not signed in" with a **Sign in** that opens the sign-in
  card under the row (the same card a chat shows; [agents.md › Sign in](agents.md#sign-in)) — the
  card follows the login and the row refreshes when the CLI confirms it. Antigravity's chat setup
  installs Google's companion. Advanced holds custom executable paths, Reinstall and Uninstall.
  Installation progress follows a retained daemon result from the terminal's actual exit status, with bounded polling only while
  Settings is visible. Re-checking and saving another row preserve unfinished path edits.
  The agent menu's **agent settings →** opens this panel; it is where every agent is set up and
  managed. Cursor's custom executable is `agents.cursor.path`; a dated Cursor
  build reads as its date and is "Up to date" only when the installed build is the latest one.
  See [agents.md](agents.md).
- **Theme palettes** (`themes.ts`) — each theme carries its own hand-tuned 16-color terminal ANSI
  palette alongside the UI tokens; a UI theme without a terminal palette is "half a theme". UI quality
  (curated light/dark) is an acceptance criterion, per [rules/web-ui.md](../../.claude/rules/web-ui.md).
  The set: the two Chimaera defaults, then GitHub Light, One Light, Catppuccin Latte, Rosé Pine Dawn,
  Terracotta Light and Graphite Light; Tokyo Night, One Dark Pro, Catppuccin Mocha, Terracotta Dark
  and Graphite Dark. Terracotta (ivory and charcoal, a clay accent) and Graphite (true neutrals, a
  soft green accent) are Chimaera's own pairs; the rest start from their canonical palettes with
  surfaces stepped for the rail/pane layering. A stored id the registry no longer knows falls back to
  the mode's Chimaera default.
- **Legibility contract** (`themes.contrast.test.ts`) — every color a theme uses as text holds
  WCAG 4.5:1 on its pane sheet (comments 3.8, muted also 4:1 on a selected row, terminal ANSI 4.5 /
  bright 3.5); a canonical tone that misses is moved by OKLab lightness only, hue and chroma kept,
  so the theme still reads as itself. The accent is a dot-and-button color and may sit under the
  floor; where it renders as text (markdown links, tool-card paths) it goes through
  `--accent-text` (`app.css`: the accent a quarter toward `--fg`), which passes on every theme.
  Secondary chat lines (thoughts, tool rows, finished work) use `--muted` at full strength — no
  opacity tier below it — because faded-muted text was the one tier that failed everywhere.

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

### Why the theme set and the legibility floors
_Captured 2026-10-15 from the maintainer's direction in the session that shipped them._

- **Readability first, identity kept.** The complaint was that chat text sometimes read worse than
  the Claude desktop app. Measurement showed the base colors were fine and the faded secondary tier
  was the problem; the fix is to stop fading, not to crank contrast. The two Chimaera defaults are
  the product's look — "it should still be the dark and the white" — and a warmed variant was tried
  and rejected as "too prominent, almost green". A darkened accent read as "cold, less awake", so
  the vivid accent stays and only its text use is mixed toward the foreground.
- **Themes are "just themes".** The extra palettes exist to be nicer, readable and modern, not to
  carry structure; swapping dated editor palettes for current ones was meant to be a small change.
  Rosé Pine Dawn stays as the one playful light theme. Terracotta and Graphite answer a wish for a
  warm-paper look and a true-neutral look under Chimaera's own names, never another product's.
- **Do not change:** the floors (they are the test), the default neutrals, and the rule that a
  palette moves by lightness only.
