# Session history & activity

A lasting record of every agent session in a workspace: who started it, what it wrote, what it
used, and where its transcript is — with or without git. **All sessions** lists every past session
(Recents keeps only the last 20, and can archive rows out of the rail); **Activity** in Settings
totals sessions, tokens and time across every workspace, and the dashboard carries one quiet line
of it. Design: [docs/git-and-session-history-plan.md](../git-and-session-history-plan.md) Part 2
(§7–§10).

**Where it lives (shared):** daemon `crates/chimaera-server/src/history/` (`mod.rs` the record and
its file, `routes.rs`, `edits.rs`, `usage.rs`) and `crates/chimaera-server/src/recents_archive.rs`;
UI `web-ui/src/lib/workspace/` (`SessionsView.svelte`, `SessionEdits.svelte`,
`SameFileNotice.svelte` + `sameFile.svelte.ts`, `history.ts`), the `sessions` singleton tab kind in
`web-ui/src/lib/layout/layout.ts` (`{v:"sessions"}`, lazy-loaded via `lazyViews.ts`),
`web-ui/src/lib/settings/ActivitySettings.svelte`, `web-ui/src/lib/dashboard/ActivityLine.svelte`,
and the rail's Recents in `web-ui/src/App.svelte`. Wire (all bearer-authed):
`GET /api/v1/workspaces/{id}/history`, `GET /api/v1/sessions/{id}/edits`,
`GET /api/v1/workspaces/{id}/same-file`, `GET /api/v1/activity`, `GET /api/v1/activity/csv`,
`POST /api/v1/recents/archive`, `POST /api/v1/recents/unarchive`, `GET /api/v1/recents/archived`,
an additive `key` on each `GET /api/v1/recents` row; the Mastermind-tier MCP tool `read_session`
reads an ended session's record; the same-file line rides the claude hook answer
(`POST /api/v1/agent-events/{id}`).

## The record

- **What & when.** One record per agent session (TUI or chat, every agent), in
  `<data_dir>/workspace/<ws>/sessions.jsonl` beside the Timeline. It opens when the session starts
  (`agents::spawn_agent_watch` — every spawn path passes there exactly once; a view switch or
  rewind keeps the same record) and closes where the session's identity ends
  (`recents::retire_with_resume`, the Mastermind's teardown). Shells keep none.
- **Fields.** `rid` (`<session id>@<started ms>` — a resurrected session keeps its id), `id`,
  `agent`, `ui` (the surface it last ran on), `title`, `first_prompt` (≤300 chars), `models[]`
  (≤4, the agent's own names), `started_by` (`you` · `mastermind` · `restart` · another session's
  id for a fork of it), `started` / `ended` (ms), `outcome` (`exited` — the agent quit or the user
  closed it; `crashed` — a chat driver's protocol error, failed handshake or non-zero exit that
  showed no life after; `retired` — the daemon ended the record: a stop or restart), `files`
  `{n, top≤10}` (workspace-relative), `usage` `{cost_usd, tokens_in, tokens_out, turns}`,
  `transcript` `{kind, journal?, path?, native?}` and `git` (null — see below).
- **Usage sources.** A claude TUI: its statusline heartbeat's running totals
  (`cost.total_cost_usd`, `context_window.total_*_tokens`) and its prompts for turns. A claude
  chat: each result's running cost total and per-turn tokens. A codex chat: the thread's running
  token totals, no cost. A codex (or gemini) TUI: nothing — `null`, never zero. A running total
  counts only what THIS record spent: its increases, the first value as the baseline when it was
  painted before any turn, a drop read as a counter restart, and a resumed conversation's previous
  total (kept as the record's `totals`) subtracted.
- **A daemon restart** closes open records as `retired` (a graceful stop does it itself; a daemon
  that died has them closed at the next boot from `<data_dir>/history-open.json`, the open
  records' ~30 s checkpoint, before the ledger resurrects them). A resurrected session continues
  under a new record with `started_by: "restart"`.
- **The audit trail.** The Mastermind's acts (`spawn_agent`, `spawn_terminal`, `message_agent`,
  `interrupt_agent`) and note deliveries (the user's `deliver`, a worker's auto-mode wake) append
  `act` lines — `{ts, by, act, target?, detail≤200}` — to the same file.
- **Git.** `git` stays null until Part 1's per-session anchors are wired: `history::git_field` is
  the one seam (`{start, current, commits[]}`, read from memory at close).
- **Bounds.** A record is about 1 KiB (lines over 8 KiB are refused). Past 4 MiB the file compacts
  (temp + rename): the newest ~2 MiB of records and every open one stay, older records fold into
  one `month` line per UTC month (sessions, time, and cost and tokens per agent and model), acts
  older than the oldest kept record go. Only open records live in memory. One writer thread;
  every read off the reactor. Deleting the workspace deletes the file.

## All sessions

- **How it's used.** A quiet **All sessions** link at the end of the rail's Recents list (also
  Quick Open "All sessions") opens a pane tab: sessions grouped by day (Today, Yesterday, Sep 26),
  newest first. A row is the agent glyph and the title (else the first prompt), then — muted, on
  the right — who started it only when it wasn't you ("by Mastermind", "after restart", "fork of
  …"), "crashed" when it did, the duration, files changed, commits once git anchors exist, and
  tokens when known. No dollars. Clicking opens the conversation: a running session, or a resume
  through the Recents flow while the agent still has it ("Resume" shows on hover). A row whose
  conversation is gone is muted with a second line, "Conversation no longer available" (the
  tooltip says why: claude deletes transcripts after `cleanupPeriodDays`, 30 by default; no handle
  was recorded); clicking it, or the files count on any row, opens what it changed.
- **Filters.** A search field (titles and first prompts); "All · Claude · Codex" only when more
  than one agent kind exists; **Archived** only when some conversations are archived (see below);
  **Mastermind actions** as a quiet link at the foot.
- **Key behaviors.** Paged by `before` (a `started` ms), 50 a page. Live records come from memory,
  flagged `live`. Refetched while visible and on the `/ws/events` recents nudge (App's
  `nudgeHistory`), never polled. Everything agent-written renders as plain text.

## Archiving Recents

- **How it's used.** Right-click a Recents row → **Archive** (the row animates out); right-click
  the section header → **Archive all** (an inline muted "Archived 12 · Undo" line for ~6 s; Undo
  restores exactly that set). An empty Recents says "No recent conversations" and keeps the All
  sessions link. No confirm, no toast.
- **Only hides.** Archive never deletes a transcript, a chat journal or a record, and never touches
  claude's or codex's own files. Archived conversations stay in All sessions under **Archived**,
  with **Unarchive** on hover or in the context menu. Resuming an archived conversation unarchives
  it (`recents_archive::forget_resumed`, from the create route): it is live again, and once it ends
  it belongs in Recents like any other.
- **Where it lives.** `recents_archive.rs`: per workspace in `<data_dir>/recents-archive.json`
  (≤2,000 entries, the oldest dropped; rewritten atomically off the reactor; removed with the
  workspace), keyed by the identity Recents uses — the native id, or `~kind:title` for a
  handle-less row — with the conversation's `supersedes` ancestors, so it can't reappear under an
  id of its own chain (a claude transcript-store conversation included). Every change bumps the
  recents epoch, so every window's rail and All sessions update.

## What a session changed, with or without git

- The session changes view (`SessionChangesView.svelte`, [git.md](git.md#session-scoped-changes))
  is one list whether or not git exists: the files, each with a small edit count
  (`SessionEdits.svelte`, also in All sessions). A click opens the git diff when there's a repo and
  the file has an uncommitted change, otherwise the agent's own edits for that file, in order,
  opened in place. Commits will come first once Part 1's session anchors are wired.
- `GET /sessions/{id}/edits?workspace_id=` reads the chat journal's edit tool calls (claude's
  Edit/Write/MultiEdit input, codex's patches) or, for a claude TUI, claude's transcript through
  the existing importer; a failed edit is left out. Capped at 100 files, 300 edits, 16 KiB per side,
  1 MiB total. It also says whether the session ran shell commands (`ran_commands`).
- The honest limit, "Changes made by shell commands aren't captured here.", shows only when the
  session ran shell commands and no repository shows those changes.

## Two sessions, one file

- Only for two LIVE agent sessions in one workspace, only while both are live: a small warn-toned
  notice on the dashboard card and on the chat's line just above the input (right side) —
  "qc.py also edited by 'fix normalization' · 3 min ago". Clicking it opens that session. Never on
  the rail. From the `files_touched` lists already on the wire (`sameFile.svelte.ts`); the times
  from `GET /workspaces/{id}/same-file`, fetched only when the set of overlapping pairs changes.
- An agent with hooks (a claude TUI or claude chat) also gets one line of context through the
  existing hook answer, on its next `PostToolUse` or `UserPromptSubmit`: "chimaera: session 'fix
  normalization' (s-…), also running in this workspace, edited src/qc.py 3 min ago." Once per file
  per pair, at most three lines an answer. Nothing locks. With no overlap the hook answer is
  unchanged (`{}`).

## Activity

- **Settings → Activity** (also Quick Open "Activity"): this week's sessions and tokens as the
  headline, with the time agents spent working (the sum of session durations); one bar chart of
  sessions per day for the last 14 days; a table by agent and model and one by workspace (sessions
  and tokens); **Export CSV**. No dollar figures anywhere in the UI.
- **Dashboard line**: "This week: 38 sessions · 1.2M tokens" (tokens left out when unknown),
  opening Activity.
- **The CSV** has one row per session (then the folded months' totals) and is the one place cost
  shows: an `estimated_cost_usd` column (the agent's own estimate at API prices). Formula-like
  cells are quoted as text.
- **Unknown stays unknown.** A codex TUI reports no tokens: "—" with a tooltip saying why, never
  zero.
- `GET /activity?workspace_id=&tz=&days=&weeks=` (tz = minutes east of UTC, for day and week
  boundaries; every workspace when none is named). Each workspace's file is parsed once per change
  into compact rows (a 16-workspace cache keyed by length and mtime); open records add their usage
  so far. No budgets or alerts.

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Session history & activity — why it exists
_Captured 2026-09-29 from the maintainer, in the session that built it (his words quoted)._

- **Problem it solves:** knowing "who is doing what work, is it in worktrees, what runs,
  traceability, accounting", and for agent transcripts "what have they done" — a lasting record of
  every session, with or without git.
- **How settled it is (all additions — deliberate today, improvable):**
  - **Activity, not dollars:** "rename to activity and rather than dollars" — sessions, tokens and
    time; claude's cost is API-priced and means nothing to a subscriber, so it stays out of the UI (the
    CSV keeps it).
  - **Archiving Recents:** "can recents chat be archived as well? like you can choose to archive all,
    or you can right click and archive specific chats" — archive only hides; resuming an archived
    conversation unarchives it (maintainer: yes).
  - **Where All sessions lives:** revealed from Recent on hover rather than a standing row ("if you look
    at Recent it will pop up on hover or something").
- **Deliberately open / left out:** budgets and alerts; sharing records or transcripts with other
  people (planned separately).
- **Do not change:** nothing here is frozen; the record's append-only, bounded storage follows the
  daemon rules, not this feature.
