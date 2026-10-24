/**
 * Whether an action that interrupts the turn needs a second press: it ends
 * work OUTSIDE the step the turn is on, and the user should hear what before
 * it goes. Esc, either Stop and Clear goal (claude, while the turn runs) ask
 * the same way; Send now never does (it ends only the current step, and
 * claude's `interrupt {send_now}` spares background work).
 *
 * The current step itself — a running command, or the foreground subagent
 * the turn waits on — is what a Stop is pressed to end, so it is never
 * named. Only background work the interrupt is known to kill counts (live,
 * issue #343; crates/chimaera-agent/protocol/passes.md Pass 49):
 *   - claude: a background `local_agent` lane (a foreground agent shows up
 *     as one only once moved to the background, Pass 44) and a
 *     `local_workflow` with its agents are killed. A `run_in_background`
 *     command, a Monitor watch, a cron job and a wakeup survive, so they
 *     are never named.
 *   - codex: `turn/interrupt` ends neither a spawned agent nor a background
 *     terminal, and it sends no background lanes, so nothing counts.
 *   - ACP: no subagent or background surface.
 * Keyed on the lane shapes, never on the agent's brand. `remote_agent` lanes
 * are left out until a probe says what a Stop does to them.
 */

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

/** A subagent row's name without its tool prefix: what its background lane
 *  calls it once the agent moves there (ChatView's running-task count
 *  dedupes rows and lanes by it). */
export function agentRowName(title: string): string {
  return title.replace(/^(Agent|Task): /, "");
}

/** The background work a Stop would end besides the turn itself. */
export function stopReach(lanes: readonly StopLane[]): StopReach {
  let subagents = 0;
  let workflows = 0;
  for (const lane of lanes) {
    if (lane.ambient) continue;
    if (lane.taskType === "local_agent") subagents++;
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

/** The controls that ask, by their `data-stop-control` key: "stop" (Esc
 *  and both Stops) and "goal" (Clear goal). Send now never asks: it ends
 *  only the step the turn is on (a command or the foreground subagent it
 *  waits on — the step the user pressed it to cut off), and Claude's
 *  `interrupt {send_now}` spares background agents and workflows. */
export type ConfirmControl = string;

/** What the view knows when a control is pressed. */
export interface ActionContext {
  lanes: readonly StopLane[];
  /** A turn or a compaction is running (a Stop has something to stop). */
  busy: boolean;
  /** A turn is running. */
  running: boolean;
  /** Clear goal stops the turn first (claude's goal is a Stop hook; codex's
   *  `clear_goal` lets the turn finish and stops nothing). */
  goalClearStops: boolean;
}

/** What the control's action would end besides the turn, as the note words
 *  it; null when it ends only the turn (it then acts at once). */
export function actionReach(control: ConfirmControl, ctx: ActionContext): string | null {
  if (control === "stop") return ctx.busy ? stopReachText(stopReach(ctx.lanes)) : null;
  // Idle, Clear goal is only the `/goal clear` message.
  if (control === "goal") {
    return ctx.running && ctx.goalClearStops ? stopReachText(stopReach(ctx.lanes)) : null;
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
  return control === "goal" ? "clear" : "stop";
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
  /** Esc is the stop key, so it confirms an armed Stop; for any other
   *  armed action (Clear goal, Send now) Esc is cancel. */
  escConfirms = true,
): "confirm" | "dismiss" | "ignore" | "hold" {
  if (e.key === "Shift" || e.key === "Control" || e.key === "Alt" || e.key === "Meta") {
    return "ignore";
  }
  if (onArmedControl && (e.key === "Enter" || e.key === " ")) return e.repeat ? "hold" : "ignore";
  if (e.key === "Escape") {
    if (e.defaultPrevented) return "ignore";
    // Another pane's held Esc is its own: never swallowed from here.
    if (!inChat || !escConfirms) return "dismiss";
    return e.repeat ? "hold" : "confirm";
  }
  return "dismiss";
}
