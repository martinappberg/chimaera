import { gatewayPrefix, gatewayWorkspace } from "./base";

export interface WorkspacePlacement {
  workspace_id: string;
  holder_id: string | null;
  route_host_id: string | null;
  epoch: number;
  policy_revision: number;
  availability: "owned" | "unowned" | "expired" | "privacy_disabled";
  server_now: string;
  expires_at: string | null;
}
const safeId = (value: unknown): value is string => typeof value === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(value);
export function parsePlacement(value: unknown, workspace: string): WorkspacePlacement {
  if (typeof value !== "object" || value === null) throw new Error("Project connection is unavailable");
  const row = value as Record<string, unknown>;
  if (!safeId(workspace) || row.workspace_id !== workspace || !Number.isSafeInteger(row.epoch) || (row.epoch as number) < 0 || !Number.isSafeInteger(row.policy_revision) || (row.policy_revision as number) < 0 || typeof row.server_now !== "string" || !Number.isFinite(Date.parse(row.server_now))) throw new Error("Project connection is unavailable");
  if (!["owned", "unowned", "expired", "privacy_disabled"].includes(String(row.availability))) throw new Error("Project connection is unavailable");
  if (row.availability === "owned") {
    if (!safeId(row.holder_id) || !safeId(row.route_host_id) || !["device-", "worker-"].some(prefix => row.route_host_id === prefix + row.holder_id) || (row.epoch as number) < 1 || typeof row.expires_at !== "string" || !Number.isFinite(Date.parse(row.expires_at)) || Date.parse(row.expires_at) <= Date.parse(row.server_now)) throw new Error("Project connection is unavailable");
  } else if (row.route_host_id !== null) throw new Error("Project connection is unavailable");
  return row as unknown as WorkspacePlacement;
}

/** Where a routed session runs, in plain words; null for a session here.
 *  `available: false` means its owner cannot be reached right now. */
export function placementLabel(placement: unknown, available: boolean | undefined): string | null {
  if (typeof placement !== "object" || placement === null) return null;
  const remote = (placement as { remote?: unknown }).remote;
  const where = typeof remote === "string" && remote.startsWith("device-") ? "On another computer" : "In the cloud";
  return available === false ? `${where} · reconnecting` : where;
}

export class PlacementError extends Error {
  constructor(readonly status: number) { super("Your project is reconnecting. This action was not sent."); }
}
let pending: { workspace: string; promise: Promise<WorkspacePlacement> } | null = null;
/** Coalesce simultaneous reads; every later request checks the owner again. */
export function readPlacement(): Promise<WorkspacePlacement> {
  const workspace = gatewayWorkspace();
  if (workspace === null) return Promise.reject(new PlacementError(409));
  if (pending?.workspace === workspace) return pending.promise;
  const promise = (async () => {
    const response = await fetch(`${gatewayPrefix()}/placement`, { cache: "no-store", redirect: "error", signal: AbortSignal.timeout(2500) });
    if (!response.ok) throw new PlacementError(response.status);
    const reader = response.body?.getReader();
    if (!reader) throw new PlacementError(503);
    const chunks: Uint8Array[] = [];
    let size = 0;
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.length;
      if (size > 16384) { await reader.cancel(); throw new PlacementError(503); }
      chunks.push(value);
    }
    const bytes = new Uint8Array(size);
    let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
    const placement = parsePlacement(JSON.parse(new TextDecoder().decode(bytes)), workspace);
    if (placement.availability !== "owned") throw new PlacementError(503);
    return placement;
  })();
  pending = { workspace, promise };
  void promise.finally(() => { if (pending?.promise === promise) pending = null; }).catch(() => {});
  return promise;
}

export async function workspaceHeaders(headers: Headers): Promise<void> {
  if (gatewayWorkspace() === null) return;
  const placement = await readPlacement();
  headers.set("X-Chimaera-Workspace", placement.workspace_id);
  headers.set("X-Chimaera-Epoch", String(placement.epoch));
  // A portable presentation alias: no real filesystem path leaves this tab.
  headers.set("X-Chimaera-Viewer-Root", "L3Byb2plY3Q");
}

/** The socket never queues an action before its scoped authentication frame. */
export function sendSocketAuth(socket: WebSocket, auth: Record<string, unknown>, current: () => boolean, sent: () => void = () => {}): void {
  const send = (scope: Record<string, unknown>): void => {
    if (!current() || socket.readyState !== WebSocket.OPEN) return;
    socket.send(JSON.stringify({ ...auth, ...scope }));
    sent();
  };
  if (gatewayWorkspace() === null) { send({}); return; }
  void readPlacement().then(placement => send({ workspace_id: placement.workspace_id, epoch: placement.epoch, viewer_root: "L3Byb2plY3Q" }), () => {
    if (current()) socket.close(4000, "Project connection changed");
  });
}
