import { writable, type Readable } from "svelte/store";
import { gatewayPrefix, gatewayWorkspace } from "./base";
import { providerLabel } from "../pro/providers";

export interface WorkspacePlacement {
  workspace_id: string;
  holder_id: string | null;
  route_host_id: string | null;
  epoch: number;
  policy_revision: number;
  /** `suspended`: a cloud machine that went to sleep keeping ownership. It
   *  keeps its `worker-` route and is routed exactly like `owned`; its lease
   *  reads expired by design. Reading it never wakes it; the first send or
   *  keystroke carries wake intent. */
  availability: "owned" | "suspended" | "unowned" | "expired" | "privacy_disabled";
  server_now: string;
  expires_at: string | null;
}
const safeId = (value: unknown): value is string => typeof value === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(value);
export function parsePlacement(value: unknown, workspace: string): WorkspacePlacement {
  if (typeof value !== "object" || value === null) throw new Error("Project connection is unavailable");
  const row = value as Record<string, unknown>;
  if (!safeId(workspace) || row.workspace_id !== workspace || !Number.isSafeInteger(row.epoch) || (row.epoch as number) < 0 || !Number.isSafeInteger(row.policy_revision) || (row.policy_revision as number) < 0 || typeof row.server_now !== "string" || !Number.isFinite(Date.parse(row.server_now))) throw new Error("Project connection is unavailable");
  if (!["owned", "suspended", "unowned", "expired", "privacy_disabled"].includes(String(row.availability))) throw new Error("Project connection is unavailable");
  if (row.availability === "owned") {
    if (!safeId(row.holder_id) || !safeId(row.route_host_id) || !["device-", "worker-"].some(prefix => row.route_host_id === prefix + row.holder_id) || (row.epoch as number) < 1 || typeof row.expires_at !== "string" || !Number.isFinite(Date.parse(row.expires_at)) || Date.parse(row.expires_at) <= Date.parse(row.server_now)) throw new Error("Project connection is unavailable");
  } else if (row.availability === "suspended") {
    // A sleeping cloud machine: its own worker route, a live epoch, and a
    // lease that may read expired (it is not renewed while asleep).
    if (!safeId(row.holder_id) || row.route_host_id !== `worker-${row.holder_id}` || (row.epoch as number) < 1 || (row.expires_at !== null && (typeof row.expires_at !== "string" || !Number.isFinite(Date.parse(row.expires_at))))) throw new Error("Project connection is unavailable");
  } else if (row.route_host_id !== null) throw new Error("Project connection is unavailable");
  return row as unknown as WorkspacePlacement;
}

const IN_THE_CLOUD = "In the cloud";
const ON_ANOTHER_COMPUTER = "On another computer";
const RECONNECTING = " · reconnecting";
const ASLEEP = " · asleep";
const WAKING = " · waking";

/** What a view knows about the owner beyond its row: its socket heard the
 *  owner is asleep (`worker_asleep`) or waking (`waking`), and whether the
 *  view already says "Reconnecting…" itself. */
export interface OwnerNote {
  owner?: "asleep" | "waking" | null;
  /** The view's own status line already says it is reconnecting: the label
   *  must not say it a second time. */
  reconnectingShown?: boolean;
}

/** Where a routed session runs, in plain words; null for a session here.
 *  `available: false` means its owner cannot be reached right now — which is
 *  also what a sleeping owner looks like to the daemon's passive roster read,
 *  so a view that heard the owner is asleep (or waking) says that instead of
 *  "reconnecting". */
export function placementLabel(placement: unknown, available: boolean | undefined, note: OwnerNote = {}): string | null {
  if (typeof placement !== "object" || placement === null) return null;
  const remote = (placement as { remote?: unknown }).remote;
  const where = typeof remote === "string" && remote.startsWith("device-") ? ON_ANOTHER_COMPUTER : IN_THE_CLOUD;
  if (note.owner === "asleep") return where + ASLEEP;
  if (note.owner === "waking") return where + WAKING;
  return available === false && note.reconnectingShown !== true ? where + RECONNECTING : where;
}

/**
 * Why a session has no process where it is shown, as its socket (`moved` /
 * `paused` frames) and its paused row (`pause`, additive) both say it. Not an
 * exit: `moved` continues on another machine; `paused` resumes on its own.
 */
export type SessionPause =
  | { type: "moved"; to: "cloud" | "computer" }
  | { type: "paused"; reason: string; provider: string | null };

/** Parse a `moved`/`paused` frame or a row's `pause` field; null otherwise. */
export function parsePause(value: unknown): SessionPause | null {
  if (typeof value !== "object" || value === null) return null;
  const frame = value as Record<string, unknown>;
  if (frame.type === "moved") return { type: "moved", to: frame.to === "computer" ? "computer" : "cloud" };
  if (frame.type === "paused" && typeof frame.reason === "string") {
    return { type: "paused", reason: frame.reason, provider: typeof frame.provider === "string" ? frame.provider : null };
  }
  return null;
}

/** A paused session row's reason (the daemon's additive `pause` field). */
export function sessionPause(row: unknown): SessionPause | null {
  return typeof row === "object" && row !== null ? parsePause((row as { pause?: unknown }).pause) : null;
}

/** A paused session, in plain words: a one-line status and, when the person
 *  can do something about it, what. `signedOut`: this computer's account is
 *  signed out, so a conversation the cloud still holds is not on its way
 *  anywhere — nothing is in progress, and only signing in brings it back. */
export function pauseLabel(pause: SessionPause | null, { signedOut = false }: { signedOut?: boolean } = {}): { status: string; detail: string | null } {
  if (pause === null) return { status: "Opening…", detail: null };
  if (pause.type === "moved") {
    if (pause.to === "computer") return { status: "Continuing on your computer…", detail: null };
    return signedOut
      ? { status: "This conversation is in the cloud. Sign in to Chimaera Pro to bring it back.", detail: null }
      : { status: "Continuing in the cloud…", detail: null };
  }
  switch (pause.reason) {
    case "restarting":
      // A daemon restart of any cause (an update, a crash, a reboot after
      // the battery ran out): say what happens next, not why.
      return { status: "Picking up where you left off…", detail: null };
    case "needs_provider": {
      // The catalog name, the same one the connect action and Pro use.
      const name = pause.provider === null ? "the agent" : providerLabel(pause.provider);
      return { status: `Waiting for ${name} in the cloud`, detail: `Sign in to ${name} there from Chimaera Pro, and this continues.` };
    }
    case "stays_on_computer":
      return { status: "This terminal stays on your computer", detail: "It opens again when the project is back on your computer." };
    default:
      return { status: "Opening…", detail: null };
  }
}

export class PlacementError extends Error {
  constructor(readonly status: number) { super("Your project is reconnecting. This action was not sent."); }
}
let pending: { workspace: string; promise: Promise<WorkspacePlacement> } | null = null;

/** Where this browser view's project runs, from the latest placement read
 *  (every action and socket authentication reads it); null until the first
 *  answer and outside a project view. Presentation only — routing always
 *  reads placement afresh. */
const projectWhereStore = writable<"cloud" | "computer" | null>(null);
export const projectWhere: Readable<"cloud" | "computer" | null> = { subscribe: projectWhereStore.subscribe };
function noteProjectWhere(placement: WorkspacePlacement): void {
  projectWhereStore.set(placement.route_host_id?.startsWith("worker-") ? "cloud" : "computer");
}

/** A project view's machine in plain words for its status strip and Home:
 *  it follows the project, so it names where the project runs now. */
export function projectWhereLabel(where: "cloud" | "computer" | null): string {
  return where === "cloud" ? IN_THE_CLOUD : where === "computer" ? "On your computer" : "This project";
}

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
    // A sleeping owner is routed like an awake one; the transport wakes it
    // for a request or socket that carries wake intent, never for a read.
    if (placement.availability !== "owned" && placement.availability !== "suspended") throw new PlacementError(503);
    noteProjectWhere(placement);
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
