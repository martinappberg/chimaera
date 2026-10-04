/** Fixed profile transport on the window's original API owner. Proposal
 * decisions, retries and account-current policy live in the private controller. */
import { api } from "../net/api";
export type StoredProfile = Record<string, unknown>;
export type ProposalDecision = "confirm" | "dismiss";
function lifetimeHeaders(lifetime?: string): Record<string, string> {
  if (lifetime === undefined) return {};
  if (!/^[0-9a-f]{64}$/.test(lifetime)) throw new Error("account_changed");
  return { "X-Chimaera-Account-Lifetime": lifetime };
}
export function readSetupProfile(workspaceId: string, lifetime?: string): Promise<Response> {
  return api(`/pro/profile?workspace_id=${encodeURIComponent(workspaceId)}`, {
    cache: "no-store", signal: AbortSignal.timeout(15_000), headers: lifetimeHeaders(lifetime),
  });
}
export function saveSetupProfile(workspaceId: string, profile: StoredProfile, revision: string, lifetime?: string): Promise<Response> {
  return api(`/pro/profile?workspace_id=${encodeURIComponent(workspaceId)}`, {
    method: "PUT", headers: { "Content-Type": "application/json", "If-Match": revision, ...lifetimeHeaders(lifetime) },
    body: JSON.stringify(profile), signal: AbortSignal.timeout(35_000),
  });
}
