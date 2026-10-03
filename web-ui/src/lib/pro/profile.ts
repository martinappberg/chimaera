import { api } from "../net/api";
import type { MirrorProfile } from "../net/native";

/**
 * A project's cloud profile as the daemon stores it (`GET /pro/profile`).
 * `PUT /pro/profile` replaces the whole profile, so a save always starts from
 * a fresh read and carries its ETag back with every field, including ones this
 * page does not know yet. A concurrent proposal or account change refuses it.
 */
export type StoredProfile = Record<string, unknown>;

/** The setup command an agent proposed, waiting for the user; null if none. */
export function proposedSetup(profile: MirrorProfile | null | undefined): string | null {
  const pending = profile?.pending_setup_command;
  return typeof pending === "string" && pending.trim() !== "" ? pending : null;
}

/** Commands a cloud machine did not run because they need the user's computer
 * (`profile.deferred`), in the order they were kept. Nothing runs them by
 * itself; they are listed for the user, never offered as a button. */
export function computerSteps(profile: MirrorProfile | null | undefined): string[] {
  const steps: unknown[] = Array.isArray(profile?.deferred) ? profile.deferred : [];
  return steps.filter((step): step is string => typeof step === "string" && step.trim() !== "");
}

export type ProposalDecision = "confirm" | "dismiss";

/**
 * The profile to save for the user's decision on the proposal they were
 * shown. Confirming makes exactly that command the project's setup command;
 * either decision clears the proposal. Everything else round-trips. Null
 * when the stored proposal is no longer the one shown (an agent replaced or
 * withdrew it meanwhile): a decision is never applied to a command the user
 * did not see.
 */
export function decideProposal(fresh: StoredProfile, shown: string, decision: ProposalDecision): StoredProfile | null {
  if (fresh.pending_setup_command !== shown) return null;
  return {
    ...fresh,
    ...(decision === "confirm" ? { setup_command: shown } : {}),
    pending_setup_command: null,
  };
}

/** How often a save refused while the project is mid-copy is sent again. */
const BUSY_RETRIES = 3;
const BUSY_RETRY_MS = 2000;

/**
 * Apply the user's decision on a proposed setup command. "changed" means the
 * proposal was replaced or withdrawn since it was shown and nothing was saved.
 * The daemon refuses a profile save while the project is being copied (409);
 * that is retried on its own a few times before it counts as a failure.
 */
export async function settleProposal(
  workspaceId: string,
  shown: string,
  decision: ProposalDecision,
  wait: (ms: number) => Promise<void> = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
): Promise<"saved" | "changed"> {
  const query = `/pro/profile?workspace_id=${encodeURIComponent(workspaceId)}`;
  let firstRevision: string | undefined;
  for (let attempt = 0; ; attempt += 1) {
    const read = await api(query, { cache: "no-store", signal: AbortSignal.timeout(15_000) });
    if (!read.ok) throw new Error("profile_unavailable");
    const fresh = await read.json() as unknown;
    if (typeof fresh !== "object" || fresh === null || Array.isArray(fresh)) throw new Error("profile_unavailable");
    const next = decideProposal(fresh as StoredProfile, shown, decision);
    if (next === null) return "changed";
    const revision = read.headers.get("etag");
    if (revision === null) throw new Error("profile_not_saved");
    if (firstRevision !== undefined && revision !== firstRevision) return "changed";
    firstRevision = revision;
    const saved = await api(query, {
      method: "PUT",
      headers: { "Content-Type": "application/json", "If-Match": revision },
      body: JSON.stringify(next),
      signal: AbortSignal.timeout(35_000),
    });
    if (saved.ok) return "saved";
    // A revision change can be a replacement account, even with the same
    // command text. Only another explicit decision may approve that profile.
    if (saved.status === 412) return "changed";
    if (saved.status !== 409 || attempt >= BUSY_RETRIES) throw new Error("profile_not_saved");
    await wait(BUSY_RETRY_MS);
  }
}
