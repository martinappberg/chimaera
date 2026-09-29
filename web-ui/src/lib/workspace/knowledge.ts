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

/** Where an entry's markdown lives: its heading line to its last line
 *  (1-based, inclusive), workspace-relative. The reader renders this slice
 *  of the file as written; bodies never ride the snapshot. */
export interface Span {
  path: string;
  line: number;
  end_line: number;
}

/** An id an entry cites, as written ("F-171", "D-152", "T-DAChromatin"). */
export interface Ref {
  kind: "finding" | "decision" | "convention" | "learning" | "todo" | string;
  id: string;
}

/** Something an entry names as its evidence: a file, a job, a commit. */
export interface Cite {
  kind: "script" | "data" | "figure" | "doc" | "path" | "job" | "commit" | string;
  text: string;
}

/** An entry's standing, only from what the text says (`⛔ SUPERSEDED BY`,
 *  `RETRACTED`, another entry's "This CORRECTS F-171"). */
export interface EntryState {
  kind: "superseded" | "corrected" | "retracted" | "suspect" | "resolved" | string;
  /** The id that did it, when known. */
  by: string;
}

/** What an entry says it changes ("This CORRECTS F-171"). */
export interface Amend {
  kind: "corrects" | "supersedes" | "retracts" | string;
  id: string;
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
  /** addendum | correction | resolution | update ("" from an older plugin). */
  kind: string;
  date: string;
  /** Its own Status, as written. */
  stated: string;
  span: Span | null;
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
  /** The Status exactly as the agent wrote it ("" when none) — what the UI
   *  shows. `status` is only the Mycelium word it starts with. */
  stated: string;
  /** Written in the heading or a Date field; "" when none. */
  date: string;
  span: Span | null;
  refs: Ref[];
  cites: Cite[];
  state: EntryState | null;
  amends: Amend[];
}

export interface Topic {
  slug: string;
  /** Client-only, unique among the topics; `slug` may repeat. */
  key: string;
  description: string;
  path: string;
  /** Frontmatter `last_updated`; "" when none. */
  date: string;
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
  /** As written in the heading ("D-157"), Mycelium's positional `D-n`, or "". */
  id: string;
  stated: string;
  span: Span | null;
  refs: Ref[];
  cites: Cite[];
  state: EntryState | null;
  amends: Amend[];
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
  id: string;
  span: Span | null;
  refs: Ref[];
  cites: Cite[];
}

export interface Todo {
  /** Client key: the provider's (`todo/T-x`, `todo/r3`) made unique. */
  key: string;
  item: string;
  priority: string;
  status: string;
  category: string;
  date: string;
  author: string;
  file: string;
  /** "#50", "T-DAChromatin", or "". */
  id: string;
  /** The item's first sentence (the item itself from an older plugin). */
  title: string;
  /** By the provider's reading of its status. */
  closed: boolean;
  /** table | section ("" from an older plugin). */
  source: string;
  span: Span | null;
  refs: Ref[];
}

export interface OpenQuestion {
  text: string;
  /** The finding it belongs to ("F-002"), or "" when free-standing. */
  finding: string;
  /** That finding's key ("" when unknown). */
  findingKey: string;
}

export interface Convention {
  key: string;
  id: string;
  title: string;
  status: string;
  date: string;
  span: Span | null;
  refs: Ref[];
  cites: Cite[];
}

export interface SessionRow {
  key: string;
  id: string;
  date: string;
  branch: string;
  duration: string;
  files: string;
  summary: string;
  outputs: string;
  status: string;
  /** The session log, workspace-relative ("" when none). */
  log: string;
}

/** Something put to the user ("Put to the user…", a parked decision). */
export interface Ask {
  text: string;
  date: string;
  source: { kind: string; id: string; key: string };
  span: Span | null;
}

/** A factual inconsistency in the recorded knowledge, and the request an
 *  agent would need to fix it. */
export interface TidyRow {
  kind: string;
  text: string;
  refs: Ref[];
  ask: string;
}

/** An id shape the provider answers for — what may become a chip. */
export interface IdShape {
  kind: string;
  pattern: string;
}

export interface StatusWord {
  word: string;
  /** 1–3 on the ladder, 0 for none. */
  rank: number;
  tone: "neutral" | "good" | "warn" | "bad" | string;
}

/** The provider's words for the view (it owns them, not core). */
export interface KnowledgeLabels {
  source: string;
  sections: Record<string, string>;
  /** The singular word per kind ("finding", "to-do"). */
  kinds: Record<string, string>;
  status_words: StatusWord[];
  status_note: string;
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
  span: Span | null;
  /** Every handoff found, newest first. */
  sources: { path: string; written_ms: number; session_id?: string; host?: string }[];
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
  conventions: Convention[];
  sessions: SessionRow[];
  asks: Ask[];
  tidy: TidyRow[];
  id_shapes: IdShape[];
  labels: KnowledgeLabels | null;
  counts: {
    findings: number;
    decisions: number;
    learnings: number;
    open: number;
    /** Open to-dos. */
    todos: number;
    questions: number;
    conventions: number;
    sessions: number;
  };
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

/** A to-do status an older provider didn't classify: closed when it starts
 *  with a closed word (the provider's own rule since 0.2.0). */
const CLOSED_STATUS = /^\W*(complete|completed|done|closed|wont-do|won't do|wont do|won't-do|cancel+ed|dropped)\b/i;

function span(v: unknown): Span | null {
  if (typeof v !== "object" || v === null) return null;
  const o = v as Record<string, unknown>;
  const path = str(o.path);
  const line = typeof o.line === "number" && o.line > 0 ? Math.floor(o.line) : 0;
  const end_line = typeof o.end_line === "number" && o.end_line >= line ? Math.floor(o.end_line) : line;
  return path !== "" && line > 0 ? { path, line, end_line } : null;
}

function refs(v: unknown): Ref[] {
  return objs<Ref>(v)
    .map((r) => ({ kind: str(r.kind), id: str(r.id) }))
    .filter((r) => r.id !== "");
}

function cites(v: unknown): Cite[] {
  return objs<Cite>(v)
    .map((c) => ({ kind: str(c.kind), text: str(c.text) }))
    .filter((c) => c.text !== "");
}

function entryState(v: unknown): EntryState | null {
  if (typeof v !== "object" || v === null) return null;
  const o = v as Record<string, unknown>;
  const kind = str(o.kind);
  return kind === "" ? null : { kind, by: str(o.by) };
}

function amends(v: unknown): Amend[] {
  return objs<Amend>(v)
    .map((a) => ({ kind: str(a.kind), id: str(a.id) }))
    .filter((a) => a.kind !== "" && a.id !== "");
}

function labels(v: unknown): KnowledgeLabels | null {
  if (typeof v !== "object" || v === null) return null;
  const o = v as Record<string, unknown>;
  const words = (v: unknown): Record<string, string> => {
    const out: Record<string, string> = {};
    if (typeof v === "object" && v !== null) {
      for (const [k, t] of Object.entries(v as Record<string, unknown>)) if (typeof t === "string" && t !== "") out[k] = t;
    }
    return out;
  };
  return {
    source: str(o.source),
    sections: words(o.sections),
    kinds: words(o.kinds),
    status_words: objs<StatusWord>(o.status_words)
      .map((w) => ({ word: str(w.word), rank: typeof w.rank === "number" ? w.rank : 0, tone: str(w.tone) || "neutral" }))
      .filter((w) => w.word !== ""),
    status_note: str(o.status_note),
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
  // A finding's key is the provider's (`<slug>/<id>`, unique by design) when
  // it sends one, else the id; the keyer keeps it unique either way.
  const findingKey = keyer();
  const topics = objs<Topic>(r.topics).map((t) => ({
    ...t,
    slug: str(t.slug),
    key: topicKey(str(t.slug)),
    description: str(t.description),
    path: str(t.path),
    date: str(t.date),
    findings: objs<Finding>(t.findings).map((f) => ({
      ...f,
      id: str(f.id),
      key: findingKey(str(f.key) || str(f.id)),
      claim: str(f.claim),
      status: (str(f.status) || "unknown") as FindingStatus,
      stated: str(f.stated),
      date: str(f.date),
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
        kind: str(a.kind),
        date: str(a.date),
        stated: str(a.stated),
        span: span(a.span),
      })),
      span: span(f.span),
      refs: refs(f.refs),
      cites: cites(f.cites),
      state: entryState(f.state),
      amends: amends(f.amends),
    })),
  }));
  const findingsN = topics.reduce((n, t) => n + t.findings.length, 0);
  const decisionKey = keyer();
  const decisions = objs<Decision>(r.decisions).map((d) => ({
    ...d,
    fp: str(d.fp),
    key: decisionKey(str(d.fp)),
    id: str(d.id),
    date: str(d.date),
    title: str(d.title),
    context: str(d.context),
    decision: str(d.decision),
    rationale: str(d.rationale),
    consequences: str(d.consequences),
    alternatives: strs(d.alternatives),
    tags: strs(d.tags),
    stated: str(d.stated),
    span: span(d.span),
    refs: refs(d.refs),
    cites: cites(d.cites),
    state: entryState(d.state),
    amends: amends(d.amends),
  }));
  const learningKey = keyer();
  const learnings = objs<Learning>(r.learnings).map((l) => ({
    ...l,
    fp: str(l.fp),
    key: learningKey(str(l.fp)),
    id: str(l.id),
    date: str(l.date),
    title: str(l.title),
    category: str(l.category),
    what: str(l.what),
    why: str(l.why),
    resolution: str(l.resolution),
    tags: strs(l.tags),
    span: span(l.span),
    refs: refs(l.refs),
    cites: cites(l.cites),
  }));
  const todoKey = keyer();
  const todos = objs<Todo>(r.todos).map((t) => {
    const item = str(t.item);
    const status = str(t.status);
    return {
      ...t,
      key: todoKey(str(t.key) || "todo"),
      item,
      priority: str(t.priority),
      status,
      category: str(t.category),
      date: str(t.date),
      author: str(t.author),
      file: str(t.file),
      id: str(t.id),
      title: str(t.title) || item,
      closed: typeof t.closed === "boolean" ? t.closed : CLOSED_STATUS.test(status.replace(/\*/g, "")),
      source: str(t.source),
      span: span(t.span),
      refs: refs(t.refs),
    };
  });
  const questions = objs<OpenQuestion & { key?: unknown }>(r.questions).map((q) => ({
    text: str(q.text),
    finding: str(q.finding),
    findingKey: str(q.key),
  }));
  const conventionKey = keyer();
  const conventions = objs<Convention>(r.conventions).map((c) => ({
    key: conventionKey(str(c.key) || str(c.id) || str(c.title)),
    id: str(c.id),
    title: str(c.title),
    status: str(c.status),
    date: str(c.date),
    span: span(c.span),
    refs: refs(c.refs),
    cites: cites(c.cites),
  }));
  const sessionKey = keyer();
  const sessions = objs<SessionRow>(r.sessions).map((x) => ({
    key: sessionKey(`session/${str(x.id) || str(x.date)}`),
    id: str(x.id),
    date: str(x.date),
    branch: str(x.branch),
    duration: str(x.duration),
    files: str(x.files),
    summary: str(x.summary),
    outputs: str(x.outputs),
    status: str(x.status),
    log: str(x.log),
  }));
  const asks = objs<Ask>(r.asks)
    .map((a) => {
      const src = (typeof a.source === "object" && a.source !== null ? a.source : {}) as Record<string, unknown>;
      return {
        text: str(a.text),
        date: str(a.date),
        source: { kind: str(src.kind), id: str(src.id), key: str(src.key) },
        span: span(a.span),
      };
    })
    .filter((a) => a.text !== "");
  const tidy = objs<TidyRow>(r.tidy)
    .map((t) => ({ kind: str(t.kind), text: str(t.text), refs: refs(t.refs), ask: str(t.ask) }))
    .filter((t) => t.text !== "");
  const id_shapes = objs<IdShape>(r.id_shapes)
    .map((x) => ({ kind: str(x.kind), pattern: str(x.pattern) }))
    .filter((x) => x.kind !== "" && x.pattern !== "");
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
          written_ms: num((leftRaw as LeftOff).written_ms, 0),
          span: span((leftRaw as LeftOff).span),
          sources: objs<LeftOff["sources"][number]>((leftRaw as LeftOff).sources)
            .map((x) => ({
              path: str(x.path),
              written_ms: num(x.written_ms, 0),
              ...(typeof x.session_id === "string" ? { session_id: x.session_id } : {}),
              ...(typeof x.host === "string" ? { host: x.host } : {}),
            }))
            .filter((x) => x.path !== ""),
        }
      : null;
  const openTodos = todos.filter((t) => !t.closed).length;
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
    conventions,
    sessions,
    asks,
    tidy,
    id_shapes,
    labels: labels(r.labels),
    counts: {
      findings: num(counts.findings, findingsN),
      decisions: num(counts.decisions, decisions.length),
      learnings: num(counts.learnings, learnings.length),
      open: num(counts.open, openTodos + questions.length),
      todos: num(counts.todos, openTodos),
      questions: num(counts.questions, questions.length),
      conventions: num(counts.conventions, conventions.length),
      sessions: num(counts.sessions, sessions.length),
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

const rootStore = writable<string | null>(null);
/** The active workspace's absolute root (entries' spans are relative to it). */
export const knowledgeRoot: Readable<string | null> = rootStore;

/** Follow the active workspace's root (the app sets it as workspaces switch). */
export function setKnowledgeRoot(root: string | null): void {
  rootStore.set(root);
}
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

// ---- "open this entry" requests ----------------------------------------------

/**
 * A one-shot request that Knowledge show an entry (a chat's id chip, a
 * Timeline row, the dashboard). Not layout state: the view takes it when it
 * is showing (or on mount), which clears it, so a remount never replays it.
 * `ekey` is the entry's index key (`finding:<key>`, `decision:<fp>`, …).
 */
const focusStore = writable<{ ekey: string; seq: number } | null>(null);
export const knowledgeFocus: Readable<{ ekey: string; seq: number } | null> = focusStore;
let focusSeq = 0;

export function focusKnowledgeEntry(ekey: string): void {
  focusStore.set({ ekey, seq: ++focusSeq });
}

/** Take (and clear) the pending request, if any. */
export function takeKnowledgeFocus(): string | null {
  let ekey: string | null = null;
  focusStore.update((v) => {
    ekey = v?.ekey ?? null;
    return null;
  });
  return ekey;
}

/**
 * How the app opens Knowledge at an entry from a pane (a chat's chip): the
 * app registers it (it owns the layout); `openKnowledgeEntry` asks it.
 */
type Opener = (ekey: string, from: { paneId: string | null; newSplit: boolean }) => void;
let opener: Opener | null = null;

export function registerKnowledgeOpener(fn: Opener): () => void {
  opener = fn;
  return () => {
    if (opener === fn) opener = null;
  };
}

export function openKnowledgeEntry(ekey: string, from: { paneId: string | null; newSplit: boolean }): void {
  if (opener !== null) opener(ekey, from);
  else focusKnowledgeEntry(ekey);
}
