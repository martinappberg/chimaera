/**
 * The daemon's transfer pick-up: when a conversation arrives on another
 * machine with work cut off, the receiving daemon sends the agent a message
 * it wrote itself (origin `moved` into the cloud, `home` back on this
 * computer). Its text is for the agent — where it runs now, what to re-check —
 * so the transcript shows a one-line note of what happened and keeps the text
 * behind a disclosure.
 */

export type TransferOrigin = "moved" | "home";

export function isTransferOrigin(origin: string | null): origin is TransferOrigin {
  return origin === "moved" || origin === "home";
}

/** The daemon adds this recovery paragraph when the machine that ran the
 *  conversation stopped answering and the other one continued from its last
 *  saved point. The wire carries no separate origin for it, so the note reads
 *  the daemon's own fixed wording (`chimaera-server` `transfer_context`). */
const RECOVERY = "after the previous host stopped responding";

/** One line saying where the conversation went, in plain words. `viewer`:
 *  the transcript is read from another device (a browser view), where the
 *  computer that stopped is "your computer", not "this computer". */
export function transferNote(origin: TransferOrigin, text: string, viewer = false): string {
  const recovered = text.includes(RECOVERY);
  if (origin === "home") {
    return recovered ? "Back on your computer after the cloud stopped responding" : "Back on your computer";
  }
  if (!recovered) return "Continued in the cloud";
  return `Continued in the cloud after ${viewer ? "your" : "this"} computer stopped responding`;
}
