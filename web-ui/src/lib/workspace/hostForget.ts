import type { HostState } from "../net/native";

/**
 * What forgetting a remote machine does. A host the account keeps connected
 * lives on in the keeper: removing only this computer's saved entry would
 * bring the row straight back at the next host list. So forgetting a kept
 * host turns keep-connected off first, then forgets it; the confirm says so
 * before the person presses it. Never disabled: a kept, connecting or failing
 * host can always be forgotten.
 */
export interface ForgetPlan {
  turnOffKeep: boolean;
  question: string;
}

export function forgetPlan(host: Pick<HostState, "kept">): ForgetPlan {
  return host.kept === true
    ? { turnOffKeep: true, question: "forget this host? Keep connected turns off too." }
    : { turnOffKeep: false, question: "forget this host?" };
}

const UNKEEP_FAILED = "Keep connected couldn’t be turned off, so this host is still here. Try again in a moment.";

/** The confirm row's line when turning keep-connected off failed: the shell's
 *  own sentence when it gave one, else one retry line. */
export function forgetFailure(reason: unknown): string {
  const text = reason instanceof Error ? reason.message : String(reason);
  return /^[A-Z][^\n]{0,240}\.$/.test(text) ? `${text} This host is still here.` : UNKEEP_FAILED;
}
