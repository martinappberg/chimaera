# Timeline, Knowledge & Plugins — design & plan

Status: **built** on `claude/mastermind-cross-agent-comms-19a84e`
(2026-09-25): A1–A3 and B1–B4 of §11, verified live against an isolated
daemon and a mycelium 0.7.2 fixture. **B5 (Browse / marketplace) is later**,
and the §6 contribution points `views` / `settings` / `commands` are
specified but not built (see [plugins.md](agent-guides/plugins.md)). The
§12 questions took their recommended defaults. It grows out of a maintainer conversation about three
frustrations — the Mastermind "often isn't doing anything", the dashboard
"doesn't say much and feels redundant", and the wish for agents (claude *and*
codex, even subagents) to talk to each other and for the workspace to *know*
things — plus a verified read of mycelium 0.7.2 and three follow-ups: a quick
way to attach mycelium, a plugin model generic enough for things like a LaTeX
plugin another agent will build later, and a view of every skill the agents
can use (a marketplace later). It builds on
[agent-dashboard-plan.md](agent-dashboard-plan.md) (shipped through v1) and
the unmerged Loadout plan (`docs/skills-manager-plan.md` on branch
`claude/chimaera-skills-manager-4c4110`), which it absorbs. Items marked
**[decide]** are the maintainer's call.

## As built — where it differs from the design below

- Plugins switch **per workspace only** (`Workspace.plugins_on`, off by
  default); there is no global `plugins.enabled` setting.
- **session** crash entries come from chat exits only. Hook-less TUIs
  (codex/gemini terminals) write no episodes.
- The interactive-program exclusions are a fixed list in `episodes.rs`, not
  a setting.
- Codex's installed plugins are inferred from the `pluginId` on its skills
  and hooks (PROTOCOL.md Pass 32), not `plugin/installed`; a codex plugin
  with neither doesn't appear.
- Claude memory is read from `~/.claude/projects/<encoded cwd>/memory`.
  Without mycelium there is no "latest episodes" fallback for *Where we
  left off*.
- The Plugins tab opens from quick-open; the dashboard, dock and Knowledge
  open the attach sheet directly.
- The Mastermind reads knowledge through the mycelium plugin's
  `knowledge_search` / `knowledge_get` (offered whenever it is active), not
  a separate `knowledge` tool.
- Knowledge *recorded by* is read back from the Timeline's episode
  evidence (it survives a restart). Agent-notes read cursors live in
  memory, so a restart can show old notes as unread again.
- Codex TUIs get the chimaera MCP (and so the plugin tools) only while a
  plugin with tools is active in the workspace.

## Decisions (maintainer, 2026-09-25)

1. **The dashboard leads with questions**, not the roster: the roster demotes
   to a one-line **Now** by default (expandable to today's cards).
2. **One Plugins tab.** Loadout's per-agent lens becomes part of it rather
   than a second tab.
3. **No "keep" — Chimaera never curates knowledge.** Knowledge is written by
   agents as they work, the way mycelium does it (structured templates,
   Stop-hook enforcement, evidence-derived confidence); Chimaera reads it,
   shows it, and links it to the Timeline. Quality comes from structure and
   evidence, not a human gate.
4. **Names:** *Timeline*, *Knowledge*, *Agent notes* ("Board" is taken by
   the figure composer).
5. **A Skills view** — every skill available to the agents, per agent — ships
   with the Plugins tab; a **Browse** (marketplace) view comes later.

## 0. The one-paragraph version

The dashboard and the Mastermind both show **state** — the rail and the panes
already do that. What nothing shows is **history** (what happened while I was
away) and **knowledge** (what this project has learned). So: one new core
primitive, a per-workspace **Timeline** the daemon writes from signals it
already has (an agent finished a piece of work, a long command failed, a
Slurm job ended, an agent recorded a finding) — no LLM, no polling added. The
dashboard is re-centred on *what needs me, what happened, where do things
stand*; the Mastermind gets memory (the Timeline), senses (Slurm, env,
terminals, windows) and a one-click **Brief me**. **Knowledge** is a
read-only view with a fixed, plain-words shape (where we left off · found ·
decided · watch out for · open) over what agents record — through
**mycelium** when attached, plus the agents' own memory and guidance files.
Anything that changes what agents see, or adds a workbench capability, is a
**plugin** — off by default, stating in words exactly what it adds. One
**Plugins** tab holds both kinds — **workbench plugins** (run in Chimaera:
mycelium, LaTeX, agent notes) and **agent plugins** (run inside claude/codex,
managed through their own plugin managers) — plus a **Skills** view of every
skill each agent can use here, and later a **Browse** view over the
marketplaces the agents already have. Agents "talking" is itself a workbench
plugin: notes on the Timeline that never start a turn.

## 1. Why it feels redundant (the diagnosis)

- **The dashboard repeats the rail.** Roster cards say who exists and what
  state they're in; the rail says the same. Changed files repeat the git
  panel; recents repeat the rail. The only piece nothing else has is the
  attention lane — and it's the piece that works.
- **The Mastermind is amnesiac.** Every answer is re-derived from a thin
  snapshot (names, states, now-lines). Asked "what's going on", it returns a
  text version of the cards you're looking at.
- **Half blind.** `workspace_status` has no Slurm, no environment, no
  command history outside linked terminals, no idea which windows are open.
- **Deaf.** Workers can't tell it anything (`ask_mastermind` is parked for
  lack of a non-triggering inbox — decision 9 in
  [agent-dashboard-plan.md](agent-dashboard-plan.md)).
- **Idle by design** — reactive-only is right (no unprompted billed turns),
  so the fix is *better answers when asked*, not autonomy.

## 2. The vocabulary (the whole model in three words)

A user learns exactly this:

- **Timeline** — *what happened*. Written by Chimaera, automatically.
- **Knowledge** — *what we know*. Written by the agents as they work.
- **Plugin** — an opt-in add-on that says what it adds.

Nobody curates. The user reads, corrects (every entry opens in its file —
it's markdown in their repo), and, when something matters, tells an agent to
record it — which is how mycelium already works. Everything else (episodes,
providers, contribution points) is internal vocabulary and never appears in
the UI; mycelium's own terms appear only on its plugin card and as a source
label.

## 3. The dashboard — re-centred on questions

```
 chimaera · main ↑2 · 2 working · 1 needs you · 3 jobs (next ends 14:20)
 ┌ NEEDS YOU ───────────────────────────────────────────────────────────┐
 │ codex-2 wants to run `rm -rf results/tmp`       [Allow] [Deny] [open] │
 └──────────────────────────────────────────────────────────────────────┘
 SINCE YOU LEFT · 3h                                       open timeline →
 ● claude-1  "fix the QC filtering for low-count cells"          38 min
   → Filtered at 200 UMIs; 3 samples fell below.  7 files
   ✎ recorded 1 finding (F-004, preliminary) · 2 learnings
 ✕ pipeline  snakemake exited 1 after 2h13m                        [open]
 ✓ job 81234 align_all — COMPLETED in 3h02m
 ◉ F-003 "batch effect in sample 3 is technical" is now supported
 WHERE THINGS STAND                                       open knowledge →
 ◉◉○ Batch effect in sample 3 is technical, not biological   supported
 ◉○○ Filtering at 200 UMIs keeps rare populations          preliminary
 Next: re-run DE without sample 3 · check doublet rate
 NOW   claude-1 editing loader.py · codex-2 waiting · 2 terminals
```

1. **Vital-signs strip** — unchanged idea, one line.
2. **Needs you** — the attention lane, unchanged. Quiet means quiet.
3. **Since you left** — the Timeline since this browser last looked
   (per-viewer, like unread today; no daemon state), grouped by session,
   newest first, ≤8 rows, "open timeline" for the rest.
4. **Where things stand** — the top of Knowledge: strongest recent findings,
   the latest decision, what's next. With no structured provider it's one
   quiet line: "your agents can record findings and decisions with mycelium
   → attach".
5. **Now** — the roster as one line (decision 1), expandable to today's
   cards. The rail already lists sessions; the dashboard stops competing.

Changed files and git fold into episode evidence and the strip's branch
chip; recents stay in the blank state. The Mastermind dock stays where it is.

## 4. The Timeline (core, daemon)

**What writes it** — only signals the daemon already receives:

| Entry | Source | Fidelity |
|---|---|---|
| **episode** — an agent's piece of work | chat journal (prompt, final text, tools, files); claude-TUI hooks (UserPromptSubmit prompt, Stop `last_assistant_message` — verify on the pinned CLI, `files_touched`); output-only TUIs: "active 14:02–14:40" | protocol › hooks › output-only, worn like the dashboard's tiers |
| **command** — a terminal command that failed after ≥10 s or ran ≥2 min | OSC 133 marks (`chimaera-pty/src/marks.rs` already keeps exit codes) | exact |
| **job** — a Slurm job ended (state, elapsed) | the compute service's existing squeue/sacct reads | stamped when the daemon notices |
| **session** — crashed / exited unexpectedly | PTY + chat exit paths | exact |
| **knowledge** — entries an agent recorded; a finding's status moved | the Knowledge providers' files, re-checked at each episode end (a few `stat`s; re-parse only on mtime change), diffed by fingerprint | attributed to the ending episode's session when it's the only agent that ran since the last check; otherwise "recorded during …" or unattributed — never guessed |
| **note** — (Agent notes plugin) posted by an agent, the Mastermind, or a person | MCP / UI | authored |

**Entry anatomy — ask → did → result.** An episode's headline is the
*user's own prompt* (the best description of intent there is, and free),
its result is the first meaningful sentence of the final message (filler
like "Done! Here's what I did:" skipped; nothing good → no result line, just
evidence), and its evidence is files · duration · turns · end state · what
it recorded in Knowledge. The session keeps its existing name (claude's own
`aiTitle`, per `naming.rs`). Consecutive episodes in one session within
~10 min merge ("+2 follow-ups").

**Importance, not completeness.** Failures always show; long things finishing
show; trivial things never do (a 3-second successful `ls` is not history).
Interactive programs (`ssh`, `vim`, `less`, `top`, agent CLIs, bare REPLs)
are excluded from command entries — a small, settings-visible list.

**Storage** — the chat-journal discipline: `~/.chimaera/workspace/<ws>/
timeline.jsonl`, append-only, gap-free `seq`, size-capped (≈2 MiB, oldest
system entries dropped first), in-RAM tail ring, inside the shared state
budget. Writes happen at episode, command and job ends — a handful per hour.
Slurm: no new standing poll unless this daemon tracks live jobs launched
from the workspace, then ≥60 s (scheduler-polite).

```json
{"v":1,"seq":812,"ts":1758841200000,"kind":"episode","sid":"a-3f2",
 "agent":"claude","ui":"chat","tier":"protocol",
 "title":"fix the QC filtering for low-count cells",
 "result":"Filtered at 200 UMIs; 3 samples fell below.",
 "evidence":{"files":["qc.R","filter.py"],"files_n":7,"ms":2280000,"turns":4,
             "recorded":{"findings":["F-004"],"learnings":2}},
 "end":"finished"}
```

Wire: `GET /api/v1/workspaces/{id}/timeline?since=&limit=` + an epoch bump on
`/ws/events` (the git/recents idiom) — additive, versioned envelope.

## 5. Knowledge (core view, read-only providers)

**Agents write, Chimaera reads** (decision 3). Chimaera has no knowledge
store and never writes knowledge files. Providers:

- **mycelium** (the plugin, §6.3) — the structured provider: `.living/`
  findings, decisions, learnings, todos, and the `.mycelium/last-session.md`
  handoff. Written by agents through mycelium's skills and hooks.
- **Guidance & memory** (always) — AGENTS.md / CLAUDE.md tree / MYCELIUM.md,
  and claude's project memory (the directory claude reports as
  `memory_paths` in its session init; its files carry `description`
  frontmatter, so they list cleanly). Codex memories — verify what codex
  exposes before promising.

**One shape.** Sections are named for the question they answer:

| Section | From mycelium | Without a structured provider |
|---|---|---|
| **Where we left off** | handoff → *Current state* + *Next steps* | the latest Timeline episodes |
| **What we found** | `.living/findings/*.md` (`## F-NNN`, status) | — |
| **What we decided** | `.living/decisions.md` | — |
| **Watch out for** | `.living/learnings.md` | — |
| **Open** | `todo/TODO_REGISTRY.md` + findings' *Open Questions* | — |
| **Guidance & memory** | + MYCELIUM.md | AGENTS.md, CLAUDE.md, claude memory |

Each item is one line + date + who recorded it (when the Timeline knows) +
source label, expanding to its body; "open in file" jumps the existing
editor to the heading — the user's correction path. Findings wear mycelium's
**confidence ladder** — ◉○○ preliminary · ◉◉○ supported · ◉◉◉ robust ·
✕ contradicted — derived by mycelium from the evidence ledger, never by us.
Empty sections don't render; without mycelium the view is Guidance & memory
plus one card: *"Want findings, decisions and learnings here? Your agents
can record them with mycelium → Attach."* One search box, client-side.

**How knowledge gets in** — the mycelium way, nothing new: agents record as
they work (mycelium's Stop hook nudges when they forget); the user who wants
something recorded says so to an agent ("record that as a finding"); the
Mastermind, if it has mycelium, records like any agent when asked.

## 6. Plugins — two kinds, one tab

**Different in who runs them, managed the same way.**

- **Workbench plugins** run in Chimaera: mycelium's knowledge provider,
  LaTeX build + preview, agent notes. Off by default.
- **Agent plugins** run inside an agent CLI: claude plugins, codex plugins,
  skill packs, MCP servers. Chimaera never owns or reimplements them — it
  reads what each agent reports and installs/updates through the CLI's own
  manager in a visible terminal.

A workbench plugin may **require** agent plugins (mycelium) or **reach
agents itself** through Chimaera's MCP server (LaTeX's `latex_build`) —
cross-vendor by construction: claude, codex and anything MCP-capable get the
same tool without a per-agent install.

**The tab** — a singleton `{v:"plugins"}` (additive layout kind), reachable
from the dashboard, settings and quick-open, naming the host ("on
sherlock") because installs are per host. Three views:

- **Installed** — workbench plugins, then each agent's plugins.
- **Skills** — every skill each agent can use here (§6.4).
- **Browse** — later: what the agents' marketplaces offer (§6.5).

```
 Plugins                    Installed · Skills · Browse     on sherlock
 WORKBENCH
 Mycelium   Project memory your agents record in .living/            [on]
   Here: active — 4 findings · 12 learnings · 3 decisions
   Adds: fills Knowledge · 2 read tools for every agent here
   Needs: claude ✓ mycelium 0.7.2 · codex ⚠ hooks not trusted (codex → /hooks)
 LaTeX      Build .tex to PDF and preview it beside the source      [off]
   Would add: build button on .tex · PDF beside source · 1 agent tool
 Agent notes  Agents leave notes for each other on the Timeline      [off]
   Would add: 2 tools to every agent here · never starts a turn
 CLAUDE 2.1.259
 mycelium 0.7.2  user   10 skills · 3 hooks · ~1.1k tokens every session
 CODEX 0.153.0
 no plugins
```

**"Adds" / "Would add" is mandatory** — every card says in words what
changes in the UI and for agents, including context cost where the agent
reports it (`claude plugin details` gives the component inventory and
always-on tokens). That line is what makes opt-in honest and the page easy
to read.

**Reading agent state — always from the agent, never re-derived:**

| | Installed plugins | Skills | Marketplace catalog |
|---|---|---|---|
| claude | `claude plugin list --json` (id, version, scope, enabled, installPath) + `claude plugin details <id>` | session init's `skills`/`plugins` (live) + scan of `.claude/skills`, `~/.claude/skills`, enabled plugins' skill dirs | `claude plugin list --json --available` (291 entries today, with descriptions) |
| codex | app-server `plugin/installed` | app-server `skills/list {cwds}` — scope, enabled, owning plugin, path, load errors (already adopted, PROTOCOL.md Pass 26) | app-server `plugin/list` + `marketplace/*` |

The codex reads need an app-server connection: reuse a live codex chat
session's (its handshake already calls `skills/list`), else a short-lived
on-demand one — single-flight, cached, never polled, no model call.

### 6.1 The workbench plugin model — data first, fixed contribution points

A plugin is a small TOML manifest plus, for first-party plugins, code behind
*named* capabilities. The manifest never contains code.

```toml
id = "mycelium"
name = "Mycelium"
summary = "Project memory your agents record in .living/"
homepage = "https://github.com/arjunrajlaboratory/mycelium"

[detect]                        # cheap file checks → "active here"
any = [".living/INDEX.md", "MYCELIUM.md"]

[requires.agent_plugins]        # delivered by each agent's own manager
claude = "mycelium@mycelium"
codex  = "mycelium@mycelium"    # + codex /hooks trust — shown, never automated

[setup]                         # the plugin's own documented step
prompt = "Set up mycelium"      # sent to an agent session the user picks

[provides]
knowledge = "mycelium"          # built-in reader (read-only)
mcp_tools = ["knowledge_search", "knowledge_get"]
```

```toml
id = "latex"
name = "LaTeX"
summary = "Build .tex to PDF and preview it beside the source"

[detect]
any = ["**/*.tex"]              # answered from the existing file index, never a new walk

[settings]
engine = { choices = ["pdflatex", "xelatex", "lualatex"], default = "pdflatex" }
build_on_save = { default = false }

[commands.build]                # runs as an ordinary terminal session
run = "latexmk -{engine} -interaction=nonstopmode -synctex=1 {file}"
output = "{dir}/{stem}.pdf"     # opened with the existing PdfView

[provides]
views = ["latex"]               # first-party Svelte module, lazy-loaded
mcp_tools = ["latex_build"]     # returns parsed errors to any agent
```

**The contribution points — only what mycelium + LaTeX + agent notes
need** (rule of two; add a point when a second plugin needs it):

| Point | Declarative (any manifest) | Named built-in (first-party code) |
|---|---|---|
| `detect` | file/glob checks, answered from existing indexes, mtime-cached | — |
| `requires.agent_plugins` | ids per agent CLI; status + install via the Installed view | — |
| `setup` | a prompt (user-clicked agent turn) or a command (visible terminal) | — |
| `settings` | typed per-workspace values, rendered generically | — |
| `commands` | shell templates run as **ordinary terminal sessions** — env prelude applied, visible in the rail, exit codes flow into the Timeline for free | — |
| `provides.knowledge` | — | a read-only reader (`mycelium`) |
| `provides.mcp_tools` | — | tools served by the chimaera MCP, only in workspaces where the plugin is active |
| `provides.views` | — | a Svelte module in `web-ui/src/lib/plugins/<id>/`, dynamic-imported so a disabled plugin costs no bundle parse |

**Lifecycle & state.** First-party plugins ship inside the binary (manifests
rust-embedded, like the UI) and version with it. Enabled set: one daemon
setting. Per workspace: *active* = enabled ∧ `detect` matches (no attach
state to drift); a per-workspace "off here" override is an additive field on
the workspace record; per-workspace settings likewise. A project's own
config wins over plugin settings (LaTeX honours an existing `latexmkrc` —
fill the gap, never fight a choice).

**Daemon seam.** `crates/chimaera-server/src/plugins/` — manifest parsing,
the registry, detect cache, routes (`GET /api/v1/plugins`, `GET/PUT
/workspaces/{id}/plugins`); named capabilities resolve by `match` to modules
(`plugins/mycelium.rs`, later `plugins/latex.rs`). No dynamic loading, no
plugin trait zoo. **UI seam.** `web-ui/src/lib/plugins/registry.ts`: id →
lazy import. A new first-party plugin = a manifest + a daemon module + an
optional UI module + a feature page — the whole recipe for the LaTeX agent
lands as `docs/agent-guides/plugins.md` with the skeleton.

**Third-party plugins — later, declarative-only.** A pasted manifest may use
the declarative column only (detect, requires, setup, settings, commands)
plus names of built-in readers/views, installed through Loadout's staging +
full-manifest review; commands shown verbatim before their first run, never
auto-run. **Never third-party code in the daemon or the UI** — the HPC
footprint and the auth-on-every-route model don't survive an extension host.

**Not Chimaera-specific where it matters.** Agent-facing pieces ride open
standards (MCP, Agent Skills, the CLIs' own plugin managers) and keep
working outside Chimaera; readers consume other tools' formats (mycelium's),
not a Chimaera format; Chimaera writes no knowledge format of its own.

### 6.2 Attaching mycelium — the quick path

From the Knowledge card, the Mastermind dock, or the plugin card: **Use
mycelium for Knowledge →** one sheet, three live-checked steps:

1. **Installed for your agents** — claude ✓ / codex ✗ [install] (a visible
   terminal running the CLI's documented commands), then codex's hook trust
   right in the sheet (§6.6) — no trip to `/hooks`.
2. **Set up in this workspace** — [Set up] sends *"Set up mycelium"* to a
   new agent session of the user's choosing (their click, their billing);
   mycelium's own init writes `.living/`, `MYCELIUM.md`, the adapters.
3. **Done** — `detect` flips, Knowledge reads `.living/`, every agent here
   gets the read tools.

The Mastermind is just an agent session, so with the plugin installed it
also gets mycelium's skills natively. **Verify before recommending:**
mycelium's Stop check blocks when "files changed but `.living/` wasn't
touched" — if its change accounting is repo-wide, a Mastermind (which never
edits) could be blocked by *workers'* edits. Mitigations exist on the claude
side (`--setting-sources` to skip the repo-local hooks file, or
`--restricted`, which also enforces "delegates, never does" — both flags
exist on 2.1.259; behaviour unverified).

### 6.3 Mycelium 0.7.2 — ground truth for the reader (verified 2026-09-25)

- Cross-host now: Claude plugin + Codex plugin (`.codex-plugin/`), canonical
  `MYCELIUM.md` with thin `CLAUDE.md`/`AGENTS.md` adapters, gitignored
  `.mycelium/` runtime state (`run/<claude|codex>/<session-id>/`, locks,
  `last-session.md` handoff with five fixed sections).
- `.living/` layout unchanged. Learnings/decisions: `### [YYYY-MM-DD] Title`
  at column 1 + `**Field**:` lines; 0.7.0's validator makes other heading
  levels an error and a migration repairs them. Their parser is not
  fence-aware — ours will be.
- **Ids:** only findings (`F-NNN`) are stored; `L-N`/`D-N` are positional
  and renumber on insert. Chimaera references entries by fingerprint
  (kind + date + title hash), never by `L-N`.
- Findings: `## F-NNN: claim`, `**Status:**` preliminary|supported|robust|
  contradicted (derived from the evidence ledger), `### Evidence Ledger`
  table, `### Open Questions`.
- Todos: `todo/TODO_REGISTRY.md` (Item|Priority|Status|Category|Date|Author|
  File).
- No JSON export, no MCP, no messaging, no presence. `recall_lessons.py`
  is text-only; we parse the files ourselves (lenient, capped, off the
  reactor, mtime-cached) and never touch `.mycelium/locks`.
- Hooks: Claude hooks are written into the repo's
  `.claude/settings.local.json` as absolute paths to whichever plugin copy
  ran init (repos initialised by an old copy keep old hooks until
  `migrate_existing_repos.py` re-runs); Codex hooks are plugin-bundled and
  trust-gated. The detached `claude -p … --dangerously-skip-permissions`
  log-scribe of the pre-0.6 builds is **gone**.
- `.mycelium/run/<host>/<session-id>/` is keyed by the agent's own session
  id, so a Timeline episode can link to that session's mycelium log without
  writing anything.
- Codex install (verified 2026-09-25, codex 0.153.0): `codex plugin
  marketplace add <local path | owner/repo>` + `codex plugin add
  mycelium@mycelium`; the app-server's `skills/list` then reports the ten
  `mycelium:*` skills (scope `user`, `pluginId: mycelium@mycelium`) and
  `hooks/list` its five hooks as `untrusted`. The pre-0.6 installer's copies
  in `~/.codex/skills/mycelium-*` duplicate them and must be moved aside.

### 6.4 The Skills view — "what can my agents do here?"

One list of every skill any agent can use in this workspace, on this host.

```
 41 skills · claude 36 · codex 14 · both 9          [claude] [codex] [search]
 THIS PROJECT
 develop         Run Chimaera locally and iterate…     claude ✓  codex ✓
 verify-app      Verify a change end-to-end…           claude ✓  codex ✓
 FROM PLUGINS
 mycelium:core   Living-repo knowledge framework…      claude ✓  codex ◌ not installed
 YOURS (user-level)
 lab-notebook    Write up today's analysis runs…      claude ✓  codex —
 BUILT INTO THE AGENT                                   (from a running session)
 code-review     Review the current diff…              claude ✓
```

- **Grouped by where it comes from** — this project · from plugins · yours
  (user-level) · built into the agent — because that's what tells you who
  else gets it (a project skill travels with the repo; a user skill doesn't).
- **One chip per agent**: ✓ available · ◌ present but not usable, with the
  reason in words (disabled, plugin not installed for that agent, codex
  project untrusted) · — not available. Invocation shown in each agent's own
  syntax (`/name` claude, `$name` codex — canonical vocabulary).
- **Row → detail**: the rendered SKILL.md (sanitized), its files, mechanical
  flags (scripts, hooks, `allowed-tools`, `` !`cmd` `` blocks), open in the
  editor; load errors codex reports are shown, not hidden.
- **The one fix, later:** "claude-only → make available to codex" writes
  Loadout's deterministic bridge file into the repo, uncommitted (git is the
  audit). No other mutations here.
- Truth comes from the agents (table in §6): codex's own resolution via
  `skills/list`; claude's scan joined with a live session's init when one
  exists (the only source for built-ins — labelled so).

This absorbs Loadout's lens slice 1; its other categories (subagents, rules,
hooks, MCP, guidance) join as the same kind of per-agent rows when their
turn comes. Two Loadout facts are stale and superseded: codex chat's skill
catalog is no longer empty (Pass 26), and mycelium is no longer claude-only.

### 6.5 Browse — later: a marketplace we don't have to run

Not our own registry. A browser over **the marketplaces the agents already
have configured**, plus Chimaera's own workbench plugins:

- claude: `claude plugin list --json --available` (id, name, description,
  marketplace, source); codex: app-server `plugin/list`.
- One card shape for every kind, labelled by what it is and where it runs
  (*claude plugin* · *codex plugin* · *workbench plugin* · *skill pack*),
  with the same "Would add" line; a badge when the same plugin exists for
  both agents (mycelium).
- **Add a marketplace / install / update** = the CLI's own commands in a
  visible terminal (`claude plugin marketplace add`, `codex plugin add` …);
  bare skill packs (a git URL) go through Loadout's pinned-SHA staging and
  full-manifest review. Nothing auto-installs, nothing auto-updates.
- The security record stands (Loadout §9: ToxicSkills, DeepJack): full
  scrollable manifests, "flags, not clearance", no LLM audit badge.

### 6.6 Hook trust — approve in Chimaera, never silently

Codex runs no plugin or project hook until the user trusts it — a real
security boundary (hooks run shell on every session), so Chimaera never
auto-approves. What it removes is the *trip*: the approval happens where the
user already is, with more context than `/hooks` gives.

- **Read:** app-server `hooks/list {cwds}` → per hook `key`, `eventName`,
  `matcher`, `source` (`plugin`/`project`/…), `pluginId`, `sourcePath`,
  `currentHash`, `trustStatus` (`managed|untrusted|trusted|modified`).
- **Show:** in the attach sheet and on the plugin card — each hook in plain
  words (when it fires, what it runs), with behavioural notes where they
  matter ("can keep a turn going until `.living/` is updated").
- **Write, on the user's click:** the same record `/hooks` writes —
  `[hooks.state."<key>"] trusted_hash = "<currentHash>"` in
  `~/.codex/config.toml` — through the app-server's own `config/batchWrite`,
  never by editing the file. Hash-pinned, so an upgrade that changes a hook
  comes back as `modified` with a diff to re-approve.
- **Verify live before building:** that a `config/batchWrite` of that key is
  honoured exactly as a `/hooks` approval on the pinned codex, and that the
  hash format round-trips.

## 7. Agents talking — the *Agent notes* workbench plugin

Off by default; turning it on is the opt-in (no separate switch). Its card:
*"adds 2 tools to every agent here; never starts a turn."*

- **Tools** (chimaera MCP, active workspaces only): `post_note {text, to?}`
  and `read_notes {since?, for_me?}`. `to` is a session or `"mastermind"` —
  which *is* `ask_mastermind`, with the non-triggering inbox it was parked
  for.
- **Notes are coordination, not knowledge** — heads-ups, questions, "I'm
  changing the loader API". Durable findings go through mycelium (§5), so
  there's one place knowledge lives.
- **Mail, not phone.** A post never starts a turn anywhere — no ping-pong
  loops, no surprise bills, and a poisoned note can't set off a chain. A
  recipient sees mail when it (a) reads, (b) gets a one-line hint on hook
  carriers that already fire (the compute-context precedent in `agents.rs`),
  (c) the user clicks **deliver** on the Timeline (a real, user-started
  message), or (d) the Mastermind relays it (`message_agent`, as today).
- **Talking isn't commanding.** Notes are information and questions, framed
  to readers as data; "nobody commands sideways" stands. Rate- and
  size-capped per session.
- **Coverage, honestly:** claude chat/TUI — read, post, hinted, addressable;
  codex chat — read, post, pull-only; codex TUI — read, post, pull-only,
  via a `-c mcp_servers` injection that rides only while a plugin with
  tools is active (live: the per-tool `approval_mode="approve"` runs
  those tools with no prompt; other chimaera tools still ask); subagents —
  inherit the parent's tools, attributed to the parent, not addressable (no
  vendor exposes a running subagent to outside messages); shells — none.

## 8. The Mastermind, upgraded

- **Senses** — `workspace_status` grows (all already in daemon memory):
  Slurm jobs + the current allocation, the effective env prelude, each
  terminal's recent commands + exit codes, links, background work, and open
  windows/surfaces (one additive `surfaces` key on the layout PUT the client
  already sends — the plan's "surface manifest", minus a new route).
- **Memory** — `read_timeline {since, session?}` and `knowledge {query?,
  section?}` (reads, silent in ask mode).
- **Inbox** — with Agent notes on, notes addressed to it show as a chip on
  the dock; clicking it is the user starting the turn.
- **Brief me** — a dock button: one user-started turn with a canned prompt
  and a fixed answer shape — *Needs you · Done · Problems · Next* — citing
  sessions by name. The thing the dashboard can't do: judgment across
  sessions and time. Suggested prompts in the empty dock: "Brief me",
  "What should I do next?", "Anything conflicting?".

## 9. The experience bar — everything earns its keep

**The rule:** every element answers a question the user has at a specific
moment, and nothing else on screen answers it. If it can't name its
question, it's cut; if its question has no answer right now, it collapses to
one honest line (or to nothing). A clickable mockup of these screens —
dashboard, Knowledge with mycelium attached, Plugins · Installed and Skills,
the attach-and-trust sheet — lives on the maintainer's design canvas.

| Element | The question it answers | Shows when | Otherwise |
|---|---|---|---|
| Needs you | "Is anything waiting on me?" | non-empty | nothing |
| Since you left | "What happened while I was away?" | entries since this viewer's last look | "Nothing new since 14:02" |
| Where things stand | "Where is the project?" | a structured provider has content | the attach line |
| Now | "Who's running?" | always — one line; the rail has the detail | — |
| Brief me | "What should I do?" (judgment, not state) | a Mastermind is bound | the setup card |
| Where we left off | "Where did we stop, what's next, what's blocked?" | a handoff exists | latest episodes |
| Findings | "What do we know — and how sure?" | findings exist | — |
| What we decided | "Why did we do X?" | decisions exist | — |
| Watch out for | "What will bite me?" | learnings exist | — |
| Open | "What's left?" | todos / open questions exist | — |
| Guidance & memory | "What are the agents told?" | always, as one row of links | — |
| A plugin's "Adds" line | "What changes if I turn this on — and what does it cost?" | every card | — |
| Per-agent skill chips | "Will codex see this too?" | every skill row | — |

**Visual language** (Chimaera's own — the `app.css` tokens, curated light
and dark, never inverted):

- **Bad news leads.** Within *Since you left*, failures and contradicted
  findings sort ahead of good news; a contradiction is the most valuable
  line a scientist can be shown.
- **The user's words are the headline** (an episode is the prompt that
  started it); the agent's first real sentence is the result; ids and
  metadata step back into muted mono.
- **Mono for identities** — session names, `F-003`, commands, paths,
  versions — the terminal's typographic DNA; prose in the UI face.
- **Confidence you can read at a glance:** mycelium's own ladder (●○○
  preliminary · ●●○ supported · ●●● robust · ✕ contradicted), explained
  once by a legend beside the list, plus an **evidence strip** — one mark
  per ledger row (filled supports, half refines, ring contradicts) — so "how
  much evidence, and does it agree" needs no reading.
- **Colour is never alone:** one accent for good/current, err for
  contradicted/failed, warn for waiting/blocked — each always paired with a
  word; body text holds 4.5:1 in both themes.
- **Cards only for units** (a finding, a learning, a plugin); everything
  else is hairline rows under small-caps section labels.
- **Empty never shows chrome.** An empty section doesn't render; an empty
  surface says one honest line and offers the single action that fills it.

## 10. Leaving today's experience alone

**Core never changes what agents see; plugins always say what they change.**

- Timeline, the dashboard rework, Knowledge, the Skills view and the
  Mastermind upgrades touch no worker: spawn argv, generated settings, MCP
  `tools/list` and initialize instructions stay byte-identical — asserted by
  a test snapshotting them for a workspace with no plugins enabled.
- Every agent-visible addition is a plugin, off by default, whose card states
  the addition; enabling one affects only workspaces where it's active.
- Chimaera writes no knowledge and installs nothing on its own: installs are
  the agents' own CLIs in a visible terminal, on a click.
- HPC: no new standing work (one conditional ≥60 s Slurm poll), bounded
  JSONL, mtime-cached capped reads off the reactor, short-lived single-flight
  codex probes, no SQLite, no LLM in the daemon.

## 11. Phasing — two tracks, joined at Knowledge

| Track A — history & knowledge | Track B — plugins & skills |
|---|---|
| **A1** Timeline writer + route; dashboard re-centred (Since you left, one-line Now); Mastermind senses + `read_timeline` + Brief me | **B1** Plugin seam: manifest, registry, detect, routes, Plugins tab (Installed · workbench), `docs/agent-guides/plugins.md` — **unblocks the LaTeX agent** |
| **A2** Knowledge view (Guidance & memory; structured sections once B2 lands); "Where things stand"; Timeline knowledge entries | **B2** mycelium plugin: read-only reader, attach sheet (incl. CLI-delegated install for its requirements), knowledge MCP tools |
| **A3** Mastermind `knowledge` reads | **B3** Installed · agents (claude JSON + details, codex app-server) + **Skills view** |
| | **B4** Agent notes plugin; codex-TUI MCP injection (live-verified) |
| | **B5** (later) Browse + general installs + skill-pack staging |

Each slice is independently shippable and verified live (`verify-app`). LaTeX
proceeds on B1 in parallel with everything else.

## 12. Open questions [decide]

1. **Knowledge without mycelium** = Guidance & memory + the attach card. Is
   that enough, or should Chimaera ever grow its own structured provider?
   (Recommendation: no — attaching mycelium is one sheet, and a second
   knowledge format is exactly what decision 3 avoids.)
2. **Browse at launch:** plugin marketplaces only, or also bare skill packs
   (git URLs) from day one?
3. **Built-in skills** in the Skills view appear only while a claude session
   is running (they have no file). Show them with that label, or leave them
   out for a steadier list?

## Appendix: what we deliberately do NOT build

- A knowledge store of our own, a "keep"/curation step, or any write into
  knowledge files.
- Delivery that starts a turn; agent-to-agent commands; addressing subagents.
- An LLM anywhere in the daemon (summaries come from the user's own agent,
  on request); embeddings, vector stores, SQLite.
- Our own agent-plugin format, registry or marketplace backend;
  reimplementing `claude plugin` / `codex plugin`; auto-install or
  auto-update.
- Third-party code in the daemon or UI; a plugin "extension host".
- Typing into terminal agents (the exec-409 wall stands).
- Cross-workspace or cross-host knowledge sync (mycelium's `transfer` owns
  that for its users).
