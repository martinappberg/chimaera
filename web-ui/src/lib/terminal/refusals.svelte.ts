import { ownerElsewhere, plainError, runningElsewhere } from "../net/api";
import type { TerminalStatus } from "./ws";

type Owner = "cloud" | "computer" | "other" | null;

/** Input a terminal could not deliver, and why. Shown inline over the pane
 *  for a few seconds — never written into the scrollback. One entry per
 *  session, cleared by its own timer, so this stays tiny. Kept as the
 *  daemon's reason rather than text: the words depend on the pane (where
 *  its project runs, whether it is watching). */
const notes = $state<Record<string, { reason: string | null; message: string | null }>>({});
/** Lasting connection states (asleep, waking), shown until the terminal is live. */
const statuses = $state<Record<string, TerminalStatus>>({});
const timers = new Map<string, ReturnType<typeof setTimeout>>();
const SHOWN_MS = 4000;

/** The daemon's `read_only` refusal (`reason` is additive; older daemons send
 *  only a message) as text for the person typing. `where` names the machine
 *  the project runs on when the pane knows it; by default it is judged from
 *  the daemon this window talks to. */
export function refusalText(reason: string | null, message: string | null, where: Owner = ownerElsewhere()): string {
  switch (reason) {
    case "watching":
      return "You’re watching. Take control to type.";
    case "busy":
      return "Your project is busy. Wait a moment before typing again.";
    case "reconnecting":
      return "Your project is reconnecting. That input was not sent.";
    case "waking":
      return "Waking the cloud machine… That input was not sent.";
    case "elsewhere":
      return runningElsewhere(where);
    default:
      return plainError(message ?? "", where) || "That input was not sent.";
  }
}

/** A lasting state in plain words. A watching pane cannot wake anything by
 *  typing, so it says what comes first. */
export function statusText(status: TerminalStatus, watching = false): string {
  if (status === "waking") return "Waking the cloud machine…";
  return watching ? "Asleep in the cloud. Take control, then press a key to wake it." : "Asleep in the cloud. Press a key to wake it.";
}

export function refuse(id: string, reason: string | null, message: string | null): void {
  notes[id] = { reason, message };
  const previous = timers.get(id);
  if (previous !== undefined) clearTimeout(previous);
  timers.set(
    id,
    setTimeout(() => {
      delete notes[id];
      timers.delete(id);
    }, SHOWN_MS),
  );
}

/** Say a lasting state over the pane until it is cleared (null). */
export function setTerminalStatus(id: string, status: TerminalStatus | null): void {
  if (status === null) delete statuses[id];
  else statuses[id] = status;
}

/** The pane's project owner is asleep (its label says so instead of
 *  "reconnecting"). */
export function terminalAsleep(id: string): boolean {
  return statuses[id] === "asleep";
}

/** What to say over the pane now: a fresh refusal, else a lasting state. */
export function refusalFor(id: string, pane: { where?: Owner; watching?: boolean } = {}): string | null {
  const note = notes[id];
  if (note !== undefined) return refusalText(note.reason, note.message, pane.where === undefined ? ownerElsewhere() : pane.where);
  const status = statuses[id];
  return status === undefined ? null : statusText(status, pane.watching);
}
