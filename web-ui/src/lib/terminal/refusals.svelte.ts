import { ownerElsewhere, plainError, runningElsewhere } from "../net/api";
import type { TerminalStatus } from "./ws";

type Owner = "cloud" | "computer" | "other" | null;

/** Input a terminal could not deliver, and why. Shown inline over the pane
 *  for a few seconds — never written into the scrollback. One entry per
 *  session, cleared by its own timer, so this stays tiny. Kept as the
 *  daemon's reason rather than text: the words depend on the pane (where
 *  its project runs, whether it is watching). */
const notes = $state<Record<string, { reason: string | null; message: string | null }>>({});
/** Lasting connection states (idle in the cloud, connecting), shown until the terminal is live. */
const statuses = $state<Record<string, TerminalStatus>>({});
/** Terminals whose socket is answered, or open and kept for an owner that has
 *  not answered yet. One entry per such terminal, removed when it drops. */
const kept = $state<Record<string, true>>({});
const timers = new Map<string, ReturnType<typeof setTimeout>>();
const SHOWN_MS = 4000;

/** The daemon's `read_only` refusal (`reason` is additive; older daemons send
 *  only a message) as text for the person typing. `where` names the machine
 *  the project runs on when the pane knows it; an unknown owner stays neutral. */
export function refusalText(reason: string | null, message: string | null, where: Owner = ownerElsewhere()): string {
  switch (reason) {
    case "watching":
      return "You’re watching. Choose Type here to type.";
    case "busy":
      return "Your project is busy. Wait a moment before typing again.";
    case "reconnecting":
      return "This project can’t be reached right now. That input was not sent.";
    case "waking":
      return "Connecting… That input was not sent.";
    // The daemon's own plain words name where the work is going.
    case "bringing":
      return message ?? "Moving this terminal here… That input was not sent.";
    case "still_working":
      return "Your other computer is still working on this. Try again when it pauses.";
    case "elsewhere":
      return runningElsewhere(where);
    default:
      return plainError(message ?? "", where) || "That input was not sent.";
  }
}

/** A lasting state in plain words: the progress of the user's own key
 *  press, or where the terminal is, never a state of the cloud. A watching
 *  pane cannot continue anything by typing, so it says what comes first. */
export function statusText(status: TerminalStatus, watching = false): string {
  if (status === "waking") return "Connecting…";
  if (status === "bringing") return "Moving this terminal here…";
  return watching ? "Idle in your cloud. Choose Type here, then press a key to continue." : "Idle in your cloud. Press a key to continue.";
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

/** The terminal's socket is answered or kept open (see `SessionSocket`'s
 *  `onKept`), or not. */
export function setTerminalKept(id: string, value: boolean): void {
  if (value) kept[id] = true;
  else delete kept[id];
}

/** Whether the terminal's own socket reaches its owner's side, for the
 *  pane's placement label: the row's failed roster read is then not this
 *  terminal reconnecting. */
export function terminalKept(id: string): boolean {
  return kept[id] === true;
}

/** What the terminal's socket heard about the owner (asleep, waking), for
 *  the pane's placement label: it says so instead of "reconnecting". */
export function terminalStatus(id: string): TerminalStatus | null {
  return statuses[id] ?? null;
}

/** What to say over the pane now: a fresh refusal, else a lasting state. */
export function refusalFor(id: string, pane: { where?: Owner; watching?: boolean } = {}): string | null {
  const note = notes[id];
  if (note !== undefined) return refusalText(note.reason, note.message, pane.where === undefined ? ownerElsewhere() : pane.where);
  const status = statuses[id];
  return status === undefined ? null : statusText(status, pane.watching);
}
