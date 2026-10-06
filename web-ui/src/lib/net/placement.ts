import { writable, type Readable } from "svelte/store";
import { gatewayPrefix, gatewayWorkspace } from "./base";
import { ownerAwake } from "./reconnect";
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

/** What a view knows about the owner beyond its row: its socket heard the
 *  owner is asleep (`worker_asleep`) or waking (`waking`), and whether the
 *  view already says "Reconnecting…" itself. */
export interface OwnerNote {
  /** What the view's socket heard; only `asleep` and `waking` change the
   *  label (work being brought here says so in the view itself). */
  owner?: "asleep" | "waking" | "bringing" | null;
  /** The view's own status line already says it is reconnecting: the label
   *  must not say it a second time. */
  reconnectingShown?: boolean;
  /** The view's own socket to the owner is answered, or open and kept for it
   *  (by a keeper that keeps a sleeping cloud machine's sockets): the row's
   *  failed roster read is not this view reconnecting. */
  reachable?: boolean;
}

/** Where a routed session runs, in plain words; null for a session here.
 *  `available: false` means its owner cannot be reached right now — which is
 *  also what an idle cloud looks like to the daemon's passive roster read,
 *  so a view that heard the cloud is idle (or starting for a send) says only
 *  where it runs, never "reconnecting", and never a state name: the send in
 *  progress says itself. Same for one whose own socket is answered or kept. */
export function placementLabel(placement: unknown, available: boolean | undefined, note: OwnerNote = {}): string | null {
  if (typeof placement !== "object" || placement === null) return null;
  const remote = (placement as { remote?: unknown }).remote;
  const where = typeof remote === "string" && remote.startsWith("device-") ? ON_ANOTHER_COMPUTER : typeof remote === "string" && remote.startsWith("worker-") ? IN_THE_CLOUD : "Running elsewhere";
  if (note.owner === "asleep" || note.owner === "waking") return where;
  return available === false && note.reconnectingShown !== true && note.reachable !== true ? where + RECONNECTING : where;
}

/** Whether the shown project's conversations and terminals run in more than
 *  one place. When they all run in one, the window says that place once (the
 *  host indicator, or a browser view's strip) and per-session labels stay
 *  quiet; only a split project labels each session. The window sets it from
 *  its own session list ({@link notePlaces}). */
const placesSplitStore = writable(false);
export const placesSplit: Readable<boolean> = { subscribe: placesSplitStore.subscribe };
export function notePlaces(sessions: readonly { placement?: unknown }[]): void {
  const places = new Set(sessions.map(row => {
    const remote = typeof row.placement === "object" && row.placement !== null ? (row.placement as { remote?: unknown }).remote : undefined;
    return typeof remote === "string" ? remote : "here";
  }));
  placesSplitStore.set(places.size > 1);
}

/**
 * Why a session has no process where it is shown, as its socket (`moved` /
 * `paused` frames) and its paused row (`pause`, additive) both say it. Not an
 * exit: `moved` continues on another machine; `paused` resumes on its own.
 */
export type SessionPause =
  | { type: "moved"; to: MovedTo }
  | { type: "paused"; reason: string; provider: string | null };

/** Where a conversation continues after a move: the cloud, the user's
 *  computer, or ("other") another of the user's computers — acting there
 *  brought the work there. On the wire the last is `to:"computer"` with the
 *  additive `other:true`. */
export type MovedTo = "cloud" | "computer" | "other" | "elsewhere";
export function movedTo(frame: { to?: unknown; other?: unknown }): MovedTo {
  if (frame.to === "cloud") return "cloud";
  if (frame.to !== "computer") return "elsewhere";
  return frame.other === true ? "other" : "computer";
}

/** Parse a `moved`/`paused` frame or a row's `pause` field; null otherwise. */
export function parsePause(value: unknown): SessionPause | null {
  if (typeof value !== "object" || value === null) return null;
  const frame = value as Record<string, unknown>;
  if (frame.type === "moved") return { type: "moved", to: movedTo(frame) };
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
    if (pause.to === "other") return { status: "Continuing on your other computer…", detail: null };
    if (pause.to === "computer") return { status: "Continuing on your computer…", detail: null };
    if (pause.to === "elsewhere") return elsewherePause(signedOut);
    return signedOut
      ? { status: "This conversation is in the cloud. Sign in to Chimaera Pro to bring it back.", detail: null }
      : { status: "Continuing in the cloud…", detail: null };
  }
  switch (pause.reason) {
    case "elsewhere":
      return elsewherePause(signedOut);
    case "restarting":
      // A daemon restart of any cause (an update, a crash, a reboot after
      // the battery ran out): say what happens next, not why.
      return { status: "Picking up where you left off…", detail: null };
    case "needs_provider": {
      // The catalog name, the same one the connect action and Pro use.
      const name = pause.provider === null ? "the agent" : providerLabel(pause.provider);
      return { status: `Waiting for ${name} in your cloud`, detail: `Connect ${name} in Chimaera Pro, and this continues.` };
    }
    case "stays_on_computer":
      return { status: "This terminal stays on your computer", detail: "It opens again when the project is back on your computer." };
    default:
      return { status: "Opening…", detail: null };
  }
}

function elsewherePause(signedOut: boolean): { status: string; detail: null } {
  return { status: signedOut ? "This conversation is elsewhere. Sign in to Chimaera Pro to view it." : "Continuing elsewhere…", detail: null };
}

export class PlacementError extends Error {
  constructor(readonly status: number) { super("This project can’t be reached right now. That wasn’t sent."); }
}
let pending: { workspace: string; context: object; promise: Promise<WorkspacePlacement> } | null = null;
export interface PlacementOwner {
  readonly workspace_id: string; readonly epoch: number;
  readonly holder_id: string | null; readonly route_host_id: string | null;
  current(): boolean;
}
let placementContext: object = {};
let currentPlacement: PlacementOwner | null = null;
const placementOwnerStore = writable<PlacementOwner | null>(null);
export const placementOwner: Readable<PlacementOwner | null> = { subscribe: placementOwnerStore.subscribe };
/** Auth retirement cannot let an older coalesced request publish a successor. */
export function invalidatePlacementOwner(): void {
  // Retire publication, not the actual request slot. A successor cannot lose
  // accounting for a predecessor still reading its bounded response body.
  placementContext = {}; currentPlacement = null; placementOwnerStore.set(null);
}
function samePlacement(a: Pick<PlacementOwner, "workspace_id" | "epoch" | "holder_id" | "route_host_id">, b: WorkspacePlacement): boolean {
  return a.workspace_id === b.workspace_id && a.epoch === b.epoch && a.holder_id === b.holder_id && a.route_host_id === b.route_host_id;
}
function admitPlacement(row: WorkspacePlacement): void {
  if (currentPlacement !== null && samePlacement(currentPlacement, row)) return;
  const context = placementContext;
  const original: PlacementOwner = Object.freeze({ workspace_id: row.workspace_id, epoch: row.epoch,
    holder_id: row.holder_id, route_host_id: row.route_host_id,
    current: () => placementContext === context && currentPlacement === original });
  currentPlacement = original; placementOwnerStore.set(original);
}

/** Where this browser view's project runs, and whether it is idle in the
 *  cloud (routing and reconnecting read it; the words never say it). */
export interface ProjectWhere {
  where: "cloud" | "computer";
  asleep: boolean;
}

/** Where this browser view's project runs, from the latest placement read
 *  (every action and socket authentication reads it); null until the first
 *  answer and outside a project view. Presentation only — routing always
 *  reads placement afresh. */
const projectWhereStore = writable<ProjectWhere | null>(null);
export const projectWhere: Readable<ProjectWhere | null> = { subscribe: projectWhereStore.subscribe };
let lastSuspended = false;
function noteProjectWhere(placement: WorkspacePlacement): void {
  const asleep = placement.availability === "suspended";
  const where = placement.route_host_id?.startsWith("worker-") ? "cloud" : "computer";
  projectWhereStore.update((now) => (now?.where === where && now.asleep === asleep ? now : { where, asleep }));
  lastSuspended = asleep;
  // The owner answers again: sockets parked while it slept dial once.
  if (!asleep) ownerAwake();
}

/** The latest placement read of this project view said its owner is asleep
 *  (`suspended`). A socket that drops meanwhile parks instead of retrying. */
export function ownerSuspended(): boolean {
  return gatewayWorkspace() !== null && lastSuspended;
}

/** Whether a viewed conversation's owner is a cloud machine: a routed row
 *  names its host; a browser view of a project knows from its latest
 *  placement read (`project`); a browser view of one host names it in its
 *  path. False for a conversation on this computer. */
export function ownerIsCloud(placement: unknown, project: ProjectWhere | null): boolean {
  if (typeof placement === "object" && placement !== null) {
    const remote = (placement as { remote?: unknown }).remote;
    return typeof remote === "string" && remote.startsWith("worker-");
  }
  if (gatewayWorkspace() !== null) return project?.where === "cloud";
  return gatewayPrefix().startsWith("/app/worker-");
}

/** A project view's place in plain words for its status strip and Home:
 *  it follows the project, so it names where the project runs now. An idle
 *  cloud is still the cloud: a send starts it and says so itself. */
export function projectWhereLabel(project: ProjectWhere | null): string {
  if (project === null) return "This project";
  return project.where === "cloud" ? IN_THE_CLOUD : "On your computer";
}

/** Coalesce simultaneous reads; every later request checks the owner again. */
export function readPlacement(): Promise<WorkspacePlacement> {
  const workspace = gatewayWorkspace();
  if (workspace === null) return Promise.reject(new PlacementError(409));
  if (pending !== null) {
    if (pending.workspace === workspace && pending.context === placementContext) return pending.promise;
    return Promise.reject(new PlacementError(503));
  }
  const context = placementContext;
  const promise = (async () => {
    const response = await fetch(`${gatewayPrefix()}/placement`, { cache: "no-store", redirect: "error", signal: AbortSignal.timeout(2500) });
    const refuse = async (status: number): Promise<never> => {
      // Headers do not prove network/body cleanup. Keep the original slot
      // until cancellation settles, including retired and failed responses.
      try { await response.body?.cancel(); } catch { /* Preserve the fixed original refusal. */ }
      throw new PlacementError(status);
    };
    if (placementContext !== context) return refuse(409);
    if (!response.ok) return refuse(response.status);
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
    if (placementContext !== context) throw new PlacementError(409);
    let placement: WorkspacePlacement;
    try { placement = parsePlacement(JSON.parse(new TextDecoder().decode(bytes)), workspace); }
    catch (error) { currentPlacement = null; placementOwnerStore.set(null); throw error; }
    // A sleeping owner is routed like an awake one; the transport wakes it
    // for a request or socket that carries wake intent, never for a read.
    if (placement.availability !== "owned" && placement.availability !== "suspended") {
      currentPlacement = null; placementOwnerStore.set(null); throw new PlacementError(503);
    }
    admitPlacement(placement);
    noteProjectWhere(placement);
    return placement;
  })();
  pending = { workspace, context, promise };
  void promise.finally(() => { if (pending?.promise === promise) pending = null; }).catch(() => {});
  return promise;
}

export async function workspaceHeaders(headers: Headers, expected?: PlacementOwner): Promise<void> {
  if (gatewayWorkspace() === null) return;
  const placement = await readPlacement();
  if (expected !== undefined && (!expected.current() || !samePlacement(expected, placement))) throw new PlacementError(409);
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

/** A refused send or keystroke in place words. Holders between this view and
 *  the project (the account's relay, the cloud's front door) answer with
 *  their own sentences, which name machinery ("waking the cloud machine");
 *  the closed `reason` they carry picks the words here instead. Unknown
 *  reasons keep the holder's sentence. */
export function refusalWords(message: string, reason: string | null): string {
  switch (reason) {
    case "waking": return "That wasn’t sent yet. Send it again in a moment.";
    case "bringing": return "That wasn’t sent while this conversation moves here. Send it again in a moment.";
    case "reconnecting": return "That wasn’t sent: this project can’t be reached right now.";
    default: return message;
  }
}
