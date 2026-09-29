import { plainError } from "../net/api";

/** Input a terminal could not deliver, and why, in plain words. Shown inline
 *  over the pane for a few seconds — never written into the scrollback. One
 *  entry per session, cleared by its own timer, so this stays tiny. */
const notes = $state<Record<string, string>>({});
const timers = new Map<string, ReturnType<typeof setTimeout>>();
const SHOWN_MS = 4000;

/** The daemon's `read_only` refusal (`reason` is additive; older daemons send
 *  only a message) as text for the person typing. */
export function refusalText(reason: string | null, message: string | null): string {
  switch (reason) {
    case "watching":
      return "You're watching. Take control to type.";
    case "busy":
      return "Your project is busy. Wait a moment before typing again.";
    case "reconnecting":
      return "Your project is reconnecting. That input was not sent.";
    case "elsewhere":
      return "This project is running on another device right now.";
    default:
      return plainError(message ?? "") || "That input was not sent.";
  }
}

export function refuse(id: string, reason: string | null, message: string | null): void {
  notes[id] = refusalText(reason, message);
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

export function refusalFor(id: string): string | null {
  return notes[id] ?? null;
}
