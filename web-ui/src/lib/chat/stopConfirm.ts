/**
 * Whether an action that interrupts the turn needs a second press: it ends
 * more than the turn, and the user should hear what before it goes. The
 * rule is the action's, not the control's: Esc, either Stop, Clear goal
 * (claude, while the turn runs) and Send now all ask the same way.
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

/** A subagent row's name without its tool prefix: what its background lane
 *  calls it once the agent moves there (ChatView's running-task count shares
 *  this, so both dedupe the same way). */
export function agentRowName(title: string): string {
  return title.replace(/^(Agent|Task): /, "");
}

/** What a Stop would end besides the turn itself. */
export function stopReach(
  agents: readonly StopAgentRow[],
  lanes: readonly StopLane[],
): StopReach {
  const rows = agents.filter((a) => LIVE.has(a.status) && !a.crossTurn);
  // A foreground agent moved to the background (Ctrl-B) is still its live
  // row AND now an agent lane: count it once (as ChatView's runningTasks).
  const names = new Set(rows.map((a) => agentRowName(a.title)));
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

/** What a Send now would end besides the turn: only the foreground
 *  subagents the turn waits on. Claude's `interrupt {send_now}` "ends no
 *  background task … outside the running turn that a Stop ends" (its schema;
 *  2026-10-05), and stops the foreground subagent (Pass 44), so background
 *  agents, a foreground agent already moved to the background and workflows
 *  go on. Codex's Send now is `turn/interrupt`, which ends no spawned agent
 *  (Pass 49): its rows are `crossTurn`, so nothing counts. A claude CLI
 *  older than `interrupt_send_now_v1` (2.1.289) gets a plain Stop instead;
 *  the tested CLIs all advertise it. */
export function sendNowReach(
  agents: readonly StopAgentRow[],
  lanes: readonly StopLane[],
): StopReach {
  const moved = new Set(
    lanes.filter((l) => !l.ambient && l.taskType === "local_agent").map((l) => l.description),
  );
  const subagents = agents.filter(
    (a) => LIVE.has(a.status) && !a.crossTurn && !moved.has(agentRowName(a.title)),
  ).length;
  return { subagents, workflows: 0 };
}

/** "Also stops 2 subagents", "Also stops 1 subagent and 1 workflow"; null
 *  when the Stop ends only the turn (it then stops at once, as before). */
export function stopReachText(reach: StopReach): string | null {
  const parts: string[] = [];
  if (reach.subagents > 0) parts.push(count(reach.subagents, "subagent"));
  if (reach.workflows > 0) parts.push(count(reach.workflows, "workflow"));
  return parts.length === 0 ? null : `Also stops ${parts.join(" and ")}`;
}

/** The controls that ask, by their `data-stop-control` key: "stop" (Esc
 *  and both Stops), "goal" (Clear goal) and `send:<id>` (a waiting
 *  message's Send now). */
export type ConfirmControl = string;

/** What the view knows when a control is pressed. */
export interface ActionContext {
  agents: readonly StopAgentRow[];
  lanes: readonly StopLane[];
  /** A turn or a compaction is running (a Stop has something to stop). */
  busy: boolean;
  /** A turn is running. */
  running: boolean;
  /** Clear goal stops the turn first (claude's goal is a Stop hook; codex's
   *  `clear_goal` lets the turn finish and stops nothing). */
  goalClearStops: boolean;
  /** Ids of the messages still waiting for the agent. */
  queued: readonly string[];
}

/** What the control's action would end besides the turn, as the note words
 *  it; null when it ends only the turn (it then acts at once). */
export function actionReach(control: ConfirmControl, ctx: ActionContext): string | null {
  if (control === "stop") return ctx.busy ? stopReachText(stopReach(ctx.agents, ctx.lanes)) : null;
  // Idle, Clear goal is only the `/goal clear` message.
  if (control === "goal") {
    return ctx.running && ctx.goalClearStops ? stopReachText(stopReach(ctx.agents, ctx.lanes)) : null;
  }
  if (control.startsWith("send:")) {
    const waiting = ctx.running && ctx.queued.includes(control.slice("send:".length));
    return waiting ? stopReachText(sendNowReach(ctx.agents, ctx.lanes)) : null;
  }
  return null;
}

/** A press of an asking control: "act" at once (nothing else would end, or
 *  this is the second press of the control that asked), or "arm" (ask). */
export function pressStep(
  armedControl: ConfirmControl | null,
  control: ConfirmControl,
  reach: string | null,
): "act" | "arm" {
  return armedControl === control || reach === null ? "act" : "arm";
}

/** The verb the hint ends with. */
export function actionVerb(control: ConfirmControl): string {
  return control === "goal" ? "clear" : control === "stop" ? "stop" : "send";
}

/** How a first press came: Esc, a keyboard press on a button (Enter or
 *  Space; its click has `detail === 0`), or a pointer click. */
export type ConfirmVia = "esc" | "key" | "pointer";

/** A button click's gesture: a keyboard activation reports no clicks. */
export function clickVia(e: { detail: number }): "key" | "pointer" {
  return e.detail === 0 ? "key" : "pointer";
}

/** The second-press hint, in the words of the gesture that made the first:
 *  the visible half ("Esc again to stop") and the polite announcement. */
export function confirmHint(verb: string, via: ConfirmVia): { how: string; spoken: string } {
  if (via === "esc") return { how: `Esc again to ${verb}`, spoken: `Press Escape again to ${verb}.` };
  if (via === "key") return { how: `Press again to ${verb}`, spoken: `Press again to ${verb}.` };
  return { how: `Click again to ${verb}`, spoken: `Click again to ${verb}.` };
}

/** While an action waits for its second press, what a key does. A key some
 *  surface already handled (the composer's own Esc, a card's Esc, a menu's)
 *  is left alone; Esc elsewhere in this chat confirms; a bare modifier does
 *  nothing; any other key, or Esc in another pane, lets it go.
 *  Enter or Space on the focused control that asked is that control's own
 *  click (it confirms), so dismissing on its keydown would re-arm instead;
 *  a held key's repeats are swallowed ("hold"), so one long press never
 *  answers its own question. */
export function armedKey(
  e: { key: string; defaultPrevented: boolean; repeat?: boolean },
  inChat: boolean,
  onArmedControl = false,
): "confirm" | "dismiss" | "ignore" | "hold" {
  if (e.key === "Shift" || e.key === "Control" || e.key === "Alt" || e.key === "Meta") {
    return "ignore";
  }
  if (onArmedControl && (e.key === "Enter" || e.key === " ")) return e.repeat ? "hold" : "ignore";
  if (e.key === "Escape") {
    if (e.defaultPrevented) return "ignore";
    if (e.repeat) return "hold";
    return inChat ? "confirm" : "dismiss";
  }
  return "dismiss";
}
