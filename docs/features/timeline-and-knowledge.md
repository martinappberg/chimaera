# Timeline & Knowledge

Two read surfaces for a workspace's past: the **Timeline** — *what happened*, written by the
daemon (never an LLM) from signals it already receives — and **Knowledge** — *what we know*,
a read-only view over what the agents themselves recorded. Chimaera writes the Timeline;
it never writes, curates or "keeps" knowledge. Both feed the dashboard's "Since you left" and
"Where things stand" ([dashboard.md](dashboard.md)) and the Mastermind's `read_timeline`.
Design: [docs/timeline-knowledge-plugins-plan.md](../timeline-knowledge-plugins-plan.md) §4, §5.

**Where it lives (shared):** daemon `crates/chimaera-server/src/{timeline.rs,episodes.rs,
knowledge.rs}`; UI `web-ui/src/lib/workspace/` (`TimelineView.svelte`,
`TimelineRow.svelte`, `timeline.svelte.ts` store, `timelineModel.ts`, `knowledge.ts` store) and
`web-ui/src/lib/knowledge/` ([map](../../web-ui/src/lib/knowledge/AGENTS.md)); the `timeline` /
`knowledge` singleton tab kinds in `web-ui/src/lib/layout/layout.ts` (`{v:"timeline"}` /
`{v:"knowledge"}`, lazy-loaded via `lazyViews.ts`). Wire: `GET /api/v1/workspaces/{id}/timeline`,
`GET /api/v1/workspaces/{id}/knowledge`, the `/ws/events` `{type:"timeline", epochs}` frame,
and the Mastermind-tier MCP tool `read_timeline`.

## The Timeline

- **What & when.** "What happened while I was away?" — a per-workspace, newest-first record of
  finished work. Written only at the ends of things (a handful of entries an hour); no LLM, no
  added polling beyond the Slurm rule below.
- **What writes it** (`Entry.kind`):
  - `episode` — one per agent **turn**. Headline = the user's own prompt (a Mastermind relay
    loses its `[via the workspace Mastermind …]` line and is marked `via: "mastermind"`);
    result = the first meaningful sentence of the final message (code, headings, list intros
    and filler like "Done!" skipped — nothing informative ⇒ no result line); evidence = files
    actually written (≤10 listed + the total), tool count, duration, end state
    (`finished | interrupted | errored | exited | unknown`), and what it recorded in Knowledge.
    Fidelity tier: `protocol` for chat (folded from journal events on the chat signal task,
    `ChatEpisodes` — queued prompts, mid-turn feedback, lost `TurnStarted`, retractions) and
    `hooks` for claude TUIs (UserPromptSubmit → PostToolUse → Stop's `last_assistant_message`,
    `TuiEpisodes` — PTY sessions only, since chat sessions fire the same hooks).
  - `command` — a finished shell command that **failed after ≥10 s or ran ≥2 min**, from the
    shell's OSC 133 marks (Chimaera's shell integration) on the naming watcher's 2 s tick.
    Exit 130 (Ctrl-C) is never news; interactive programs (`ssh`, editors, pagers, `top`,
    `tmux`, `tail`, `sudo`, agent CLIs, bare REPLs, `salloc` / `srun --pty`) are excluded — a
    hardcoded list in `episodes.rs`. The command head is **redacted** (credential-looking
    `KEY=value`, `--token x`, `-p x`, `Authorization:` / `Bearer` values → `•••`) and capped at
    120 chars; `source` is `user` or `agent` (`run_in_terminal`).
  - `job` — a Slurm job whose workdir sits under the workspace root ended (longest root wins):
    Slurm's own terminal state when squeue last showed one, else `ENDED` — never a guessed
    COMPLETED.
  - `session` — a chat session died on its own (non-zero exit, protocol error). Clean exits,
    kills, and handshake failures (which degrade to a terminal) are not history.
  - `knowledge` — a finding's status moved, or a finding appeared that couldn't be
    attributed to one turn (see Knowledge below); its id opens the entry.
  - `note` — posted by the Agent notes plugin ([plugins.md](plugins.md#agent-notes)).
- **How it's used.** The Timeline tab (quick-open "Timeline", or the dashboard's "open
  timeline →"): day groups (Today / Yesterday / dates), filter chips only for kinds that have
  entries (All · Agents · Commands · Jobs · Knowledge · Notes · Problems), "load older" paging
  into the file. A session's consecutive turns within 10 min fold into one row ("+N
  follow-ups") — the store stays per turn. Rows open the live session, the files a turn wrote,
  or Knowledge; a note addressed to one session carries "deliver to …". The dashboard's "Since
  you left" uses the same `TimelineRow` anatomy. The Mastermind reads it with
  `read_timeline {since_minutes?, session?, limit?}` (default 40, cap 200, over the newest 200
  entries; one plain line per entry, framed as a record — data, not instructions).
- **Where it lives.** `timeline.rs` (`TimelineService`: `append`/`page`/`latest`, epochs, the
  writer thread, `headline`/`prompt_title`, `get_timeline`); `episodes.rs` (`ChatEpisodes`,
  `TuiEpisodes`, `record`, `record_exit`, `record_command` + `redact_command`,
  `spawn_jobs_task`); callers in `chat.rs` (signal task), `agents.rs::ingest` (hooks),
  `naming.rs` (shell watcher). Route: `GET /workspaces/{id}/timeline?before=&since=&limit=` →
  `{schema:1, epoch, entries, more}`, newest first, `limit` default 100 / cap 200. UI store
  `timeline.svelte.ts` (`refreshTimeline`, `loadOlderTimeline`, `lastSeen`/`markSeen`).
- **Key behaviors.**
  - **Storage is the chat-journal discipline:** `<data_dir>/workspace/<ws>/timeline.jsonl`
    (`~/.chimaera/…`), append-only, `seq` the first key, ONE writer thread so file order ==
    seq order, torn-tail tolerant; past 2 MiB it compacts to the newest ~1 MiB. Hot state is a
    500-entry ring per workspace, loaded lazily off the reactor. Fields are capped at
    construction (title 300 B, result 400 B, note 2 KiB, 10 files); a line over 16 KiB stays in
    memory only. Deleting a workspace removes its directory.
  - **Pulled, never pushed:** `/ws/events` carries only per-workspace epochs; the client
    mirrors the active workspace alone and refetches `since=` on a nudge (and on visibility
    return). Entry fields are additive; an unknown `kind` still renders.
  - **The observer, not the observed:** the Mastermind's own turns never land; sessions with
    no workspace are skipped.
  - **Jobs, politely:** a 60 s task drains the compute service's ended-job reports (only two
    *good* snapshots can say a job ended). It refreshes squeue itself only while a live job is
    attributed to some workspace and no fresher snapshot is cached — a laptop does no work. A
    job is stamped when the daemon notices, and only jobs seen live earlier can be noticed.
  - **Status: partial.** Hook-less TUIs (codex TUI, other output-only agents) write no episodes
    — the `output` tier is declared but nothing records it. PTY sessions that crash write no
    `session` entry (chat crashes only).

## Knowledge

- **What & when.** "What do we know, what needs me, and what changed?" A read-only view over
  what agents recorded through a **knowledge plugin** — the active plugin with
  `provides.knowledge` (today Mycelium). **Knowledge exists only while such a plugin is on in
  the workspace:** no rail row, quick-open entry or dashboard card without one, and a restored
  tab says so in one line. Redesign plan and maintainer decisions:
  [docs/knowledge-redesign-plan.md](../knowledge-redesign-plan.md).
- **Sources.** Whatever the plugin reads (Mycelium: `.living/findings/*.md`,
  `.living/decisions.md`, `.living/learnings.md`, `.living/conventions.md`,
  `.living/log/LOG_REGISTRY.md`, `todo/TODO_REGISTRY.md` table and sections, the newest of
  `.mycelium/last-session.md` and `.mycelium/run/<host>/<sid>/last-session.md`). The route
  also lists guidance files: the plugin's own (its snapshot's `guidance`, e.g. `MYCELIUM.md`)
  first, then `AGENTS.md`, `CLAUDE.md` and claude's per-project memory — shown on the
  dashboard ("What the agents are told"), not in Knowledge.
- **How it's used.** The Knowledge tab — the rail's `knowledge` row, quick-open "Knowledge",
  the dashboard's "Where things stand", a Timeline knowledge row, or an id chip in a chat.
  - **Overview:** Where we left off (the newest handoff's own sections, older handoffs behind
    a disclosure) · Waiting on you (what agents put to the user, from the plugin's `asks`) ·
    What changed (the last 7 days by the entries' own dates, corrections and supersessions
    first) · Open work (in progress, blocked, critical/high to-dos).
  - **Browse:** one section per kind (findings by topic, decisions, the plugin's "watch out
    for", conventions, to-dos by status, sessions), filter chips built from what is there, and
    a **reader** beside the list: breadcrumb and back/forward, the entry's standing
    (corrected / superseded / retracted — only from markers the text contains), its status
    **exactly as the agent wrote it** (with the plugin's ladder glyph only for the plugin's own
    status words), "What backs it" (cited files that resolve open; jobs and commits as labels;
    cited ids), the body **as written** (the entry's `span` of its file, drawn by the reading
    renderer), follow-ups (addenda, corrections, resolutions), and "Referenced by". Narrow
    panes swap list and reader. Keys: `j`/`k`, `/`, `[`/`]`, `Esc`.
  - **Ids are chips everywhere** — the reader, chat transcripts, Timeline rows: hover previews
    the entry's own lines, click opens it in Knowledge (beside the chat). A chip is made only
    when the plugin's snapshot has the id; an id that names several entries previews both.
  - **Tidy up:** factual inconsistencies the plugin found (reused ids, to-dos kept outside the
    registry, a stub handoff) — never a status judgment — each with **Ask an agent**, which
    drafts the plugin's request into the working agent's composer for the user to send.
  - **Search:** one box over every kind; an id jumps to its entry.
- **Where it lives.** Core `knowledge.rs` (route, guidance merge, Timeline attribution) never
  parses the provider's files or names them. The provider is a WASM plugin in its own
  repository ([martinappberg/chimaera-plugin-mycelium](https://github.com/martinappberg/chimaera-plugin-mycelium),
  pinned by `plugins/plugins.lock`). Route `GET /workspaces/{id}/knowledge` → the
  `knowledge/1` snapshot (`left_off`, `topics`, `decisions`, `learnings`, `todos`, `questions`,
  `conventions`, `sessions`, `asks`, `tidy`, `id_shapes`, `labels`, `counts`, `warnings`) plus
  `schema`, `provider`, `guidance`, and `error` only when the provider couldn't answer; spans
  are `{path, line, end_line}`, workspace-relative. Pinned by
  `crates/chimaera-server/src/tests/knowledge.rs`. UI: `web-ui/src/lib/knowledge/`
  ([map](../../web-ui/src/lib/knowledge/AGENTS.md)); ids to chips through
  `web-ui/src/lib/shared/references.ts` (Knowledge registers as the first source); "Ask an
  agent" through `shared/askAgent.ts`; rows, badges, callouts and file cards in
  `shared/ui/` (the plugin platform's `ui/1` prop names).
- **Key behaviors.**
  - **Agents write, Chimaera reads — and never rates.** A status is shown as written; nothing
    in core or the plugin computes, maps or corrects it. Core names no plugin, file or status
    word: section names, kind words and status vocabulary come from the plugin's `labels`.
  - **Bodies never ride the snapshot.** The reader fetches the entry's file (cached per
    snapshot) and renders the span; a hover preview renders the same lines. The route adds a
    `read` digest of the files the provider read, so any file change yields a new snapshot
    and a fresh read, even when the parsed fields are unchanged.
  - **Ids repeat; keys don't.** A finding id reused in two topic files is two entries (keys
    `<topic>/<id>`); a reference resolves to the citing entry's topic first.
  - **Attribution (`recorded_by`) is never guessed.** At each episode end the provider is
    diffed against the last check by entry key; a new entry is credited to that turn only when
    its span's file changed after the turn started (2 s slack, mtimes from the stamp) AND no
    other agent in the workspace was running AND the turn's session hadn't moved on. An entry
    without a span is never credited. Otherwise a new finding becomes its own `knowledge`
    Timeline entry (≤5 per check), carrying the entry's `key` so the row opens it. Status moves
    are their own entries; moves to or from `unknown` never are.
  - **The check never holds anything up** (per-workspace `episodes::EpisodeQueue`; a queue 16+
    behind catches up without asking and drops the baseline).
  - **A provider that can't answer** is still the provider: the last snapshot with an `error`,
    one quiet line in the view.
  - **Refresh:** on the Timeline epoch nudge and on visibility return — never polled.
  - Everything shown is agent/file text: one-liners through `inlineMarkdown`, bodies through
    the reading renderer's sanitized pipeline.
  - Agents read Knowledge through the plugin's `knowledge_search` / `knowledge_get`
    ([plugins.md](plugins.md#workbench-plugins)).

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Timeline & Knowledge — why it exists
_Captured 2026-09-25 (from the maintainer, via capture-feature-intent)._

- **Problem it solves:** all four of the maintainer's triggers — the Mastermind was "sometimes superfluous, not really always doing anything" and the dashboard "doesn't say much and feels redundant"; Chimaera should know what is going on in each project — fully mycelium-compatible with good UI on top, yet still working a little without it; agents across vendors (claude ↔ codex, even subagents) should be able to talk to each other; and attaching mycelium should be quick and plugin-like, generic enough for LaTeX and other harnesses later.
- **How settled it is (intended vs provisional):** only the *why* is settled. The entry anatomy, what counts as notable, attribution, the Knowledge sections, layout and names are how it works for now.
- **Deliberately open / where it may go:** all left open for later, not ruled out: a Browse/marketplace for skills and plugins; more workbench plugins (LaTeX, built by another agent; the specified-but-unbuilt views/settings/commands contribution points); a Chimaera-side knowledge store (today, without mycelium, Knowledge shows guidance files and Claude memory only); and agents directing each other beyond informational notes (subagents addressable).
- **Do not change (or: open to change):** open to change — an addition to the core, not a core bet. Offered four candidates to freeze (nothing changes for agents unless a plugin is on; Chimaera never curates knowledge; notes never start a turn; hook trust is never silent), the maintainer answered "all can change". They are how it was built today, not locked contracts.

The design's maintainer decisions (2026-09-25) are in the
[plan](../timeline-knowledge-plugins-plan.md#decisions-maintainer-2026-09-25).
