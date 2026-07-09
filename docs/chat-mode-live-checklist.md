# Chat-mode live-test checklist

Purpose: confirm that the review's fixes + refactors **preserved behavior** —
nothing in the structured chat mode regressed. Run against a live daemon + UI
(the `develop` skill; use `chimaerad-isolated` + Vite in a worktree), plus
`just chat-smoke` for the pinned-protocol driver paths.

Legend: ☐ = to verify. Note the agent (claude/codex) where it matters — the two
drivers must behave the same.

## 0. Gates (automated, run first)
- ☐ `cargo +1.96.0 fmt --all --check` clean
- ☐ `cargo clippy --workspace --all-targets -- -D warnings` clean
- ☐ `cargo test --workspace` green (live suite stays ignored)
- ☐ `npm --prefix web-ui run check` → 0 errors / 0 warnings
- ☐ `npm --prefix web-ui run build` succeeds
- ☐ `just chat-smoke` green (bills a few cents; verifies claude + codex wire
  against the pinned versions — note claude is 2.1.205 vs pinned 2.1.204)

## 1. Session create + first turn
- ☐ Create a **claude** chat session → header names the agent, model/mode show.
- ☐ Create a **codex** chat session → same.
- ☐ Send a prompt → user bubble appears, assistant text streams in smoothly
  (no freeze, no jank on a long answer), turn completes, rail state → idle.
- ☐ A tool call renders a card (title, status → completed); output is bounded
  (no multi-MB dump).

## 2. Permissions & questions
- ☐ Trigger a permission (e.g. a Write/Bash) → PermissionCard; **Allow** →
  tool proceeds, card resolves. Rail shows "needs permission" while pending.
- ☐ Trigger a permission → **Deny** → tool marked failed / turn aborts cleanly
  (claude deny aborts; codex decline). No stuck spinner.
- ☐ An AskUserQuestion / requestUserInput → QuestionCard; pick options
  (incl. a case with two same-labeled options if reachable) → answer submits,
  no crash, correct option recorded.
- ☐ A permission preview with a large payload is truncated, not multi-MB.

## 3. Streaming, reconnect, gap-replay
- ☐ Mid-stream, kill the chat WS (close the tab / drop the socket) and reattach
  → transcript replays the gap from `last_seq`, no duplicated or missing events.
- ☐ Type a message **during** a reconnect window → the draft is NOT lost (stays
  in the composer); the status row shows connecting/disconnected.
- ☐ Reconnect after a longer gap (lag path) → the lagged re-replay fills in
  without a hole.

## 4. View toggle chat ↔ terminal (both agents, both directions)
- ☐ claude chat → terminal → back to chat: same conversation continues, same
  session id, no orphan/duplicate rail rows.
- ☐ codex chat → terminal → back to chat: same.
- ☐ Toggle while a turn is running → busy confirm appears (not a silent kill).
- ☐ **Double-click the toggle fast** → no half-dead orphan row, no lost session
  (server serializes; client guards the button).
- ☐ Toggle a **light-theme** codex session to terminal → the TUI is not stuck
  dark (theme carried) [needs the codex flag confirmation from chat-smoke].
- ☐ A rename made in one surface survives the toggle to the other.

## 5. Rewind (claude)
- ☐ Rewind to a checkpoint → files restore, conversation forks at that message.
- ☐ After rewind, the rewound-away turns do NOT reappear as live history on
  reload, and dead checkpoints don't offer broken rewind buttons.
- ☐ Rewind on a session whose driver already died → does not hang ~5s / 500.

## 6. Durability & lifecycle
- ☐ Resume a finished conversation from Recents → history is seeded (journal
  copied), seq numbering continues, the new turn appends cleanly.
- ☐ Kill the daemon mid-write (SIGKILL) and restart → the journal reopens with
  no corruption; seq continues; [if sv-11 landed] chat sessions resurrect or at
  least appear in Recents for resume.
- ☐ `DELETE /api/v1/sessions` (close all) with chat sessions present → chat rows
  actually end and are counted; no claude/codex child left running.
- ☐ `POST /shutdown` → chat drivers stop (not orphaned).

## 7. Untrusted-output safety (the security fixes)
- ☐ Have the agent echo prose containing a raw `<style>…</style>` block → it is
  NOT applied (the workbench isn't restyled; permission cards can't be hidden).
- ☐ Have the agent print an external `[link](https://example.com)` → clicking
  opens a new tab (does not navigate the workbench SPA away).
- ☐ Agent output that references real workspace files → those become clickable
  (path stamping still works), including >50 paths in one message (cu-7).

## 8. Composer & mentions
- ☐ `/` slash popover lists commands; picking one works; arrow-then-narrow then
  Enter does not throw (stale index).
- ☐ `@file` mention resolves against the workspace; `@term:` completes linked
  terminals.
- ☐ Dropping a reference / @term grant into a not-yet-open chat session lands in
  the composer (not dropped).

## 9. Rail & panes
- ☐ A session mid view-switch does NOT jump to the top of the rail (⌘1–9 chords
  stay stable).
- ☐ An agent that exits reconciles off the rail promptly even if /ws/events is
  briefly down.
- ☐ Chat panes have **no** A−/A+ font controls and ignore ⌘± (terminal panes
  still have them).
- ☐ Open a "session changes" tab, then simulate an older build reading the saved
  layout → the whole layout is NOT reset (unknown tab skipped, not poisoning).
- ☐ On an old-git host, the SessionChangesView shows the "git too old" hint
  (not silent); a linked-worktree session's changes resolve against its own repo.

## 10. Resource discipline (spot-check)
- ☐ During a long/heavy session, daemon RSS stays bounded (~150 MB target); no
  monotonic growth across many turns (item_locations / journal ring / blocks all
  bounded).
