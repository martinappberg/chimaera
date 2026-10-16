# Loops and goals

Work that resumes an agent on its own, shown in its chat: Claude Code's `/loop` (a self-paced wakeup, or interval jobs), Claude's `/goal`, and Codex thread goals. A chat with something pending says what will happen next and when, every turn nobody typed says what started it, and the rail, the dashboard and notifications never call such a chat finished.

**Where it lives (shared):** agent crate `crates/chimaera-agent/src/{autonomy.rs,cron.rs}` (Claude's schedule and goal from its frames), `claude.rs` and `codex.rs` (the drivers), `model.rs` (the events); daemon `crates/chimaera-server/src/{goal_outcome.rs,notices.rs,chat.rs}`; UI `web-ui/src/lib/chat/{AheadCard.svelte,WorkStrip.svelte,ahead.ts}`, the store (`store.svelte.ts`), `ChatView.svelte`, `web-ui/src/lib/shared/{CountdownRing,GoalGlyph,WakeRing}.svelte`, the rail and dashboard predicates in `web-ui/src/lib/workspace/sessions.ts`. Wire: the chat WebSocket's `schedule`, `goal` and `turn_cause` events and `set_goal` / `pause_goal` / `resume_goal` / `clear_goal` commands; the session row's `next_wake` and `goal` on `/ws/events`. Protocol facts: [PROTOCOL.md Pass 46](../../crates/chimaera-agent/PROTOCOL.md).

## The Ahead card

- **What & when.** One quiet pinned card above the composer, beside the plan card and the work trays, while an agent has anything that will resume it: its goal, its pending wakeup, its interval jobs. It answers "will this chat do something by itself, when, and why?" without a click.
- **How it's used.** Start a loop or a goal the agent's own way — `/loop check the deploy`, `/loop 10m post the error graph`, `/goal all tests pass` in a Claude chat, `/goal <objective>` in a Codex chat — and the card appears. Each line is the agent's own words (its reason, the job's schedule and prompt, the goal's objective) with a still countdown ring and a calm time ("in 4m", "in 38s" in the last minute, "at 5:39 PM", "tomorrow 3:47 AM"). Hover a line for its actions; click it for its detail (what it will send, when it runs, when a job expires, a goal's last check, time and tokens) with the same actions as text. Three or more scheduled things fold into "3 scheduled · next: …".
- **Actions.** A Claude loop has no control to cancel it: **Stop** sends "Stop the loop." and **Cancel** on a job sends "Cancel scheduled job <id>." — ordinary messages that show in the transcript, which Claude's own `/loop` tells the model to honor; the row reads "stopping…" until Claude's schedule drops it. A Claude goal's **Clear** sends `/goal clear` (answered locally, no model call). A Codex goal gets **Pause** / **Resume** / **Clear** as direct commands (no turn).
- **Where it lives.** `AheadCard.svelte`; data from the store's `wakeup`, `jobs` and `goal` (the `schedule` level-set and the latest `goal`); actions in `ChatView.svelte` (`stopLoop`, `cancelJob`, `goalPause/Resume/Clear`).
- **Key behaviors.** A row is exactly one line, hover or not: the time and the actions share one cell, so nothing moves under the pointer. Nothing animates while waiting; the ring takes the accent in the last minute, and a run that is the turn in progress shows the live dot. The ticking is the shared 1 Hz clock, only while the card is visible. A dead chat shows nothing (nothing resumes a dead process).

## The folded work strip

- **What & when.** When three or more kinds of work are pinned at once — subagents, background work, a goal, a schedule — the trays and the Ahead card fold into one line, so a busy chat never grows a tower of cards.
- **How it's used.** The goal leads the line (it says why the rest is happening; otherwise the busiest kind leads); every other kind is a glyph and a number ("◉ 2", the ring and "in 4m"). Each part is a button that opens that kind's rows under the line, one at a time; the goal's part opens straight to its detail.
- **Key behaviors.** It folds at three kinds and unfolds only when one kind is left, so it doesn't flip as subagents come and go. The plan card stays its own card. Rows inside it are the trays' and the card's own (`embedded`), with their stop and cancel actions.

## In the transcript

- A turn nobody typed opens with one quiet line naming its cause: "Woke up · <the wakeup's reason>", "Woke on schedule · “<the job's prompt>”" (its schedule on hover), "Continuing toward the goal" (a Codex continuation), next to the existing "Woke on a monitor event" / "Woke on background work" / "Resumed on its own" (a cause the wire doesn't name, such as Claude's hidden loop keepalive).
- A goal's moves are lines in the agents' own words: "Goal set", "Goal not yet met · <Claude's check>", "Goal paused" / "Goal resumed" / "Goal stalled" / "Goal hit its usage limit" / "Goal reached its token budget" (Codex), "Goal achieved · 14m · 3 turns · 212k tokens", "Goal could not be achieved · <reason>", "Goal cleared". A long line opens on click.
- A Codex `sleep` shows as a "sleep: 5m 00s" row.

## Rail, dashboard and notifications

- A chat between loop ticks keeps a calm idle glyph (never "done") with a small ring after its name; the tooltip says "next in 4m · <reason>". Closing it asks first, because that ends the loop.
- The dashboard's Now line and agent cards read "wakes in 4m · <reason>"; the chat is not counted as finished.
- The turn-end "done" notification is held while anything is scheduled, so a loop notifies once, when its last tick ends with nothing left. Each tick still marks the chat unread.
- **Where it lives.** `scheduledWake`, `dotState`, `dotTitle`, `isBusy` in `workspace/sessions.ts`; `WakeRing.svelte` in the rail (`App.svelte`) and `AgentCard.svelte` / `NowLine.svelte`; the row's `next_wake` / `goal` from `ChatInfo` (`chimaera-agent/src/lib.rs`), the hold in `notices.rs`.

## Restarts

- Claude's `--resume` brings back its jobs and its goal but not a pending wakeup. The daemon carries all three (`Carryover`): the jobs and goal seed the new driver, and a lost wakeup makes the restart pick-up message name it ("Your pending loop wakeup was lost with the old process: "…", due in 6 minutes.") and ask Claude to schedule it again — subject to the usual pick-up rules (`chat.resumeAfterRestart`, never a Mastermind, the restart-loop guard).
- Codex keeps its goal on the thread; the driver reads it again when the thread opens.

## How it's derived (Claude)

Claude has no control that lists or reports loops and goals, so the driver keeps them from frames (`autonomy.rs`): `ScheduleWakeup` / `CronCreate` / `CronDelete` / `CronList` calls and their structured results, a job's next run after each fire computed from its cron and Claude's stable per-job jitter (`cron.rs`), a turn the CLI opens on its own (a `command_lifecycle` for a command nobody here sent) named by whatever was due, `/goal`'s own replies, and the synthetic "Stop hook feedback" frame of a failed check. How a goal ended never reaches stdout: after the turn, the daemon reads it from Claude's transcript (`goal_outcome.rs`).

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Loops and goals — why they exist
_Captured 2026-10-16 (from the maintainer: the design pass on 2026-10-15 and the intent questionnaire)._

- **Problem it solves:** agents that resume by themselves (`/loop`, `/goal`, Codex goals) must be visible: what will happen next, when, and why, and a chat that will act again must never look finished. Before this, all three worked in chat but nothing showed them.
- **How settled it is (intended vs provisional):** the *why* and the quietness are settled. The surface should be "insanely good" — something the user loves, "not too much", "super easy to understand and follow while not being in their face" — in the agent's own words, and never "done" too early. The card, the fold rule and the wording are how it works today and can change if improved (an addition to the core, not a core bet).
- **Deliberate calls from the design pass (2026-10-15):** option A (one quiet pinned card, one line per thing) over a composer chip or folding into the background tray; when many things run at once, fold into one line ("could we figure out how to fold it into other things when you just have too many things up at once?"), for any three kinds of work, not only with a loop or goal; stopping a Claude loop sends Claude a visible message (Claude has no control for it); the "done" notification waits for the real end. Hovering a row must not change the card's height — a flicker in the mock was the first thing noticed.
- **Do not change (or: open to change):** open to change — improve freely, keeping the why above.
