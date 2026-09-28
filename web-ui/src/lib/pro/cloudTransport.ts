import { api } from "../net/api";
import { isNativeShell, proCloudRequest, type CloudSetupInfo, type CloudSetupRequest } from "../net/native";

/** All passive operations stay GETs without wake intent. Provider connection is
 * the user's explicit authorization to prepare/wake the cloud machine. */
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
    case "onboard": path = "/pro/cloud/onboard"; method = "POST"; body = request; break;
    case "project": path = "/pro/cloud/project"; method = "POST"; body = request; break;
  }
  const headers: Record<string, string> = {};
  if (method !== "GET" || request.operation === "start") headers["X-Chimaera-Wake"] = "interaction";
  if (body !== undefined) headers["Content-Type"] = "application/json";
  const timeout = AbortSignal.timeout(request.operation === "project" ? 310_000 : request.operation === "resume_handoff" ? 1_200_000 : method === "GET" && request.operation !== "start" ? 40_000 : 95_000);
  const response = await api(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body), signal: signal ? AbortSignal.any([signal, timeout]) : timeout });
  if (!response.ok) throw new Error("This cloud operation couldn't finish. Please try again.");
  return response.status === 204 ? {} : await response.json() as CloudSetupInfo;
}
