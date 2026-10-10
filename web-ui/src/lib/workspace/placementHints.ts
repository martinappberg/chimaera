/**
 * What a project's row on this computer's Home says when its work is not (only)
 * here: the daemon's own `GET /pro/status` names each project's ownership, and
 * a project the cloud holds otherwise looks idle here ("9m ago", no live
 * session). Only a project owned elsewhere gets a hint — a quiet line, in the
 * row's muted voice, never a badge — and only when Pro is configured on this
 * daemon; without Pro the answer carries nothing to say.
 */
import { api } from "../net/api";
import type { ProTier } from "../net/plan";

/** Whether Home may ask this daemon for ownership at all: only with an
 *  active plan (`proTier`), on this computer's own Home (not a remote host's,
 *  not a browser view) and while the page shows. A free or merely offered
 *  window never requests `/pro/status`. */
export function readsOwnership(o: { tier: ProTier; remoteHome: boolean; gateway: boolean; visible: boolean }): boolean {
  return o.tier === "active" && !o.remoteHome && !o.gateway && o.visible;
}

/** The ownership states that name somewhere else, in a row's words. The rest
 *  (`local`, `awaiting_verification`, `setting_up`, `privacy_disabled`) say
 *  nothing a row needs: the project is here, or not yet anywhere else. */
const HINTS: Record<string, string> = {
  // Ownership holders are opaque IDs, not verified cloud/device routes.
  remote: "Running elsewhere",
  // Files and conversations installing back onto this computer: a return
  // under way (`Ownership::Hydrating` on the receiving daemon).
  hydrating: "Coming back here…",
  // This computer is handing the project over (its sessions are stopping).
  transferring: "Moving…",
};

/** Workspace id → its hint, from a `/pro/status` body; empty unless Pro is
 *  configured. Anything malformed reads as "nothing to say". */
export function parseOwnershipHints(body: unknown): { configured: boolean; hints: Map<string, string> } {
  const hints = new Map<string, string>();
  if (typeof body !== "object" || body === null) return { configured: false, hints };
  const status = body as { configured?: unknown; workspaces?: unknown };
  if (status.configured !== true) return { configured: false, hints };
  for (const row of Array.isArray(status.workspaces) ? status.workspaces : []) {
    if (typeof row !== "object" || row === null) continue;
    const { workspace_id: id, ownership } = row as { workspace_id?: unknown; ownership?: { state?: unknown } | null };
    const hint = typeof ownership?.state === "string" ? HINTS[ownership.state] : undefined;
    if (typeof id === "string" && hint !== undefined) hints.set(id, hint);
  }
  return { configured: true, hints };
}

/** Read this daemon's ownership answer. `null` when it could not be read
 *  (the daemon is unreachable, or failed): the caller keeps what it last knew
 *  rather than dropping a hint on a blip. A daemon without the route has no
 *  hints, for good. Never throws. */
export async function fetchOwnershipHints(): Promise<{ configured: boolean; hints: Map<string, string> } | null> {
  try {
    const res = await api("/pro/status");
    if (res.status === 404) return { configured: false, hints: new Map() };
    if (!res.ok) return null;
    return parseOwnershipHints(await res.json());
  } catch {
    return null;
  }
}
