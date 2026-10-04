/** Fixed daemon operations on the existing window API owner; no retries or caller paths. */
import { api } from "../net/api";
import type { CloudSetupInfo, CloudSetupRequest, MirrorStatus } from "../net/native";
import { CLOUD_ASLEEP } from "../pro/presentation";
export async function legacyCloudRequest(request: CloudSetupRequest, signal?: AbortSignal, expectedAccountLifetime?: string): Promise<CloudSetupInfo> {
  let path: string;
  let method = "GET";
  let body: unknown;
  switch (request.operation) {
    case "info": case "start": path = "/pro/cloud"; break;
    case "providers": path = "/pro/cloud/providers"; break;
    case "provider_connect": path = `/pro/cloud/providers/${encodeURIComponent(request.provider_id)}/connect`; method = "POST"; body = {}; break;
    case "provider_disconnect": path = `/pro/cloud/providers/${encodeURIComponent(request.provider_id)}/disconnect`; method = "POST"; body = { acknowledge_cloud_work: request.acknowledge_cloud_work }; break;
    case "provider_connection": path = `/pro/cloud/connections/${encodeURIComponent(request.connection_id)}`; break;
    case "provider_submit": path = `/pro/cloud/connections/${encodeURIComponent(request.connection_id)}/input`; method = "POST"; body = { code: request.code }; break;
    case "provider_cancel": path = `/pro/cloud/connections/${encodeURIComponent(request.connection_id)}/cancel`; method = "POST"; body = {}; break;
    case "resume_handoff": path = "/pro/hydrate"; method = "POST"; body = { workspace_id: request.workspace_id, expected_epoch: request.expected_epoch, requires_fork: false }; break;
    case "open_provider_browser": throw new Error("Use the provider's secure sign-in link.");
    case "project": path = "/pro/cloud/project"; method = "POST"; body = request; break;
    // Anything else (an older page's request to open a window on the cloud,
    // say) never reaches it: the cloud's own page never opens from here.
    default: throw new Error("This cloud operation isn't available.");
  }
  const headers: Record<string, string> = {};
  if (expectedAccountLifetime !== undefined) {
    if (!/^[0-9a-f]{64}$/.test(expectedAccountLifetime)) throw new Error("account_changed");
    headers["X-Chimaera-Account-Lifetime"] = expectedAccountLifetime;
  }
  if (method !== "GET" || request.operation === "start") headers["X-Chimaera-Wake"] = "interaction";
  if (body !== undefined) headers["Content-Type"] = "application/json";
  const timeout = AbortSignal.timeout(request.operation === "project" ? 310_000 : request.operation === "resume_handoff" ? 1_200_000 : method === "GET" && request.operation !== "start" ? 40_000 : 95_000);
  const response = await api(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body), signal: signal ? AbortSignal.any([signal, timeout]) : timeout });
  // A cloud machine asleep or still starting answers through its transport:
  // 503 `worker_asleep`/`worker_unavailable`, or a reply marked sleeping (a
  // cache answer, never live setup data). A state the page shows quietly.
  const sleeping = response.headers.get("x-chimaera-worker-state") === "sleeping";
  if (!response.ok || sleeping) {
    // Only these bounded codes are safe to carry through from the daemon.
    // Vendor output and raw error details must never reach the connection panel.
    const details = response.ok ? null : await response.json().catch(() => null) as { error?: unknown } | null;
    if (sleeping || response.status === 503 && (details?.error === "worker_asleep" || details?.error === "worker_unavailable")) throw new Error(CLOUD_ASLEEP);
    if (request.operation === "provider_disconnect" && response.status === 409 && details?.error === "provider_busy") throw new Error("provider_busy");
    // A half-pasted code leaves the sign-in waiting, so it can be pasted again.
    if (request.operation === "provider_submit" && response.status === 409 && details?.error === "authorization_code_incomplete") throw new Error("authorization_code_incomplete");
    throw new Error("This cloud operation couldn't finish. Please try again.");
  }
  return response.status === 204 ? {} : await response.json() as CloudSetupInfo;
}

export async function readBrowserMirrorStatus(signal: AbortSignal | undefined, lifetime: string): Promise<MirrorStatus | null> {
  if (!/^[a-f0-9]{64}$/.test(lifetime)) throw new Error("account_changed");
  const timeout = AbortSignal.timeout(20_000);
  const response = await api("/pro/status", { signal: signal ? AbortSignal.any([signal, timeout]) : timeout, headers: { "X-Chimaera-Account-Lifetime": lifetime } });
  if (response.status === 409) throw new Error("account_changed");
  if (!response.ok) return null;
  return await response.json() as MirrorStatus;
}
