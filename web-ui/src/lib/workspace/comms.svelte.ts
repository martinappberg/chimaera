/**
 * Client + reactive store for agent communication's per-workspace state
 * (docs/agent-communication-plan.md §12):
 *   GET  /workspaces/{id}/comms                {enabled, wakes, unread, wake_requests}
 *   POST /workspaces/{id}/comms/wakes/{wid}    {wake}     answer a wake request
 *   POST /workspaces/{id}/comms/deliver        {session}  hand an inbox over
 *
 * The timeline discipline: state is pulled, never pushed. `/ws/events`
 * carries a `{type:"comms", epochs}` nudge (every workspace's epoch in one
 * frame); the store mirrors ONLY the active workspace and refetches when that
 * workspace's epoch moved — deferred while the document is hidden, and again
 * after the events socket reconnects (a restarted daemon renumbers epochs and
 * sends no frame until something changes). Hot state is a runes class because
 * the dashboard's cards, the Needs-you lane and the Mastermind panel read the
 * same counts.
 */
import { api, ApiError } from "../net/api";

export type WakePolicy = "never" | "ask" | "auto";

/** A message that would start a turn in an idle agent, waiting on the user
 *  (the "Ask me" policy), or a thread that hit its hop limit. */
export interface WakeRequest {
  id: string;
  to_sid: string;
  to_name: string;
  from_sid: string;
  from_name: string;
  /** The newest message seq the request covers. */
  message: number;
  /** That message's text (the card shows one clamped line). */
  text: string;
  /** "ask" | "hop_limit", verbatim; anything else reads as "ask". */
  reason: string;
  created_ms: number;
}

export interface CommsSnapshot {
  enabled: boolean;
  wakes: WakePolicy;
  /** Unread messages per session id (absent = none). */
  unread: Record<string, number>;
  wake_requests: WakeRequest[];
}

const str = (v: unknown, fallback = ""): string => (typeof v === "string" ? v : fallback);
const num = (v: unknown): number => (typeof v === "number" && Number.isFinite(v) ? v : 0);

/** Defensive parse (the compute.ts idiom): malformed rows are dropped, never
 *  allowed to blank the surface; wake requests are de-duplicated by id
 *  because they key a rendered list (a repeated key throws). */
export function parseComms(raw: unknown): CommsSnapshot {
  const r = typeof raw === "object" && raw !== null ? (raw as Record<string, unknown>) : {};
  const unread: Record<string, number> = {};
  if (typeof r.unread === "object" && r.unread !== null) {
    for (const [sid, n] of Object.entries(r.unread as Record<string, unknown>)) {
      if (typeof n === "number" && Number.isFinite(n) && n > 0) unread[sid] = Math.floor(n);
    }
  }
  const seen = new Set<string>();
  const wakeRequests: WakeRequest[] = [];
  for (const w of Array.isArray(r.wake_requests) ? r.wake_requests : []) {
    if (typeof w !== "object" || w === null) continue;
    const o = w as Record<string, unknown>;
    const id = str(o.id);
    if (id === "" || seen.has(id) || str(o.to_sid) === "") continue;
    seen.add(id);
    wakeRequests.push({
      id,
      to_sid: str(o.to_sid),
      to_name: str(o.to_name, str(o.to_sid)),
      from_sid: str(o.from_sid),
      from_name: str(o.from_name, str(o.from_sid, "an agent")),
      message: num(o.message),
      text: str(o.text),
      reason: str(o.reason, "ask"),
      created_ms: num(o.created_ms),
    });
  }
  const wakes = r.wakes === "never" || r.wakes === "auto" ? r.wakes : "ask";
  return { enabled: r.enabled !== false, wakes, unread, wake_requests: wakeRequests };
}

async function checked(res: Response): Promise<Response> {
  if (res.ok) return res;
  let message = `request failed with status ${res.status}`;
  try {
    const body = (await res.json()) as { error?: string };
    if (body.error) message = body.error;
  } catch {
    // non-JSON error body; keep the generic message
  }
  throw new ApiError(res.status, message);
}

const route = (wsId: string, rest = "") => `/workspaces/${encodeURIComponent(wsId)}/comms${rest}`;

/** A hung fetch must not stall the refetch chain (the poll rule). */
const FETCH_TIMEOUT_MS = 10_000;

export async function fetchComms(wsId: string): Promise<CommsSnapshot> {
  const res = await checked(await api(route(wsId), { signal: AbortSignal.timeout(FETCH_TIMEOUT_MS) }));
  return parseComms(await res.json());
}

function post(path: string, body: unknown): Promise<Response> {
  return api(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  }).then(checked);
}

// ---- the reactive store (active workspace only) --------------------------------

class CommsStore {
  wsId = $state<string | null>(null);
  /** null until the first fetch settles; false = the daemon has no comms
   *  route — surfaces show nothing rather than zeros they can't vouch for. */
  available = $state<boolean | null>(null);
  /** The daemon's view of the setting (the settings store is the live one). */
  enabled = $state(true);
  wakes = $state<WakePolicy>("ask");
  unread = $state<Record<string, number>>({});
  wakeRequests = $state<WakeRequest[]>([]);
  /** Answers the daemon refused (the agent is gone, the setting went off).
   *  The daemon drops a request whatever the answer, so the lane keeps these
   *  itself until dismissed — the click's outcome must not just vanish. */
  wakeFailures = $state<{ request: WakeRequest; message: string }[]>([]);
  /** The daemon's own words for a failed fetch, or null. */
  error = $state<string | null>(null);
}

export const commsStore = new CommsStore();

/** Unread messages waiting for `sid` (0 when none, or nothing is known). */
export function unreadFor(sid: string | null | undefined): number {
  if (sid === null || sid === undefined) return 0;
  return commsStore.unread[sid] ?? 0;
}

const lastEpoch = new Map<string, number>();
/** A nudge (or a reconnect) came while nobody could see: refetch on return. */
let staleWhileHidden = false;
let inflight = false;
let again = false;

function hidden(): boolean {
  return typeof document !== "undefined" && document.visibilityState === "hidden";
}

if (typeof document !== "undefined") {
  // The store's own visibility catch-up (the compute.ts idiom).
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState !== "visible" || !staleWhileHidden) return;
    staleWhileHidden = false;
    void pull();
  });
}

function apply(snap: CommsSnapshot): void {
  commsStore.available = true;
  commsStore.error = null;
  commsStore.enabled = snap.enabled;
  commsStore.wakes = snap.wakes;
  commsStore.unread = snap.unread;
  commsStore.wakeRequests = snap.wake_requests;
}

/** Fetch the active workspace's state. One request at a time: nudges that
 *  land mid-flight collapse into one more pull, and a workspace switch
 *  mid-flight discards the stale answer and fetches the new one. */
async function pull(): Promise<void> {
  if (inflight) {
    again = true;
    return;
  }
  inflight = true;
  try {
    do {
      again = false;
      const ws = commsStore.wsId;
      if (ws === null) break;
      try {
        const snap = await fetchComms(ws);
        if (commsStore.wsId !== ws) {
          again = true;
          continue;
        }
        apply(snap);
      } catch (e) {
        if (commsStore.wsId !== ws) {
          again = true;
          continue;
        }
        if (e instanceof ApiError && e.status === 404) {
          commsStore.available = false;
          commsStore.error = null;
        } else {
          commsStore.error = e instanceof Error ? e.message : String(e);
        }
      }
    } while (again);
  } finally {
    inflight = false;
  }
}

/** Point the store at a workspace (or null) and load it. */
export function activateCommsWorkspace(wsId: string | null): void {
  if (wsId === commsStore.wsId) return;
  commsStore.wsId = wsId;
  commsStore.available = null;
  commsStore.error = null;
  commsStore.unread = {};
  commsStore.wakeRequests = [];
  commsStore.wakeFailures = [];
  staleWhileHidden = false;
  if (wsId !== null) void pull();
}

/** Handle a `{type:"comms", epochs}` frame: refetch iff the active
 *  workspace's epoch moved since the last one seen, deferred while hidden. */
export function onCommsNudge(epochs: Record<string, number>): void {
  const ws = commsStore.wsId;
  if (ws === null) return;
  const epoch = epochs[ws];
  if (typeof epoch !== "number" || epoch === lastEpoch.get(ws)) return;
  lastEpoch.set(ws, epoch);
  if (hidden()) {
    staleWhileHidden = true;
    return;
  }
  void pull();
}

/** The events socket came back: the daemon may have restarted (epochs start
 *  over, and no frame comes until something changes) or state moved while
 *  the socket was down — forget the epochs and refetch. */
export function onCommsReconnect(): void {
  lastEpoch.clear();
  if (commsStore.wsId === null) return;
  if (hidden()) {
    staleWhileHidden = true;
    return;
  }
  void pull();
}

/** Force a refetch of the active workspace (a surface becoming visible). */
export function refreshComms(): void {
  if (commsStore.wsId !== null) void pull();
}

/** Most refused answers the lane keeps on show. */
const FAILURES_MAX = 4;

/**
 * Answer a wake request: `true` delivers every unread message to its agent
 * as one message (a turn on the user's account), `false` leaves them in its
 * inbox. The daemon drops the request either way. A 404 means it was
 * already settled (another window, a policy change) — nothing to report.
 * Any other refusal (409: the agent is gone, or isn't a chat) is kept in
 * `wakeFailures` with the daemon's words, and thrown.
 */
export async function wake(wid: string, wake: boolean): Promise<void> {
  const ws = commsStore.wsId;
  if (ws === null) throw new Error("no workspace");
  const request = commsStore.wakeRequests.find((r) => r.id === wid);
  let failure: unknown = null;
  try {
    await post(route(ws, `/wakes/${encodeURIComponent(wid)}`), { wake });
  } catch (e) {
    if (!(e instanceof ApiError && e.status === 404)) failure = e;
  }
  if (commsStore.wsId === ws) {
    commsStore.wakeRequests = commsStore.wakeRequests.filter((r) => r.id !== wid);
    if (failure !== null && request !== undefined) {
      const message = failure instanceof Error ? failure.message : String(failure);
      commsStore.wakeFailures = [
        ...commsStore.wakeFailures.filter((f) => f.request.id !== wid),
        { request, message },
      ].slice(-FAILURES_MAX);
    }
  }
  void pull();
  if (failure !== null) throw failure;
}

/** Put away a refused answer's card. */
export function dismissWakeFailure(wid: string): void {
  commsStore.wakeFailures = commsStore.wakeFailures.filter((f) => f.request.id !== wid);
}

/** The user's hand-over of every unread message waiting for `session` (one
 *  message, one turn). Throws with the daemon's words on failure. */
export async function deliver(session: string): Promise<void> {
  const ws = commsStore.wsId;
  if (ws === null) throw new Error("no workspace");
  await post(route(ws, "/deliver"), { session });
  if (commsStore.wsId === ws && session in commsStore.unread) {
    const next = { ...commsStore.unread };
    delete next[session];
    commsStore.unread = next;
  }
  void pull();
}
