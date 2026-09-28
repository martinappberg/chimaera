import type { CloudProviderConnection, CloudProviderStatus, CloudSetupInfo } from "../net/native";
import catalog from "../../../../crates/chimaera-core/src/cloud-providers.json";

export function pendingConnection(connection: CloudProviderConnection | null): boolean {
  return connection !== null && ["preparing", "waiting", "verifying"].includes(connection.phase);
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

export function connectionError(code: string | null): string {
  switch (code) {
    case "expired": case "connection_expired": return "This sign-in request expired. Start again for a fresh request.";
    case "canceled": case "connection_canceled": return "Sign-in was canceled. Your existing provider connections haven't changed.";
    case "device_login_unavailable": return "Device sign-in couldn't start. Check your connection and that your provider account allows device sign-in, then try again.";
    case "device_auth_disabled": return "Device sign-in isn't enabled for this account. Enable it in your provider's security settings, then try again.";
    case "installation_failed": case "install_failed": return "Sign-in couldn’t be prepared for this agent. Try again in a moment.";
    case "installation_unavailable": return "Sign-in isn’t available for this agent right now. Try again later.";
    case "probe_timeout": return "The agent took too long to confirm sign-in. Check the connection again in a moment.";
    case "sign_in_not_confirmed": return "The provider hasn’t confirmed sign-in yet. Open its sign-in flow again to finish.";
    case "browser_login_unavailable": return "Guided sign-in isn’t available for this agent right now. Try again later.";
    case "unsupported": case "unsupported_auth": return "This provider doesn’t support guided sign-in for cloud work yet.";
    default: return "The provider couldn't complete sign-in. You can try again without changing your local account.";
  }
}
