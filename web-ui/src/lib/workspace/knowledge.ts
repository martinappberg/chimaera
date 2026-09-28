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

/** A follow-up the project wrote under its own `F-NNN addendum…` heading. */
export interface FindingAddendum {
  /** The heading's word and qualifier as written: "Addendum (2)". */
  label: string;
  /** The heading after its separator; may be empty. */
  title: string;
  /** Markdown. */
  text: string;
  /** 1-based line of its heading; 0 when unknown. */
  line: number;
}

export interface Finding {
  id: string;
  /** Client-only, unique across the snapshot (`normalizeKnowledge`): the
   *  list key, the expand state and the element id. `id` may repeat. */
  key: string;
  claim: string;
  status: FindingStatus;
  implications: string;
  tags: string[];
  ledger: LedgerRow[];
  questions: string[];
  /** In file order; empty when the provider sends none. */
  addenda: FindingAddendum[];
  line: number;
  updated: string;
  recorded_by?: RecordedBy;
}

export interface Topic {
  slug: string;
  /** Client-only, unique among the topics; `slug` may repeat. */
  key: string;
  description: string;
  path: string;
  findings: Finding[];
}

export interface Decision {
  fp: string;
  /** Client-only, unique among the decisions; `fp` may repeat. */
  key: string;
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
  /** Client-only, unique among the learnings; `fp` may repeat. */
  key: string;
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
  /** The provider couldn't answer just now (faulted, too slow, a bad
   *  snapshot): the lists are the last snapshot it gave, or empty. Null
   *  when it answered. */
  error: string | null;
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

/** Objects only: a provider's list may hold anything. */
function objs<T>(v: unknown): T[] {
  return arr<unknown>(v).filter((x): x is T => typeof x === "object" && x !== null);
}

function str(v: unknown): string {
  return typeof v === "string" ? v : "";
}

function strs(v: unknown): string[] {
  return arr<unknown>(v).filter((x): x is string => typeof x === "string");
}

/**
 * Keys for keyed lists, unique within one snapshot. The provider is a
 * plugin reading hand-kept files, and real ones repeat ids (a `.living/`
 * that files `### F-027 addendum:` under F-027, decisions numbered twice):
 * a repeated key throws inside Svelte's list reconciler and takes the whole
 * window's UI down with it. The first holder keeps the id; later ones get
 * `~2`, `~3`, … (never a key already taken).
 */
function keyer(): (base: string) => string {
  const used = new Set<string>();
  return (base) => {
    const b = base === "" ? "entry" : base;
    let key = b;
    for (let n = 2; used.has(key); n++) key = `${b}~${n}`;
    used.add(key);
    return key;
  };
}

/** Defensive normalization: every list defaults to empty, every text field
 *  to a string, and every row gets a unique `key`, so a partial, older or
 *  odd provider payload renders its present sections instead of throwing. */
export function normalizeKnowledge(raw: unknown): Knowledge {
  const r = (typeof raw === "object" && raw !== null ? raw : {}) as Record<string, unknown>;
  const counts = (typeof r.counts === "object" && r.counts !== null ? r.counts : {}) as Record<
    string,
    unknown
  >;
  const num = (v: unknown, fallback: number): number => (typeof v === "number" ? v : fallback);
  const topicKey = keyer();
  const findingKey = keyer();
  const topics = objs<Topic>(r.topics).map((t) => ({
    ...t,
    slug: str(t.slug),
    key: topicKey(str(t.slug)),
    description: str(t.description),
    path: str(t.path),
    findings: objs<Finding>(t.findings).map((f) => ({
      ...f,
      id: str(f.id),
      key: findingKey(str(f.id)),
      claim: str(f.claim),
      status: (str(f.status) || "unknown") as FindingStatus,
      implications: str(f.implications),
      updated: str(f.updated),
      tags: strs(f.tags),
      ledger: objs<LedgerRow>(f.ledger).map((row) => ({
        date: str(row.date),
        run: str(row.run),
        dataset: str(row.dataset),
        project: str(row.project),
        result: str(row.result),
        direction: (str(row.direction) || "unknown") as LedgerDirection,
      })),
      questions: strs(f.questions),
      addenda: objs<FindingAddendum>(f.addenda).map((a) => ({
        label: str(a.label),
        title: str(a.title),
        text: str(a.text),
        line: num(a.line, 0),
      })),
    })),
  }));
  const findingsN = topics.reduce((n, t) => n + t.findings.length, 0);
  const decisionKey = keyer();
  const decisions = objs<Decision>(r.decisions).map((d) => ({
    ...d,
    fp: str(d.fp),
    key: decisionKey(str(d.fp)),
    date: str(d.date),
    title: str(d.title),
    context: str(d.context),
    decision: str(d.decision),
    rationale: str(d.rationale),
    consequences: str(d.consequences),
    alternatives: strs(d.alternatives),
    tags: strs(d.tags),
  }));
  const learningKey = keyer();
  const learnings = objs<Learning>(r.learnings).map((l) => ({
    ...l,
    fp: str(l.fp),
    key: learningKey(str(l.fp)),
    date: str(l.date),
    title: str(l.title),
    category: str(l.category),
    what: str(l.what),
    why: str(l.why),
    resolution: str(l.resolution),
    tags: strs(l.tags),
  }));
  const todos = objs<Todo>(r.todos).map((t) => ({
    ...t,
    item: str(t.item),
    priority: str(t.priority),
    status: str(t.status),
    category: str(t.category),
    author: str(t.author),
  }));
  const questions = objs<OpenQuestion>(r.questions).map((q) => ({
    text: str(q.text),
    finding: str(q.finding),
  }));
  const leftRaw = r.left_off;
  const left_off =
    typeof leftRaw === "object" && leftRaw !== null
      ? {
          ...(leftRaw as LeftOff),
          current: str((leftRaw as LeftOff).current),
          worked_on: str((leftRaw as LeftOff).worked_on),
          decisions: str((leftRaw as LeftOff).decisions),
          path: str((leftRaw as LeftOff).path),
          blockers: strs((leftRaw as LeftOff).blockers),
          next: strs((leftRaw as LeftOff).next),
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
    guidance: objs<GuidanceFile>(r.guidance),
    warnings: strs(r.warnings),
    error: typeof r.error === "string" && r.error !== "" ? r.error : null,
  };
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
/** The body behind the snapshot held: most refetches (every turn end
 *  nudges one) answer the same bytes, and a new object would re-render
 *  every row of a large repository for nothing. */
let heldText: string | null = null;
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
  heldText = null;
  availableStore.set(null);
  errorStore.set(null);
  staleWhileHidden = false;
  if (wsId !== null) await refresh(wsId);
}

async function refresh(wsId: string): Promise<void> {
  const seq = ++refreshSeq;
  try {
    const res = await api(`/workspaces/${encodeURIComponent(wsId)}/knowledge`);
    if (!res.ok) await json<unknown>(res);
    const text = await res.text();
    if (currentWs !== wsId || seq !== refreshSeq) return;
    if (text !== heldText) {
      knowledgeStore.set(normalizeKnowledge(JSON.parse(text)));
      heldText = text;
    }
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
