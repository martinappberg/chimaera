# Timeline & Knowledge

Two read surfaces for a workspace's past: the **Timeline** — *what happened*, written by the
daemon (never an LLM) from signals it already receives — and **Knowledge** — *what we know*,
a read-only view over what the agents themselves recorded. Chimaera writes the Timeline;
it never writes, curates or "keeps" knowledge. Both feed the dashboard's "Since you left" and
"Where things stand" ([dashboard.md](dashboard.md)) and the Mastermind's `read_timeline`.
Design: [docs/timeline-knowledge-plugins-plan.md](../timeline-knowledge-plugins-plan.md) §4, §5.

**Where it lives (shared):** daemon `crates/chimaera-server/src/{timeline.rs,episodes.rs,
knowledge.rs,mycelium.rs}`; UI `web-ui/src/lib/workspace/` (`TimelineView.svelte`,
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
  - `knowledge` — a finding's confidence moved, or a finding appeared that couldn't be
    attributed to one turn (see Knowledge below).
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

- **What & when.** "What do we know — and how sure?" A read-only view with a fixed,
  plain-words shape over what agents recorded as they worked. The user's correction path is the
  file itself ("open file" opens it in the editor — there is no jump to the entry's line).
- **Sources.** The structured provider — **mycelium**, a WASM workbench plugin, only while it
  is *active* here ([plugins.md](plugins.md#workbench-plugins)): `.living/findings/<topic>.md`,
  `.living/decisions.md`, `.living/learnings.md`, `todo/TODO_REGISTRY.md`, and the
  `.mycelium/last-session.md` handoff (falling back to an in-flight
  `.mycelium/run/<host>/<session-id>/` one). Always: the guidance files at the root
  (`MYCELIUM.md`, `AGENTS.md`, `CLAUDE.md` — a thin adapter or `@AGENTS.md` include says where
  it points) and claude's per-project memory (`~/.claude/projects/<encoded cwd>/memory/`,
  counted, `MEMORY.md` linked). Codex memories are not read.
- **How it's used.** The Knowledge tab — from the rail's `knowledge` row (shown only while a
  provider is active), quick-open "Knowledge", the dashboard's "open knowledge →", or a
  Timeline knowledge row. Sections in a fixed order, each named for its question: Where we left
  off (the handoff) · What we found (findings by topic, mycelium's confidence ladder ●○○
  preliminary · ●●○ supported · ●●● robust · ✕ contradicted, plus an evidence strip — one mark
  per ledger row) · What we decided · Watch out for · Open (todos + findings' open questions) ·
  Guidance & memory. A sticky section nav with counts, a "How sure" legend, one client-side
  search box (it covers the handoff too). Without a provider: Guidance & memory plus one card,
  "Use mycelium for Knowledge →", opening the attach sheet.
- **Where it lives.** Core `knowledge.rs` keeps guidance, attribution and the route
  (`get_knowledge`, `prime`, `recorded_since_last_check`); it never parses the provider's
  files. The provider is the WASM plugin `plugins/mycelium` — `src/reader.rs` (plan, parse,
  stamp, fingerprint, the `Knowledge` wire shape), `src/tools.rs` (`knowledge_search` /
  `knowledge_get`), `src/lib.rs` (the `knowledge` export) — asked through
  `plugins::runtime::knowledge` with the stamp the daemon holds. Route:
  `GET /workspaces/{id}/knowledge` → `{schema:1, provider: "mycelium" | null, left_off, topics,
  decisions, learnings, todos, questions, counts, guidance, warnings}`; paths workspace-relative
  (claude memory absolute). The route JSON and the tool texts are pinned by
  `crates/chimaera-server/src/tests/knowledge.rs`. UI: `KnowledgeView.svelte`,
  `FindingRow.svelte`, `Ladder.svelte`, `model.ts`.
- **Key behaviors.**
  - **Agents write, Chimaera reads.** The ladder is mycelium's (derived from its evidence
    ledger), read, never computed. Nothing writes a knowledge file or touches
    `.mycelium/locks`.
  - **A bounded, lenient reader** matching mycelium 0.7.2's writers: malformed input degrades
    to fewer items plus a `warnings` line (never an error); fence- and HTML-comment-aware (an
    example entry in a code block is not knowledge); never follows symlinks. Caps: a file over
    2 MiB is skipped, 16 MiB per read, 200 topic files, 400 items per kind, 50 ledger rows,
    2 KiB per text field. It reads through the plugin host's bounded filesystem functions
    (workspace-relative, symlinks refused, off the reactor). Every request re-stats (metadata
    only): when the stamp — `(path, mtime, len)` triples — still matches the one the daemon
    holds, the plugin answers "unchanged" and the cached snapshot is served; otherwise it
    re-parses. The first ask in a daemon's life compiles the plugin (about 200 ms in a release
    build); later asks take milliseconds.
  - **Ids:** findings by their `F-NNN`; decisions and learnings by a fingerprint (kind + date +
    title hash), never mycelium's positional `L-N` / `D-N`.
  - **Attribution (`recorded_by`) is never guessed.** At each episode end the provider is
    diffed against the last check (the file mtimes come from the stamp); a new entry is
    credited to that turn only when its file changed after the turn started (2 s slack) AND no
    other agent in the workspace was running. Otherwise it stays unattributed — a new finding
    becomes its own `knowledge` Timeline entry (≤5 per check). The baseline is primed at turn
    start (and when the view loads) so the first turn after a restart can be credited; a
    workspace's first check only sets the baseline. Confidence moves are their own entries;
    moves to or from `unknown` (a torn read) never are. `recorded_by` lives in daemon memory
    (gone after a restart); the episode's `evidence.recorded` persists on the Timeline.
  - **Refresh:** the client refetches on the Timeline epoch nudge and on visibility return —
    never polled. A hand edit between turns shows on the next fetch but writes no Timeline
    entry until an episode ends.
  - Everything shown is agent/file text: one-liners through `inlineMarkdown`
    (escape-then-format), bodies through the sanitized chat `Markdown`.
  - Agents reach Knowledge through the mycelium plugin's `knowledge_search` /
    `knowledge_get` ([plugins.md](plugins.md#workbench-plugins)); a Mastermind-only
    `knowledge` tool (plan phase A3) is not built.

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
