/**
 * Unread-output tracking: which agent sessions have FULLY finished a turn the
 * user hasn't looked at yet. A session earns the mark only when a whole turn
 * completes — the agent handed the floor back AND no subagents are still
 * working — while it is NOT the focused session; focusing it clears the mark.
 * Per-window and in-memory by design — "unread" is about this viewer's eyes,
 * not daemon state, and a reload honestly forgets what you had or hadn't seen.
 *
 * Deliberately NOT triggered by mid-turn output: a paused output stream (a
 * hook-less TUI thinking, or a gap between tool calls) is still "working", not
 * done — marking it unread would nag on every lull. Only a real turn boundary
 * counts, so a session that is genuinely still busy never wears the mark.
 *
 * Runes discipline: mutate the set only through this module's functions.
 * The set is replaced (never mutated in place) so every `isUnread` read is
 * reactive.
 */

import type { Session } from "./sessions";

let unread = $state<ReadonlySet<string>>(new Set());

/** True when the session finished output the user hasn't looked at. */
export function isUnread(id: string): boolean {
  return unread.has(id);
}

/** The user is looking at it now (focus, open) — the mark clears. */
export function markSeen(id: string): void {
  if (!unread.has(id)) return;
  const next = new Set(unread);
  next.delete(id);
  unread = next;
}

/**
 * Fold one roster snapshot: compare each agent row against its previous
 * state and mark the transitions that mean "it finished something".
 * `focusedId` is exempt — output that ended under the user's eyes was seen.
 * Ids gone from the snapshot are pruned (a killed session can't stay
 * unread forever).
 */
export function foldUnread(
  prev: ReadonlyMap<string, Session>,
  next: readonly Session[],
  focusedId: string | null,
): void {
  const out = new Set<string>();
  const ids = new Set(next.map((s) => s.id));
  for (const id of unread) if (ids.has(id)) out.add(id);
  for (const s of next) {
    if (s.kind !== "agent" || s.id === focusedId || s.mastermind === true) continue;
    const before = prev.get(s.id);
    if (before === undefined) continue;
    // The one trigger: the main turn transitioned from running to a
    // handed-back state (finished, or waiting on the user). In chat mode
    // agent_state only reaches these once the whole turn — every in-turn
    // subagent included — has completed; a claude TUI's Stop hook fires the
    // same way. Mid-turn output lulls never reach here (no agent_state
    // change), so a still-working agent is never marked.
    const turnEnded =
      before.agent_state === "running" &&
      (s.agent_state === "finished" || s.agent_state === "idle_prompt");
    // Belt-and-suspenders for the hooks tier: if any subagent is still on
    // the wire (SubagentStop hasn't landed), the fan-out isn't done — hold
    // the mark until the roster row clears them. Null/absent (chat rows,
    // hook-less TUIs, old daemons) reads as "none", so this never blocks the
    // common case.
    const subagentsBusy = (s.subagents?.length ?? 0) > 0;
    if (turnEnded && !subagentsBusy) out.add(s.id);
  }
  if (out.size !== unread.size || [...out].some((id) => !unread.has(id))) {
    unread = out;
  }
}
