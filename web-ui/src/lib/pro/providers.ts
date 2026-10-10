/** Shared catalog labels and additive paused-session intent; paid panel policy lives privately. */
import catalog from "../../../../crates/chimaera-core/src/cloud-providers.json";

/** A provider's display name from the shared catalog, for ids the daemon did
 * not list (never a raw id when a name is known). */
export function providerLabel(id: string): string {
  return catalog.providers.find(provider => provider.id === id)?.label ?? id;
}

const SAFE_ID = /^[a-zA-Z0-9_-]{1,128}$/;

/** What a paused session waits for, from the daemon's additive
 * `blocked_provider` on its row: the agent to connect (named from the catalog)
 * and its project, for the shared connection flow. Null for any row without
 * one, so a row from an older daemon or a free user's session is unchanged. */
export function pausedConnect(row: unknown): { providerId: string; label: string; workspaceId?: string } | null {
  if (typeof row !== "object" || row === null) return null;
  const { suspended, blocked_provider: providerId, workspace_id: workspaceId } = row as Record<string, unknown>;
  if (suspended !== true || typeof providerId !== "string" || !SAFE_ID.test(providerId)) return null;
  return {
    providerId,
    label: providerLabel(providerId),
    ...(typeof workspaceId === "string" && SAFE_ID.test(workspaceId) ? { workspaceId } : {}),
  };
}
