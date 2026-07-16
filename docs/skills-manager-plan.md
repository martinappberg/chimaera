# The Skills Manager ("Loadout") — design & plan

Status: **design draft for discussion** (2026-07-16). Nothing here is built.
This document synthesizes a research pass over the codebase, the current
skills/plugins/MCP ecosystem (verified July 2026 — the Agent Skills standard,
Claude Code plugins & marketplaces, Codex's native skill support, mycelium,
the MCP registry landscape, and the ToxicSkills/DeepJack security record),
plus a three-lens design panel (files-as-truth / marketplace / agents-as-
operators) with a judge and an adversary pass. Decisions marked **[decide]**
are the maintainer's call. A fun datapoint: all three independent designers
converged on the same name — *Loadout* — unprompted.

## 0. The one-paragraph version

A per-workspace **Loadout tab**: one honest inventory of everything this
project equips its agents with — skills, subagents, rules, hooks, MCP
servers, AGENTS.md guidance — rendered as a **claude/codex visibility
matrix** over the repo's *real dotfiles* (files are the database; the daemon
only scans). On top of that lens, three mutations, in order of shipping:
**deterministic fixes** (generate a Codex bridge, create a CLAUDE.md shim —
the 8-line/3-line templates this repo already proved), **installs** (paste a
git URL → pinned-SHA fetch → a full "will install" manifest → files
materialize *uncommitted*, with provenance written into the skill's own
frontmatter so it survives `git clone`), and **doers** — daemon-composed
prompts that spawn a normal Tier-B chat session to do the genuinely
agent-shaped work: *author a project-aware skill*, *sync semantic bridge
drift*. Skills then become visible where agents live: a Skills group in the
chat composer (for codex this is the first discoverability it has at all —
it reports an empty slash catalog today), inserted as `/name` for claude and
`$name` for codex. The defensible core is the **cross-agent lens on a remote
daemon** — marketplaces already exist; nobody has "what can my agents do in
*this* workspace, on *this* host, and is it the same story for both CLIs?"

## 1. What already exists (the surprising inventory)

The repo itself is the prototype: it maintains dual-agent config by hand
today, and the daemon already has every idiom the feature needs.

| Primitive | Where | State |
|---|---|---|
| Canonical skills + Codex bridges (`.claude/skills/<n>/SKILL.md` ↔ `.agents/skills/<n>/SKILL.md`, 8 pairs) | this repo, since [#64](https://github.com/martinappberg/chimaera/pull/64) | proven by hand; **no generator/sync — descriptions drift silently** |
| AGENTS.md canonical + 3-line `CLAUDE.md` `@AGENTS.md` shims (root + 8 nested) | this repo | proven by hand |
| Subagent pairs (`.claude/agents/*.md` ↔ `.codex/agents/*.toml` delegating to the canonical .md) | this repo | proven by hand; TOML `sandbox_mode` stands in for claude's `tools:` |
| Managed-install idiom: daemon-composed curated scripts, HTTPS-only, never sudo, streamed into a **visible PTY session**, provenance in words ("yours"/"chimaera") | `runtimes.rs`, [features/agents.md](features/agents.md) | shipped — the template for pack installs |
| Bespoke per-workspace config panel precedent (own route + own JSON store, fetch-merge-put, explicit Save) | Environment preludes ([features/environment.md](features/environment.md)) | shipped |
| Workspace model + fs routes (list/read/atomic-write with mtime conflicts/create/rename/delete), quickopen's capped-walk discipline | `workspaces.rs`, `router.rs` | shipped — note: fs routes are **not** workspace-confined today |
| Chat slash catalog: claude's `initialize` `commands` array relayed verbatim into the composer popover; **codex emits an empty catalog** | `claude.rs:562`, `codex.rs:574`, `ChatView.svelte` | shipped — codex skills are invisible in chat today |
| `/mcp` panel: live per-server status via session-scoped `mcp_status`/`mcp_toggle`/`mcp_reconnect` — claude-only, never edits config files | `McpPanel.svelte`, PROTOCOL.md | shipped |
| The daemon's own MCP server (linked terminals), injected via generated `--mcp-config` — **claude-only; codex never receives it** | `mcp.rs`, `agents.rs` | shipped |
| Per-workspace epoch invalidation on `/ws/events` (`git`, `recents`) | `router.rs` | shipped — the refresh mechanism to reuse |
| Skills in product code | — | **zero**: no crate or UI file mentions skills; `reload_skills` is a known-but-unadopted claude control subtype |

## 2. Ecosystem ground truth (verified July 2026)

### The Agent Skills standard — this is now the easy part
- **Agent Skills is an open standard** (agentskills.io, Anthropic, Dec 2025):
  a folder with `SKILL.md` (`name` + `description` frontmatter, optional
  `metadata` k/v, `license`, `allowed-tools`), progressive disclosure
  normative. ~40 adopters including Claude Code, **OpenAI Codex**, Gemini
  CLI, Cursor, Copilot.
- **Codex reads skills natively from `.agents/skills/`** (CWD → parents →
  repo root; `~/.agents/skills` user; `/etc/codex/skills` system), invoked as
  `$skill-name` or `/skills`. Per-skill disable via `[[skills.config]]` in
  `config.toml`. Custom prompts (`~/.codex/prompts`) are deprecated in favor
  of skills. So this repo's `.agents/skills/` bridge convention sits exactly
  on codex's official discovery path.
- Claude Code reads `.claude/skills/` (project) / `~/.claude/skills` (user),
  merged commands+skills, rich frontmatter extensions (`when_to_use`,
  `context: fork`, path-gated activation, `` !`cmd` `` dynamic injection),
  live file-watching mid-session.
- **Codex per-project surface is real now**: `.codex/config.toml` (MCP —
  trust-gated), `.codex/agents/*.toml`, `.codex/hooks.json` (hooks stable
  since v0.124.0, content-hash trust-gated). An **untrusted project's entire
  `.codex/` layer is silently skipped** — visibility UI must show this.
- Plugins: both CLIs have plugin systems + marketplaces (claude
  `.claude-plugin/`, `/plugin`; codex `.codex-plugin/`, `/plugins`, launched
  2026-03). Plugin installs are **user-level** in both — there is no
  repo-pinned plugin mechanism. Plugin commands are namespaced
  (`/mycelium:analyze`) and **do not bridge across agents**.

### Mycelium (the founding use case)
`arjunrajlaboratory/mycelium` is a **Claude Code plugin** (v0.3.0, MIT,
~49★): 8 `/mycelium:*` commands, shell hooks, a Python toolkit, and
swappable "convention packs" (`robust-analysis`, `bioinformatics`, …). Its
philosophy: agents' graceful-degradation instinct is wrong for science —
fail hard, flag everything. Install is user-level via claude's own
marketplace (`claude plugin marketplace add arjunrajlaboratory/mycelium` +
`claude plugin install mycelium@mycelium`), then per-project scaffolding by
telling the agent "set up mycelium" (creates `.living/`). Its `skill-bridge`
pack already points at the big bare skill-pack repos: **bioSkills**
(GPTomics, "439 skills"), **scientific-agent-skills** (K-Dense-AI), and
researcher-persona packs. So the two install shapes Loadout must handle are
exactly: *plugin-shaped* (mycelium — delegate to the CLI's own installer)
and *bare skill packs* (bioSkills — vendor selected skills into the repo).

### Registries & the security record
- Marketplaces exist and are crowded: skills.sh (`npx skills add`, ~70
  agents), smithery, the official MCP registry (metadata-only, pre-GA,
  explicitly designed for *curated subregistries* on top), Docker MCP
  Catalog (signed images + SBOM), claude/codex first-party managers. **A
  cross-agent per-project skills manager with a UI already exists**
  (xingkongliang/skills-manager, Tauri, 3.1k★) — but it's local-desktop;
  none of this works over a remote daemon on a login node.
- **ToxicSkills** (Snyk, Feb 2026): of 3,984 community skills scanned,
  36.8% had security issues, 76 confirmed-malicious, 91% of malicious
  skills pair prompt injection with code. Skills also arrive **silently via
  `git pull`** into a trusted repo's `.claude/skills/` (Datadog vector).
- **DeepJack** (Cursor, unfixed): a one-line install-confirmation box that
  content can scroll a payload out of → 1-click RCE. Lesson: the review
  surface must be a full scrollable manifest, never a single-line summary.

## 3. The Loadout tab — the lens

A new singleton workspace tab, `{surface:"loadout"}` (serialized
`{v:"loadout"}`; unknown tab kinds skip on layout rollback, so it's
additive). One entry point: the workspace header, with a quiet count chip
("8 skills · 2 MCP · ⚠1"). It is workspace *content*, not daemon config —
so it is not a Settings section.

**The inventory matrix.** Sections: Skills · Subagents · Rules · Hooks ·
MCP · Guidance. Each row is a real file/dir:

- name + description (frontmatter), source badge when in-file provenance
  exists (§5), git state (tracked / modified / **untracked = "not yet
  shared"**)
- **visibility chips**: `claude` / `codex`, each **filled** (discoverable),
  **hollow** (exists but gated — codex untrusted project, `[[skills.config]]
  enabled=false`, plugin-namespaced), or absent — with the reason in words
  in the tooltip. Rules and hooks render honestly: *"claude-only mechanism —
  codex is told to read these via AGENTS.md"*.
- mechanical risk icons where grep-detectable: executable assets, hook
  wiring, `` !`cmd` `` dynamic-injection blocks, `allowed-tools`.
- amber drift badges (§4).

Row click opens the actual file in the existing editor/preview pane — the
panel has no bespoke editor; `PUT /fs/file` mtime conflicts and fsEvents
refresh already exist. Hand-edits (vim over ssh) stay first-class and simply
appear on rescan — fill the gap, never fight a choice. An **"unrecognized
files present"** honesty row covers format churn (these layouts are <18
months old) rather than pretending completeness.

**Daemon side.** `GET /api/v1/workspaces/{id}/loadout` — bearer-authed and
**workspace-confined** (the first confined fs surface, deliberately).
Computed, never stored: a `spawn_blocking` scan of known dirs only —
`.claude/{skills,agents,rules,settings.json}`, `.agents/skills`,
`.codex/{agents,config.toml,hooks.json}`, `.mcp.json`, the
AGENTS.md/CLAUDE.md tree — no recursive walks outside them, ≤8 KB
frontmatter reads, entry caps, lenient never-brick parsing, results cached
on dir mtimes (Lustre discipline). Returns a **versioned envelope**
(`{schema: 1, skills: [...], ...}`) — this becomes stable public wire on day
one, so the shape is designed once, deliberately. No new `/ws/events` frame
in slice 1: these dirs live in the repo, so changes already bump the
per-workspace **git epoch**; the panel refetches on git epoch + focus + its
own mutations.

## 4. Cross-agent visibility & drift — deterministic only

The scan encodes the convention this repo proved by hand: canonical
`.claude/skills/<n>/SKILL.md` ↔ `.agents/skills/<n>/SKILL.md` bridge;
CLAUDE.md `@AGENTS.md` shims; subagent `.md`/`.toml` pairs. Drift checks are
**mechanical file facts, never guesses**:

- bridge missing for a canonical skill / orphaned bridge
- bridge's relative link dangling (the `check-doc-links.mjs` guarantee,
  computed live in Rust)
- frontmatter `name` mismatch or spec violation (≤64 chars, lowercase,
  matches folder)
- CLAUDE.md shim missing/nonstandard beside an AGENTS.md
- subagent pair incomplete
- MCP server present in `.mcp.json` but absent from `.codex/config.toml`
  (informational, not auto-fixed)

Two one-click fixes, both deterministic templates via existing fs routes:
**Generate bridge** (the 8-line bridge; the condensed description is opened
in the editor as a *flagged draft* — never silently auto-condensed, since a
bad condensation manufactures exactly the semantic drift the panel exists to
catch) and **Create shim** (the 3-line CLAUDE.md). Semantic drift — a
canonical description that changed meaning since its bridge was written — is
the **bridge-sync doer's** job (§6), not a heuristic's. Subagent md→toml
generation is deliberately *not* mechanical (`sandbox_mode`, `model`, real
`developer_instructions` can't be derived — a stub TOML wearing a checkmark
is fake parity); it belongs to hand or agent authorship.

## 5. Installing — two honest paths

**The fork is the design**: pack shape decides the flow, stated in words.

**Bare skill packs (bioSkills, scientific-agent-skills, a colleague's
repo).** Loadout → Skills → *Add from source* → paste a git URL/`owner/repo`
`[+ref]`.

1. The daemon fetches with the managed-install discipline: composed script,
   HTTPS-only, `git clone --depth 1` **pinned to a SHA**, into
   `runtime_dir()/loadout/staging/` — never the workspace, night-scrubbed.
2. The staging scan produces the **"will install" manifest**: every file,
   per-skill frontmatter, and red flags (hooks, scripts, `` !`cmd` ``
   blocks, `allowed-tools`, secret-pattern hits, binaries) — rendered as a
   full scrollable surface (the anti-DeepJack rule), framed as **"flags, not
   clearance"** (static scanning is beatable — ToxicSkills payloads hid in
   referenced files).
3. The user selects skills (439 is too many for a checkbox wall — search
   inside the manifest) and approves. **No agent ever reads pack content
   before this human approval** — an LLM "auditor" reading malicious content
   pre-approval is itself the injection channel, so there is deliberately no
   audit-agent gate.
4. Materialize: selected skills land in `.claude/skills/` + generated
   `.agents/skills/` bridges. Provenance is written **into the skill's own
   frontmatter** — `metadata: {source: "github.com/...", ref: "<sha>"}` —
   the spec's own k/v field, so provenance is git-committed, survives
   `git clone`, and reads identically from the laptop daemon and the
   Sherlock daemon over the same NFS home. **No daemon-side lock database.**
5. Everything lands **uncommitted**: the git pane is the audit, git is the
   undo, nothing auto-commits, the daemon never executes anything installed.
   Uninstall = delete the files whose provenance matches (visible in diff).
   AGENTS.md's skill list gets a *suggested* edit shown as a diff.

**Plugin-shaped packs (mycelium).** The staging scan detects
`.claude-plugin/plugin.json` and says so: *"This is a Claude Code plugin —
installs user-level via claude's own plugin manager; scaffolds
per-project."* One click composes the verbatim documented commands
(`claude plugin marketplace add arjunrajlaboratory/mycelium && claude plugin
install mycelium@mycelium`) and streams them into a **visible shell PTY
session** in the workspace — exactly the `POST /agents/{id}/install`
pattern; chimaera never reimplements the CLI's plugin client. On exit, a
follow-up card: *"mycelium sets up per-project. Send 'set up mycelium' to a
claude session here?"* — one click opens chat with the prompt staged. The
row then reads honestly: *mycelium — via claude plugin — claude only*
(namespaced plugin commands don't bridge; no fake parity).

## 6. Doers — the agent-first mutations

For work that is genuinely agent-shaped, the panel doesn't grow forms — it
dispatches a **normal Tier-B chat session** with a daemon-composed prompt
(rust-embedded, versioned templates — the same auditability posture as
curated install scripts; never client-composed), spawned **ask-first**, one
per kind per workspace (409), exit-watcher → rescan. The session is fully
steerable, lands in recents, and its journal is the audit trail (no separate
runs ledger — the chat journal already is one).

Two doers, not five:

- **Create skill** — Martin's "create skills doer". One field ("skill for
  launching our nf-core pipeline on Slurm"). The template primes the agent
  with: Agent Skills spec essentials (frontmatter rules, progressive
  disclosure, <500-line body), this repo's canonical+bridge convention, and
  the workspace's AGENTS.md. The agent interviews briefly, writes canonical
  + bridge + the AGENTS.md list entry, and test-fires the skill in a
  subagent before reporting. A deterministic **Scaffold** button (template
  files, open in editor) exists beside it for when you don't want a billed
  run.
- **Bridge & sync** — reads canonical skills whose bridges drifted
  semantically, re-condenses descriptions properly, updates AGENTS.md —
  i.e., does what [#64](https://github.com/martinappberg/chimaera/pull/64)
  did by hand.

Doer template quality is the product here and needs a chat-smoke-style
(billed) verification loop before shipping each template.

## 7. MCP management

Two truths, kept separate and both shown. **File truth**: the MCP section
parses `.mcp.json` and `.codex/config.toml [mcp_servers]` read-only,
side-by-side, with per-agent presence chips and the gates chimaera can't
bypass stated in words (claude's per-user approval of project servers;
codex's project-trust prompt). `${VAR}` references are never resolved; no
secrets displayed or stored. **Live truth** stays where it lives today — the
session-scoped `/mcp` chat panel — each row deep-links "check status in a
session" rather than duplicating it. **Add server** starts as a small form
writing `.mcp.json` only; writing `.codex/config.toml` (comment-preserving
`toml_edit` round-trips of hand-edited config) waits until it has
fs-file-grade atomic/mtime rigor — one corrupted config burns all trust.
User-scope config (`~/.claude.json`) is never touched. Adjacent honest gap,
out of scope but recorded: the daemon's own linked-terminals MCP is
injected claude-only today; codex sessions can't reach it.

## 8. Skills where agents live — chat & TUI

- **Composer Skills group.** The slash popover grows a Skills group. Claude
  entries come from the init catalog we already relay, joined with the scan
  for descriptions + a "view file" native action. **Codex reports an empty
  catalog today, so synthesized entries from the `.agents/skills` scan are
  the first skill discoverability codex chat has at all** — the single
  highest-value chat integration per line of code. Insertion is
  agent-correct: `/name` for claude, `$name` for codex — canonical
  vocabulary, never relabeled. Chimaera inserts text; the CLI does the
  invoking.
- **Session chip.** A passive loadout chip in the session header ("8 skills
  · 2 MCP") for chat *and* TUI sessions, linking to the tab. Zero PTY
  injection — Tier A discovers its dirs natively.
- **Usage badges.** Frames whose tool/command name matches a known inventory
  entry get a small "used skill: …" badge — labeling, not new protocol.
- **Later:** adopt claude's known-but-unadopted `reload_skills` control
  subtype so a mid-session install lands without restart (driver change ⇒
  `just chat-smoke`).

## 9. Trust & security (the threat model)

The environment-preludes page already names the rule: anything derived from
a checked-in workspace file needs an explicit confirmation gate. Skills are
that, squared — instructions *and* code, 36.8% dirty in the wild.

1. **Provenance over promises.** Pinned-SHA fetch; provenance in-file;
   three-word source labels; "flags, not clearance" framing on all scans.
2. **The human gate is the gate.** Full scrollable manifest before any file
   lands; no agent reads third-party pack content pre-approval; no LLM
   audit badge (a flipped auditor *launders* trust).
3. **Git is the audit and the undo.** All writes land uncommitted in the
   working tree; the daemon never executes installed content; hooks arrive
   inert as file edits the user must commit/enable.
4. **The `git pull` vector.** Inventory is diffed against a small ack
   snapshot (`~/.config/chimaera/loadout-ack.json`, atomic-write, pruned on
   workspace delete): a skill that appeared without a Loadout action gets a
   **"new since last review"** badge — chimaera can't gate the CLIs' own
   loading, so it warns, mirroring codex's content-hash hook trust.
5. **Rendering.** All third-party frontmatter/markdown rendered sanitized
   (agent-output rule), no raw `{@html}`, no single-line review surfaces.

## 10. Phasing

1. **Slice 1 — the lens (a week, dogfoodable on this repo's own 8 skills).**
   Scanner route + versioned envelope, Loadout tab, visibility matrix with
   honest chips, deterministic drift badges, Generate-bridge/Create-shim
   (flagged-draft descriptions), git-epoch refresh. Rust unit tests on the
   scanner (the UI has no JS tests). `feat:` ⇒ feature page + intent capture.
2. **Slice 2 — chat visibility.** Composer Skills group (the codex win),
   session loadout chip.
3. **Slice 3 — installs.** Staging + manifest gate + bare-pack materialize
   with in-file provenance; plugin delegation flow (mycelium) + staged
   "set up" chat card; ack-snapshot badge.
4. **Slice 4 — doers.** Create-skill (+ scaffold button), bridge-sync;
   template verification loop.
5. **Slice 5+.** MCP add-server writes (with `toml_edit` rigor),
   `reload_skills` adoption, on-demand update check (a per-row button
   comparing in-file SHA against upstream — never polled), user-scope
   (`~/.claude/skills`, `~/.agents/skills`) display, dashboard tie-ins
   (out of scope here; the agent-dashboard plan's status feed — branch
   `claude/agent-dashboard-design-aa17b8` — is where "skill X was used 12×
   this week" would live).

## 11. Open questions **[decide]**

- **The name.** All three designers independently landed on *Loadout*.
  Alternatives: plain *Skills*, *Gear*, *Kit*. Lowercase-chimaera vocabulary
  matters here.
- **Bridge files vs symlinks** for installed spec-pure packs:
  `.agents/skills/<n>` → symlink into `.claude/skills/<n>` would eliminate
  description drift entirely (mycelium itself ships a SKILL.md symlink), at
  the cost of Windows/git-config edge cases and losing the condensed
  codex-style description. The repo's own hand-built convention says bridge
  files; installs could go either way.
- **Canonical direction.** This plan keeps `.claude/skills` canonical (the
  repo's proven convention, and claude's frontmatter is richer). The
  contrarian option — canonical in `.agents/skills` since that's the open
  standard's cross-tool home and codex's native path — deserves one
  explicit yes/no.
- **Install scope.** Workspace-only at first, or also user-scope
  (`~/.claude/skills` / `~/.agents/skills`)? On HPC the home dir is shared
  across nodes, so user-scope = install once, every project on the cluster
  sees it. Powerful, but it weakens the per-workspace story and the git
  audit. Recommendation: workspace-only through slice 3.
- **Doer priority.** Martin named the create-skill doer explicitly — should
  it jump to slice 2/3 ahead of installs?
- **Slice-1 hook/rule editing.** The matrix *shows* hooks and rules; editing
  stays "open the file". Enough, or do rules deserve a path-glob-aware
  editor eventually?

## Appendix: what we deliberately do NOT build

- **Catalog federation / curated marketplace UI** — skills.sh, `/plugin`,
  `/plugins`, smithery, and the MCP registry already exist; a one-maintainer
  curated seed list rots by Q4. A paste-a-git-URL box suffices for years.
  (Revisit only if a "chimaera curated for science" list becomes a real
  community artifact.)
- **A daemon-side provenance/lock database** — breaks files-as-truth: on a
  shared NFS home, per-host daemon state shows different truth per daemon,
  and teammates' clones show none. In-file frontmatter provenance does the
  job.
- **Update polling** (`git ls-remote` on a timer) — network-dependent
  standing behavior on proxy-restricted login nodes; on-demand per-row check
  only, later.
- **An LLM security-audit gate** — the reviewer reading malicious content is
  the injection channel; an "agent-reviewed ✓" badge launders trust. Static
  flags + human manifest approval + git diff are the controls.
- **Mechanical subagent md→toml bridges** — underivable fields make it fake
  parity.
- **A runs ledger / Runs rail** — doers are chat sessions; the journal and
  recents already record them.
- **"Last used" usage stats from journal scans** — cost with no decision it
  informs (until the dashboard exists to give it a home).
- **Editing user-scope agent config** (`~/.claude.json`, `~/.codex/`) —
  fill the gap, never fight a choice.
