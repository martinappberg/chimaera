/**
 * Whether a Stop needs a second press: the interrupt ends more than the
 * turn, and the user should hear what before it goes.
 *
 * Only what the interrupt is known to end counts (live, issue #343;
 * crates/chimaera-agent/protocol/passes.md Pass 49):
 *   - claude: a live subagent row of this turn and a background `local_agent`
 *     lane are killed, and so is a `local_workflow` with its agents. A
 *     `run_in_background` command, a Monitor watch, a cron job and a wakeup
 *     survive, so they are never named.
 *   - codex: `turn/interrupt` ends neither a spawned agent nor a background
 *     terminal. Its agent rows are `crossTurn` (process-owned) and it sends
 *     no background lanes, so nothing counts.
 *   - ACP: no subagent or background surface.
 * Keyed on the event shapes, never on the agent's brand: a process-owned row
 * outlives the turn by definition, and the lane names are the agent's own.
 * `remote_agent` lanes are left out until a probe says what a Stop does to
 * them.
 */

/** The fields of a subagent tool row this reads (`ChatStore.activeAgents`). */
export interface StopAgentRow {
  title: string;
  status: string;
  crossTurn: boolean;
}

/** The fields of a background lane this reads (`ChatStore.backgroundTasks`). */
export interface StopLane {
  taskType: string;
  description: string;
  ambient: boolean;
}

export interface StopReach {
  subagents: number;
  workflows: number;
}

/** How long the second press stays armed. Long enough to read six words
 *  (and for a polite announcement), short enough that a stale arm never
 *  turns a later, unrelated Esc into a stop. */
export const STOP_CONFIRM_MS = 5000;

const LIVE = new Set(["pending", "in_progress"]);

/** What a Stop would end besides the turn itself. */
export function stopReach(
  agents: readonly StopAgentRow[],
  lanes: readonly StopLane[],
): StopReach {
  const rows = agents.filter((a) => LIVE.has(a.status) && !a.crossTurn);
  // A foreground agent moved to the background (Ctrl-B) is still its live
  // row AND now an agent lane: count it once (as ChatView's runningTasks).
  const names = new Set(rows.map((a) => a.title.replace(/^(Agent|Task): /, "")));
  let subagents = rows.length;
  let workflows = 0;
  for (const lane of lanes) {
    if (lane.ambient) continue;
    if (lane.taskType === "local_agent" && !names.has(lane.description)) subagents++;
    else if (lane.taskType === "local_workflow") workflows++;
  }
  return { subagents, workflows };
}

function count(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

/** "Also stops 2 subagents", "Also stops 1 subagent and 1 workflow"; null
 *  when the Stop ends only the turn (it then stops at once, as before). */
export function stopReachText(reach: StopReach): string | null {
  const parts: string[] = [];
  if (reach.subagents > 0) parts.push(count(reach.subagents, "subagent"));
  if (reach.workflows > 0) parts.push(count(reach.workflows, "workflow"));
  return parts.length === 0 ? null : `Also stops ${parts.join(" and ")}`;
}

/** While a Stop waits for its second press, what a key does. A key some
 *  surface already handled (the composer's own Esc, a card's Esc, a menu's)
 *  is left alone; Esc elsewhere in this chat confirms; a bare modifier does
 *  nothing; any other key, or Esc in another pane, lets the stop go. */
export function armedKey(
  e: { key: string; defaultPrevented: boolean },
  inChat: boolean,
): "confirm" | "dismiss" | "ignore" {
  if (e.key === "Shift" || e.key === "Control" || e.key === "Alt" || e.key === "Meta") {
    return "ignore";
  }
  if (e.key === "Escape") {
    if (e.defaultPrevented) return "ignore";
    return inChat ? "confirm" : "dismiss";
  }
  return "dismiss";
}
