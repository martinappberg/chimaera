/**
 * Client + reactive store for the daemon's per-workspace Timeline:
 *   GET /workspaces/{id}/timeline?since=&before=&limit=   {schema, epoch, entries, more}
 *
 * The git.ts discipline: entries are pulled, never pushed. `/ws/events`
 * carries a tiny `{type:"timeline", epochs}` nudge per workspace; the store
 * mirrors ONLY the active workspace, pulls incrementally with `since=` on a
 * nudge, and pages older history with `before=` on demand. Hot state is a
 * runes class (the chat-store idiom) because the dashboard, the Timeline
 * view and the Mastermind dock all read the same live list.
 *
 * "Since you left" is per viewer, not daemon state: the last-seen seq per
 * workspace lives in localStorage (every access wrapped — private mode and
 * quota errors must never break the surface).
 */
import { api, ApiError } from "../net/api";

export type TimelineKind = "episode" | "command" | "job" | "session" | "knowledge" | "note";
export type EpisodeEnd = "finished" | "interrupted" | "errored" | "exited" | "waiting" | "unknown";
export type EpisodeTier = "protocol" | "hooks" | "output";

export interface TimelineRecorded {
  findings: string[];
  learnings: number;
  decisions: number;
  fps?: string[];
}

export interface TimelineEvidence {
  files: string[];
  files_n: number;
  tools: number;
  turns?: number;
  recorded?: TimelineRecorded;
}

export interface TimelineCommand {
  text: string;
  exit?: number;
  ms: number;
  source: "user" | "agent";
}

export interface TimelineJob {
  id: string;
  name: string;
  /** Slurm's own word (COMPLETED, FAILED, …) or ENDED — never relabeled. */
  state: string;
  elapsed?: string;
}

export interface TimelineKnowledge {
  change: "status" | "new";
  id: string;
  from?: string;
  to: string;
  claim: string;
}

export interface TimelineNote {
  from_sid: string;
  from_name: string;
  /** A session id, "mastermind", or absent (everyone). */
  to?: string;
  text: string;
  delivered?: boolean;
}

/** One wire entry (crates/chimaera-server/src/timeline.rs `Entry`); only the
 *  fields relevant to `kind` are present. An unknown kind still parses — it
 *  renders generically rather than blanking the list. */
export interface TimelineEntry {
  seq: number;
  ts: number;
  kind: TimelineKind | string;
  sid?: string;
  name?: string;
  agent?: string;
  ui?: "chat" | "term";
  tier?: EpisodeTier;
  title?: string;
  result?: string;
  end?: EpisodeEnd;
  via?: string;
  start_ts?: number;
  ms?: number;
  evidence?: TimelineEvidence;
  command?: TimelineCommand;
  job?: TimelineJob;
  knowledge?: TimelineKnowledge;
  note?: TimelineNote;
}

interface TimelinePage {
  schema: number;
  epoch: number;
  entries: TimelineEntry[];
  more: boolean;
}

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) {
    let message = `request failed with status ${res.status}`;
    try {
      const body = (await res.json()) as { error?: string };
      if (body.error) message = body.error;
    } catch {
      // non-JSON error body; keep the generic message
    }
    throw new ApiError(res.status, message);
  }
  return (await res.json()) as T;
}

/** Defensive entry parse (the compute.ts idiom): a malformed row is dropped,
 *  never allowed to blank the whole list. Optional sub-objects pass through
 *  typed — every consumer treats their fields as possibly absent. */
export function parseEntry(raw: unknown): TimelineEntry | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.seq !== "number" || typeof r.ts !== "number" || typeof r.kind !== "string") {
    return null;
  }
  return r as unknown as TimelineEntry;
}

export async function fetchTimeline(
  workspaceId: string,
  opts: { since?: number; before?: number; limit?: number } = {},
): Promise<TimelinePage> {
  const q = new URLSearchParams();
  if (opts.since !== undefined) q.set("since", String(opts.since));
  if (opts.before !== undefined) q.set("before", String(opts.before));
  if (opts.limit !== undefined) q.set("limit", String(opts.limit));
  const qs = q.toString();
  const page = await json<TimelinePage>(
    await api(`/workspaces/${encodeURIComponent(workspaceId)}/timeline${qs ? `?${qs}` : ""}`),
  );
  const entries = Array.isArray(page.entries)
    ? page.entries.map(parseEntry).filter((e): e is TimelineEntry => e !== null)
    : [];
  return {
    schema: typeof page.schema === "number" ? page.schema : 1,
    epoch: typeof page.epoch === "number" ? page.epoch : 0,
    entries,
    more: page.more === true,
  };
}

/** Agent notes: deliver a note to its addressee as a real, attributed message
 *  (the user's click starts that turn — never the post itself). */
export async function deliverNote(
  workspaceId: string,
  seq: number,
): Promise<{ session_id: string }> {
  return json(
    await api(`/workspaces/${encodeURIComponent(workspaceId)}/timeline/${seq}/deliver`, {
      method: "POST",
    }),
  );
}

// ---- the reactive store (active workspace only) --------------------------------

const PAGE = 100;

class TimelineStore {
  /** The workspace the list belongs to (null = none). */
  wsId = $state<string | null>(null);
  /** Newest first, gap-free between `oldestSeq` and `head`. */
  entries = $state<TimelineEntry[]>([]);
  /** Highest seq held (0 = nothing loaded). */
  head = $state(0);
  /** An older page exists past the oldest held entry. */
  more = $state(false);
  loading = $state(false);
  /** null until the first fetch settles; false = the daemon has no timeline
   *  route (predates the feature) — surfaces say so instead of spinning. */
  available = $state<boolean | null>(null);
  /** The daemon's own words for a failed fetch, or null. */
  error = $state<string | null>(null);
}

export const timelineStore = new TimelineStore();

let refreshSeq = 0;
const lastEpoch = new Map<string, number>();
/** A nudge arrived while the document was hidden: refetch on return. */
let staleWhileHidden = false;
/** The per-workspace timeline epoch this store last applied (invalidate-and-
 *  pull): a nudge refetches iff the active workspace's epoch moved. */
const listeners = new Set<() => void>();

/** Subscribe to "the active workspace's timeline changed" (the knowledge
 *  store refetches at episode ends off this signal). */
export function onTimelineChange(cb: () => void): () => void {
  listeners.add(cb);
  return () => listeners.delete(cb);
}

function notify(): void {
  for (const cb of listeners) cb();
}

function hidden(): boolean {
  return typeof document !== "undefined" && document.visibilityState === "hidden";
}

if (typeof document !== "undefined") {
  // The store owns its own visibility catch-up: a nudge that arrived while
  // nobody could see the list is honoured the moment someone can (the
  // compute.ts idiom — polls and pulls stay quiet in a hidden window).
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState !== "visible" || !staleWhileHidden) return;
    staleWhileHidden = false;
    if (timelineStore.wsId !== null) void pull(timelineStore.wsId);
  });
}

/** Point the store at a workspace (or null) and load its newest page. */
export async function activateTimelineWorkspace(wsId: string | null): Promise<void> {
  if (wsId === timelineStore.wsId) return;
  timelineStore.wsId = wsId;
  timelineStore.entries = [];
  timelineStore.head = 0;
  timelineStore.more = false;
  timelineStore.error = null;
  timelineStore.available = null;
  staleWhileHidden = false;
  if (wsId !== null) await pull(wsId);
}

/** Merge `incoming` (any order) into the held list: dedupe by seq, keep
 *  newest first. Always a fresh array (never an in-place mutation). */
function merge(held: TimelineEntry[], incoming: TimelineEntry[]): TimelineEntry[] {
  const bySeq = new Map<number, TimelineEntry>();
  for (const e of held) bySeq.set(e.seq, e);
  for (const e of incoming) bySeq.set(e.seq, e);
  return [...bySeq.values()].sort((a, b) => b.seq - a.seq);
}

/** Pull the newest entries: everything past `head` when we hold some, else
 *  the newest page. A `since=` pull that returns a full page means the gap
 *  outgrew one page — take it and mark `more` so the view can keep paging. */
async function pull(wsId: string): Promise<void> {
  const seq = ++refreshSeq;
  const since = timelineStore.head > 0 ? timelineStore.head : undefined;
  timelineStore.loading = true;
  try {
    const page = await fetchTimeline(wsId, since !== undefined ? { since, limit: PAGE } : { limit: PAGE });
    if (timelineStore.wsId !== wsId || seq !== refreshSeq) return;
    lastEpoch.set(wsId, page.epoch);
    timelineStore.available = true;
    timelineStore.error = null;
    const wasEmpty = timelineStore.entries.length === 0;
    if (page.entries.length > 0) {
      const merged = merge(timelineStore.entries, page.entries);
      timelineStore.entries = merged;
      timelineStore.head = merged[0]?.seq ?? 0;
    }
    if (wasEmpty || since === undefined) timelineStore.more = page.more;
    else if (page.more) timelineStore.more = true;
    // A journal reset (data dir wiped) shows as a head BELOW what a viewer
    // remembers — lastSeen() clamps against `head`, so nothing to do here.
    notify();
  } catch (e) {
    if (timelineStore.wsId !== wsId || seq !== refreshSeq) return;
    if (e instanceof ApiError && e.status === 404) {
      timelineStore.available = false;
      timelineStore.error = null;
    } else {
      timelineStore.error = e instanceof Error ? e.message : String(e);
    }
  } finally {
    if (timelineStore.wsId === wsId && seq === refreshSeq) timelineStore.loading = false;
  }
}

/**
 * Handle a `{type:"timeline"}` epoch frame: refetch iff the active
 * workspace's epoch moved since we last applied it (invalidate-and-pull),
 * deferred while the document is hidden.
 */
export function onTimelineNudge(epochs: Record<string, number>): void {
  const ws = timelineStore.wsId;
  if (ws === null) return;
  const epoch = epochs[ws];
  if (typeof epoch !== "number" || epoch === lastEpoch.get(ws)) return;
  if (hidden()) {
    staleWhileHidden = true;
    return;
  }
  void pull(ws);
}

/** Force a refresh of the active workspace (a view's manual refresh). */
export function refreshTimeline(): void {
  if (timelineStore.wsId !== null) void pull(timelineStore.wsId);
}

/** Page one step further into history (the Timeline view's "load older"). */
export async function loadOlderTimeline(): Promise<void> {
  const ws = timelineStore.wsId;
  if (ws === null || timelineStore.loading || !timelineStore.more) return;
  const oldest = timelineStore.entries[timelineStore.entries.length - 1];
  if (oldest === undefined) return;
  const seq = ++refreshSeq;
  timelineStore.loading = true;
  try {
    const page = await fetchTimeline(ws, { before: oldest.seq, limit: PAGE });
    if (timelineStore.wsId !== ws || seq !== refreshSeq) return;
    timelineStore.entries = merge(timelineStore.entries, page.entries);
    timelineStore.more = page.more;
    timelineStore.error = null;
  } catch (e) {
    if (timelineStore.wsId !== ws || seq !== refreshSeq) return;
    timelineStore.error = e instanceof Error ? e.message : String(e);
  } finally {
    if (timelineStore.wsId === ws && seq === refreshSeq) timelineStore.loading = false;
  }
}

// ---- per-viewer "last seen" ------------------------------------------------------

const SEEN_PREFIX = "chimaera.timeline.seen.";
const MM_INBOX_PREFIX = "chimaera.mastermind.inbox.";

export interface SeenMark {
  /** The highest seq this viewer had looked at. */
  seq: number;
  /** Wall clock (ms) of that look — "Nothing new since 14:02". */
  ts: number;
}

function readMark(key: string): SeenMark | null {
  try {
    const raw = localStorage.getItem(key);
    if (raw === null) return null;
    const v = JSON.parse(raw) as { seq?: unknown; ts?: unknown };
    if (typeof v.seq !== "number" || typeof v.ts !== "number") return null;
    return { seq: v.seq, ts: v.ts };
  } catch {
    return null;
  }
}

function writeMark(key: string, mark: SeenMark): void {
  try {
    localStorage.setItem(key, JSON.stringify(mark));
  } catch {
    // quota / private mode: the surface degrades to "everything is new"
  }
}

/** This viewer's last look at `wsId`'s timeline, clamped to `head`: a stored
 *  seq above the daemon's head means the data dir was wiped — start over. */
export function lastSeen(wsId: string, head: number): SeenMark | null {
  const mark = readMark(SEEN_PREFIX + wsId);
  if (mark === null) return null;
  return mark.seq > head ? null : mark;
}

/** Record that this viewer has looked up to `seq` (now). Never moves back. */
export function markSeen(wsId: string, seq: number): void {
  const prev = readMark(SEEN_PREFIX + wsId);
  if (prev !== null && prev.seq >= seq && prev.seq <= timelineStore.head) return;
  writeMark(SEEN_PREFIX + wsId, { seq, ts: Date.now() });
}

/** The Mastermind inbox cursor: notes addressed to "mastermind" past this
 *  seq are unread (the dock's chip). Separate from the dashboard mark — the
 *  user reading the dashboard has not handed anything to the Mastermind. */
export function inboxSeen(wsId: string): number {
  const mark = readMark(MM_INBOX_PREFIX + wsId);
  return mark === null || mark.seq > timelineStore.head ? 0 : mark.seq;
}

export function markInboxSeen(wsId: string, seq: number): void {
  writeMark(MM_INBOX_PREFIX + wsId, { seq, ts: Date.now() });
}
