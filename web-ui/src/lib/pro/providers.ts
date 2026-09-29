import type { CloudProviderConnection, CloudProviderStatus, CloudSetupInfo } from "../net/native";
import catalog from "../../../../crates/chimaera-core/src/cloud-providers.json";

export function pendingConnection(connection: CloudProviderConnection | null): boolean {
  return connection !== null && ["preparing", "waiting", "verifying"].includes(connection.phase);
}

export function disconnectConnection(connection: CloudProviderConnection | null): boolean {
  return connection?.operation === "disconnect";
}

export function sameConnection(expected: CloudProviderConnection, received: CloudProviderConnection | null | undefined): boolean {
  return received != null && received.id === expected.id && received.provider_id === expected.provider_id
    && (received.operation ?? "connect") === (expected.operation ?? "connect");
}

export function canDisconnect(provider: CloudProviderStatus): boolean {
  return provider.disconnect_supported === true && (provider.state === "signed_in" || provider.installed === true && provider.state === "unknown");
}

export function connectionSuccessCurrent(connection: CloudProviderConnection | null, providers: CloudProviderStatus[], fresh: boolean): boolean {
  if (!fresh || !connection) return false;
  const state = providers.find(provider => provider.id === connection.provider_id)?.state;
  return connection.phase === "connected" ? state === "signed_in"
    : connection.phase === "disconnected" && (state === "needs_sign_in" || state === "missing");
}

/** Catalog recovery must neither replace another pending job nor roll a known
 * terminal outcome back to a delayed in-flight snapshot of the same job. */
export function recoverDisconnect(current: CloudProviderConnection | null, observed: CloudProviderConnection | null | undefined): CloudProviderConnection | null {
  if (!observed || !disconnectConnection(observed)) return current;
  if (current && (current.id === observed.id ? !pendingConnection(current) : pendingConnection(current))) return current;
  return observed;
}

/** An expired local polling deadline is not proof that the server job finished. */
export function canStartConnection(connection: CloudProviderConnection | null, fresh: boolean): boolean {
  return fresh && !pendingConnection(connection);
}

/** Initial setup needs one agent; continuing a handoff needs every actual
 * provider used by its sessions. Installation and unknown auth never qualify. */
export function providersReady(providers: CloudProviderStatus[], required: string[] = []): boolean {
  const connected = new Set(providers.filter(p => p.category === "agent" && p.state === "signed_in").map(p => p.id));
  return required.length > 0 ? required.every(id => connected.has(id)) : connected.size > 0;
}

export type ProviderHandoff = NonNullable<CloudSetupInfo["handoffs"]>[number];
export function handoffKey(handoff: ProviderHandoff): string {
  return JSON.stringify([handoff.workspace_id, handoff.expected_epoch]);
}

/** Only an existing provider-blocked transfer may continue automatically. A
 * generic connected agent never authorizes a new move or an unscoped hydrate. */
export function nextReadyHandoff(providers: CloudProviderStatus[], handoffs: ProviderHandoff[], attempted: string[], current: boolean): ProviderHandoff | undefined {
  if (!current) return undefined;
  return handoffs.find(handoff => handoff.workspace_id.length > 0
    && Number.isSafeInteger(handoff.expected_epoch) && handoff.expected_epoch > 0
    && handoff.blocked_providers.length > 0
    && !attempted.includes(handoffKey(handoff))
    && providersReady(providers, handoff.blocked_providers.map(provider => provider.id)));
}

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

export function providerStateLabel(provider: CloudProviderStatus): string {
  switch (provider.state) {
    case "signed_in": return "Connected";
    case "needs_sign_in": return "Not connected";
    case "missing": return "Not connected";
    case "unavailable": return "Unavailable";
    default: return "Couldn't confirm connection";
  }
}

/** Browser clients enforce the same vendor destinations as the native bridge.
 * Adding a provider never grants arbitrary external navigation. */
export function providerLoginUrl(providerId: string, value: string): string | null {
  const origins = catalog.providers.find(provider => provider.id === providerId)?.auth_origins ?? [];
  if (value.length > 4096) return null;
  try {
    const url = new URL(value);
    return url.protocol === "https:" && !url.username && !url.password && !url.hash && (!url.port || url.port === "443") && origins.includes(url.origin) ? url.href : null;
  } catch { return null; }
}

export function connectionError(code: string | null, operation: "connect" | "disconnect" = "connect"): string {
  if (operation === "disconnect") {
    switch (code) {
      case "provider_busy": return "This service has another connection request in progress. Check its status before trying again.";
      case "external_auth_unverified": return "This connection uses access managed outside Chimaera. Chimaera couldn't confirm its removal.";
      case "disconnect_not_confirmed": return "The service still reports a connection. Check its status before trying again.";
      case "expired": case "connection_expired": return "This request timed out before disconnection could be confirmed. Check the connection before trying again.";
      case "invalid_status": return "The service couldn't confirm its connection status. Check again before trying to disconnect.";
      default: return "Disconnection couldn't be confirmed. Check the connection before trying again. Sign-in on your other devices hasn't changed.";
    }
  }
  switch (code) {
    case "provider_busy": return "This service has another connection request in progress. Check its status before trying again.";
    case "invalid_status": return "The service couldn't confirm its connection status. Check again before starting sign-in.";
    case "expired": case "connection_expired": return "This sign-in request expired. Start again for a fresh request.";
    case "canceled": case "connection_canceled": return "Sign-in was canceled. Your existing provider connections haven't changed.";
    case "device_login_unavailable": return "Device sign-in couldn't start. Check your connection and that your provider account allows device sign-in, then try again.";
    case "installation_failed": case "install_failed": return "Sign-in couldn’t be prepared for this agent. Try again in a moment.";
    case "installation_unavailable": return "Sign-in isn’t available for this agent right now. Try again later.";
    case "probe_timeout": return "The agent took too long to confirm sign-in. Check the connection again in a moment.";
    case "sign_in_not_confirmed": return "The provider hasn’t confirmed sign-in yet. Open its sign-in flow again to finish.";
    // Claude's page shows `code#state`; half of it keeps the sign-in waiting.
    case "authorization_code_incomplete": return "Paste the whole code, including the part after #.";
    case "browser_login_unavailable": return "Guided sign-in isn’t available for this agent right now. Try again later.";
    case "unsupported": case "unsupported_auth": return "This provider doesn’t support guided sign-in for cloud work yet.";
    default: return "The provider couldn't complete sign-in. You can try again without changing your local account.";
  }
}
