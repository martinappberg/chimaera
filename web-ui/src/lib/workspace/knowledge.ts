/**
 * Client + reactive store for the daemon's read-only Knowledge route:
 *   GET /workspaces/{id}/knowledge   the fixed, plain-words shape (design §5)
 *
 * Agents write, Chimaera reads: the daemon parses what the structured
 * provider (mycelium) recorded plus the guidance/memory files, and this
 * store mirrors the ACTIVE workspace's answer. Knowledge changes land at
 * episode ends, so the refetch signal is the Timeline's epoch nudge
 * (`onTimelineChange`) — never a poll — plus a visibility catch-up so a
 * hidden window pulls once on return instead of per nudge.
 */
import { writable, type Readable } from "svelte/store";

import { api, ApiError } from "../net/api";
import { onTimelineChange } from "./timeline.svelte";

export type FindingStatus = "preliminary" | "supported" | "robust" | "contradicted" | "unknown";
export type LedgerDirection = "supports" | "contradicts" | "refines" | "unknown";
export type LearningCategory = "gotcha" | "edge-case" | "insight" | "failure" | "tip" | "other";

export interface RecordedBy {
  sid: string;
  name: string;
}

export interface LedgerRow {
  date: string;
  run: string;
  dataset: string;
  project: string;
  result: string;
  direction: LedgerDirection;
}

export interface Finding {
  id: string;
  claim: string;
  status: FindingStatus;
  implications: string;
  tags: string[];
  ledger: LedgerRow[];
  questions: string[];
  line: number;
  updated: string;
  recorded_by?: RecordedBy;
}

export interface Topic {
  slug: string;
  description: string;
  path: string;
  findings: Finding[];
}

export interface Decision {
  fp: string;
  date: string;
  title: string;
  context: string;
  decision: string;
  alternatives: string[];
  rationale: string;
  consequences: string;
  tags: string[];
  line: number;
  recorded_by?: RecordedBy;
}

export interface Learning {
  fp: string;
  date: string;
  title: string;
  category: LearningCategory | string;
  what: string;
  why: string;
  resolution: string;
  tags: string[];
  line: number;
  recorded_by?: RecordedBy;
}

export interface Todo {
  item: string;
  priority: string;
  status: string;
  category: string;
  date: string;
  author: string;
  file: string;
}

export interface OpenQuestion {
  text: string;
  /** The finding it belongs to ("F-002"), or "" when free-standing. */
  finding: string;
}

export interface LeftOff {
  worked_on: string;
  decisions: string;
  blockers: string[];
  current: string;
  next: string[];
  written_ms: number;
  path: string;
  session_id?: string;
  host?: string;
  by?: RecordedBy;
}

export interface GuidanceFile {
  path: string;
  label: string;
  description: string;
}

export interface Knowledge {
  schema: number;
  provider: string | null;
  updated_ms: number;
  left_off: LeftOff | null;
  topics: Topic[];
  decisions: Decision[];
  learnings: Learning[];
  todos: Todo[];
  questions: OpenQuestion[];
  counts: { findings: number; decisions: number; learnings: number; open: number };
  guidance: GuidanceFile[];
  warnings: string[];
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

function arr<T>(v: unknown): T[] {
  return Array.isArray(v) ? (v as T[]) : [];
}

/** Defensive normalization: every list defaults to empty so a partial or
 *  older payload renders its present sections instead of throwing. */
export function normalizeKnowledge(raw: unknown): Knowledge {
  const r = (typeof raw === "object" && raw !== null ? raw : {}) as Record<string, unknown>;
  const counts = (typeof r.counts === "object" && r.counts !== null ? r.counts : {}) as Record<
    string,
    unknown
  >;
  const topics = arr<Topic>(r.topics).map((t) => ({
    ...t,
    findings: arr<Finding>(t.findings).map((f) => ({
      ...f,
      tags: arr<string>(f.tags),
      ledger: arr<LedgerRow>(f.ledger),
      questions: arr<string>(f.questions),
    })),
  }));
  const num = (v: unknown, fallback: number): number => (typeof v === "number" ? v : fallback);
  const findingsN = topics.reduce((n, t) => n + t.findings.length, 0);
  const decisions = arr<Decision>(r.decisions).map((d) => ({
    ...d,
    alternatives: arr<string>(d.alternatives),
    tags: arr<string>(d.tags),
  }));
  const learnings = arr<Learning>(r.learnings).map((l) => ({ ...l, tags: arr<string>(l.tags) }));
  const todos = arr<Todo>(r.todos);
  const questions = arr<OpenQuestion>(r.questions);
  const leftRaw = r.left_off;
  const left_off =
    typeof leftRaw === "object" && leftRaw !== null
      ? {
          ...(leftRaw as LeftOff),
          blockers: arr<string>((leftRaw as LeftOff).blockers),
          next: arr<string>((leftRaw as LeftOff).next),
        }
      : null;
  return {
    schema: num(r.schema, 1),
    provider: typeof r.provider === "string" ? r.provider : null,
    updated_ms: num(r.updated_ms, 0),
    left_off,
    topics,
    decisions,
    learnings,
    todos,
    questions,
    counts: {
      findings: num(counts.findings, findingsN),
      decisions: num(counts.decisions, decisions.length),
      learnings: num(counts.learnings, learnings.length),
      open: num(counts.open, todos.length + questions.length),
    },
    guidance: arr<GuidanceFile>(r.guidance),
    warnings: arr<string>(r.warnings),
  };
}

export async function fetchKnowledge(workspaceId: string): Promise<Knowledge> {
  return normalizeKnowledge(
    await json<unknown>(await api(`/workspaces/${encodeURIComponent(workspaceId)}/knowledge`)),
  );
}

// ---- reactive store (active workspace only) ---------------------------------

const knowledgeStore = writable<Knowledge | null>(null);
/** The active workspace's knowledge (`null` = not loaded / unavailable). */
export const knowledge: Readable<Knowledge | null> = knowledgeStore;

const availableStore = writable<boolean | null>(null);
/** null until the first fetch settles; false = the daemon has no knowledge
 *  route yet (predates the feature) — surfaces say so honestly. */
export const knowledgeAvailable: Readable<boolean | null> = availableStore;

const errorStore = writable<string | null>(null);
export const knowledgeError: Readable<string | null> = errorStore;

let currentWs: string | null = null;
let refreshSeq = 0;
let staleWhileHidden = false;

function hidden(): boolean {
  return typeof document !== "undefined" && document.visibilityState === "hidden";
}

if (typeof document !== "undefined") {
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState !== "visible" || !staleWhileHidden) return;
    staleWhileHidden = false;
    if (currentWs !== null) void refresh(currentWs);
  });
}

// Knowledge changes are attributed at episode ends, which bump the timeline
// epoch — so that nudge is the refetch signal (no poll, no second frame).
onTimelineChange(() => {
  if (currentWs === null) return;
  if (hidden()) {
    staleWhileHidden = true;
    return;
  }
  void refresh(currentWs);
});

/** Point the store at a workspace (or `null`) and fetch its knowledge. */
export async function activateKnowledgeWorkspace(wsId: string | null): Promise<void> {
  if (wsId === currentWs) return;
  currentWs = wsId;
  knowledgeStore.set(null);
  availableStore.set(null);
  errorStore.set(null);
  staleWhileHidden = false;
  if (wsId !== null) await refresh(wsId);
}

async function refresh(wsId: string): Promise<void> {
  const seq = ++refreshSeq;
  try {
    const k = await fetchKnowledge(wsId);
    if (currentWs !== wsId || seq !== refreshSeq) return;
    knowledgeStore.set(k);
    availableStore.set(true);
    errorStore.set(null);
  } catch (e) {
    if (currentWs !== wsId || seq !== refreshSeq) return;
    if (e instanceof ApiError && e.status === 404) {
      availableStore.set(false);
      errorStore.set(null);
    } else {
      errorStore.set(e instanceof Error ? e.message : String(e));
    }
  }
}

/** Force a refresh of the active workspace (tab focus, the attach sheet
 *  closing, a manual control). */
export function refreshKnowledge(): void {
  if (currentWs !== null) void refresh(currentWs);
}
