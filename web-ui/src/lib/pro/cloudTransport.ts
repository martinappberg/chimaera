import { api } from "../net/api";
import { isNativeShell, proCloudRequest, type CloudSetupInfo, type CloudSetupRequest } from "../net/native";
import { CLOUD_ASLEEP, WAKE_BOUND_MS, cloudAsleep } from "./presentation";

/** All passive operations stay GETs without wake intent. Provider connection is
 * the user's explicit authorization to prepare/wake the cloud. */
/** Whether this daemon is a cloud machine: a passive read that never wakes one.
 * Any other daemon answers `available: false`. */
export async function isCloudMachine(signal?: AbortSignal): Promise<boolean> {
  return (await cloudRequest({ operation: "info" }, signal)).available === true;
}

/** An action the user took (Connect, Disconnect): the press itself wakes the
 * cloud. While the cloud answers that it is still coming up (`cloud_asleep`)
 * the same request is sent again after a short pause, starting no new attempt
 * later than `WAKE_BOUND_MS` after the press; then that answer stands and the
 * caller shows its usual failure. Null once `wanted` says the press no longer
 * matters (a newer action, a closed page). */
export async function cloudAction(request: CloudSetupRequest, wanted: () => boolean = () => true, pauseMs = 3000): Promise<CloudSetupInfo | null> {
  const started = Date.now();
  for (;;) {
    try { return await cloudRequest(request); }
    catch (cause) { if (!cloudAsleep(cause) || Date.now() - started >= WAKE_BOUND_MS) throw cause; }
    await new Promise(resolve => setTimeout(resolve, pauseMs));
    if (!wanted()) return null;
  }
}

export async function cloudRequest(request: CloudSetupRequest, signal?: AbortSignal): Promise<CloudSetupInfo> {
  if (isNativeShell()) {
    const result = await proCloudRequest(request);
    signal?.throwIfAborted();
    return result;
  }
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
    case "open_provider_terminal": {
      const result = await cloudRequest({ operation: "provider_connection", connection_id: request.connection_id }, signal);
      const action = result.connection?.action;
      if (action?.type !== "terminal") throw new Error("The sign-in terminal is no longer available.");
      window.dispatchEvent(new CustomEvent("chimaera:provider-terminal", { detail: { workspaceId: action.workspace_id, sessionId: action.session_id } }));
      return result;
    }
    case "open_provider_browser": throw new Error("Use the provider's secure sign-in link.");
    case "project": path = "/pro/cloud/project"; method = "POST"; body = request; break;
  }
  const headers: Record<string, string> = {};
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
