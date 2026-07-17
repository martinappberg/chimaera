/**
 * Unread-output tracking: which agent sessions FINISHED something the user
 * hasn't looked at yet. A session earns the mark when its turn ends (or a
 * hook-less TUI goes quiet after streaming output) while it is NOT the
 * focused session; focusing it clears the mark. Per-window and in-memory by
 * design — "unread" is about this viewer's eyes, not daemon state, and a
 * reload honestly forgets what you had or hadn't seen.
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
    // A protocol/hooks turn ended, or an idle prompt appeared: the agent
    // handed the floor back with something to read.
    const turnEnded =
      before.agent_state === "running" &&
      (s.agent_state === "finished" || s.agent_state === "idle_prompt");
    // Output-only TUIs have no turn boundary; going quiet after streaming
    // is the honest equivalent (same signal the busy dot uses).
    const wentQuiet = before.output_active === true && s.output_active === false;
    if (turnEnded || wentQuiet) out.add(s.id);
  }
  if (out.size !== unread.size || [...out].some((id) => !unread.has(id))) {
    unread = out;
  }
}
