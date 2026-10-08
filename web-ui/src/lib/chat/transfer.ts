/**
 * The daemon's transfer pick-up: when a conversation arrives on another
 * machine with work cut off, the receiving daemon sends the agent a message
 * it wrote itself, tagged by origin — `moved` (into the cloud), `home` (back
 * on this computer), `recovered` (either direction, when the other machine
 * stopped responding and the conversation continues from its last saved
 * point), or `returning` (the one sentence the cloud sends a conversation it
 * is about to stop, before it goes back to the computer). Its text is for the agent — where it runs now, what to
 * re-check — so the transcript shows a one-line note of what happened and
 * keeps the text behind a disclosure. A surface that holds only the text (the
 * Timeline records a turn's prompt, not the message's origin tag) recognises
 * the pick-up from its opening words with {@link pickupNote}.
 */

export type TransferOrigin = "moved" | "home" | "recovered" | "returning";

export function isTransferOrigin(origin: string | null): origin is TransferOrigin {
  return origin === "moved" || origin === "home" || origin === "recovered" || origin === "returning";
}

/** The origin tag carries no direction for a recovery; the daemon's own
 *  sentence does ("Chimaera moved this conversation back to the user's
 *  computer because…", `chimaera-server` `transfer_context`). Anything else
 *  reads as the common case: the cloud took over from a computer that went
 *  quiet (a lid closed on a dying battery). */
const HOMEWARD = "back to the user's computer";

/** One line saying where the conversation went, in plain words. `viewer`:
 *  the transcript is read from another device (a browser view), where the
 *  computer that stopped is "your computer", not "this computer". */
export function transferNote(origin: TransferOrigin, text: string, viewer = false): string {
  if (origin === "home") return "Back on your computer";
  if (origin === "moved") return "Continued in the cloud";
  if (origin === "returning") return "Going back to your computer";
  if (text.includes(HOMEWARD)) return "Back on your computer after the cloud stopped responding";
  return `Continued in the cloud after ${viewer ? "your" : "this"} computer stopped responding`;
}

/** What the daemon's restart pick-up (`origin: "restart"`) reads as where a
 *  short line stands in for the message. */
export const RESTART_NOTE = "Picked up after a restart";

/** The daemon's pick-up carries, besides its sentences, blocks the agent
 *  reads as data (`<work-capabilities>…</work-capabilities>` and
 *  `<cloud-profile-data>…</cloud-profile-data>`: JSON about the platform and
 *  the saved profile). A person opening "Show what the agent was told" gets
 *  the sentences only; the blocks are the machine's, not words. */
const MACHINE_BLOCKS = /\n?<(work-capabilities|cloud-profile-data)>[\s\S]*?<\/\1>\n?/g;

export function toldInWords(text: string): string {
  return text.replace(MACHINE_BLOCKS, "\n").replace(/\n{3,}/g, "\n\n").trim();
}

/** The opening words of the daemon's own pick-up messages (`chimaera-server`
 *  `transfer_context`, `restart_message`). They are the daemon's, never
 *  typed by a person, so they recognise a pick-up where its origin tag is
 *  not on hand. */
const TRANSFER_OPENING = "Chimaera moved this conversation ";
const RECOVERY_REASON = " because the other machine stopped responding";
const RESTART_OPENING = "The Chimaera daemon hosting this session restarted";

/** The one-line note a daemon pick-up reads as, or null when `text` is not
 *  one (something a person wrote). For surfaces that quote a turn's prompt
 *  and only have its text — the Timeline's "Since you left" — so the
 *  agent-facing words never pass for what the person said. `viewer` is as in
 *  {@link transferNote}. `free`: a window that can never have Pro
 *  (`proTier` "free") quotes every prompt as it always did. */
export function pickupNote(text: string, viewer = false, free = false): string | null {
  if (free) return null;
  const head = text.trimStart();
  if (head.startsWith(RESTART_OPENING)) return RESTART_NOTE;
  if (!head.startsWith(TRANSFER_OPENING)) return null;
  // The first sentence carries the direction and the reason; look no further
  // (a Timeline title is capped, and the rest is the agent's instructions).
  const sentence = head.slice(0, head.indexOf(". ") === -1 ? head.length : head.indexOf(". "));
  if (sentence.includes(RECOVERY_REASON)) return transferNote("recovered", sentence, viewer);
  return transferNote(sentence.includes(HOMEWARD) ? "home" : "moved", sentence, viewer);
}
