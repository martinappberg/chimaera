/**
 * Whether the live status row says its phase word ("Thinking", "Running
 * tools", …). It drops the word only where a row the reader can see already
 * says it; when in doubt, the word is said. Pure, so every runtime's phase
 * signals are pinned by tests (a Codex reasoning item with an empty summary
 * draws no thought row, so its "Thinking" must stay).
 */

export interface StatusWordInput {
  compacting: boolean;
  /** The agent's own phrase (claude `task_summary`). */
  activityLine: string | null;
  activity: { kind: "thinking" | "writing" | "tool" | "waiting"; detail: string } | null;
  running: boolean;
  /** The newest transcript block, if any. */
  newest: { kind: string } | undefined;
  /** The window holds the live tail and the reader is at the bottom. */
  tailInView: boolean;
  /** A running call's line is mounted in the open trailing run, in view. */
  liveToolLineInView: boolean;
}

export function statusWordShown(i: StatusWordInput): boolean {
  if (i.compacting || i.activityLine !== null) return true;
  const a = i.activity;
  // Waiting — on a permission, a question, the next step — has no row
  // saying it.
  if (a === null || a.kind === "waiting") return true;
  if (a.kind === "tool") return !i.liveToolLineInView;
  // The streaming tail row says it only while it is the newest block, still
  // live, and on screen. A token estimate is said only here.
  const tailSays = i.running && i.tailInView;
  if (a.kind === "thinking") return a.detail !== "" || !(tailSays && i.newest?.kind === "thought");
  return !(tailSays && i.newest?.kind === "message");
}
