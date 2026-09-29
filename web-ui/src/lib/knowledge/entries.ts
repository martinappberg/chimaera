/**
 * One flat index over a knowledge snapshot: every finding, decision,
 * learning, convention, to-do and session as an `Entry` with the envelope
 * the list, the reader and the id chips navigate by (kind, id, title, date,
 * the status as written, state, references, citations, and where its
 * markdown lives). The bodies stay in the files: the reader renders an
 * entry's `span` as written.
 *
 * Ids repeat in real projects (a finding id reused in two topic files);
 * `ekey` never does. An id resolves to every entry that carries it; a
 * reference resolves same-topic first (how the provider resolves amends).
 * Unit-tested in entries.test.ts.
 */
import type {
  Amend,
  Cite,
  Convention,
  Decision,
  EntryState,
  Finding,
  Knowledge,
  Learning,
  RecordedBy,
  Ref,
  SessionRow,
  Span,
  Todo,
  Topic,
} from "../workspace/knowledge";

export type EntryKind = "finding" | "decision" | "learning" | "convention" | "todo" | "session";

export interface Entry {
  /** Unique across the snapshot: `<kind>:<the normalized key>`. */
  ekey: string;
  kind: EntryKind;
  /** As written ("F-228", "D-157", "C-12", "T-DAChromatin", "#50"), or "". */
  id: string;
  /** One line: the heading, claim or item title. */
  title: string;
  date: string;
  /** The finding's topic slug; "" for other kinds. */
  topic: string;
  /** The status as written (never computed here). */
  stated: string;
  state: EntryState | null;
  amends: Amend[];
  refs: Ref[];
  cites: Cite[];
  span: Span | null;
  /** The file it lives in (the span's, else the kind's known file). */
  file: string;
  /** 1-based line of its heading; 0 when unknown. */
  line: number;
  recordedBy: RecordedBy | null;
  /** Lower-cased text the one search box reads. */
  haystack: string;
  finding?: Finding;
  topicOf?: Topic;
  decision?: Decision;
  learning?: Learning;
  convention?: Convention;
  todo?: Todo;
  session?: SessionRow;
}

export interface KnowledgeIndex {
  entries: Entry[];
  byKey: Map<string, Entry>;
  /** Upper-cased id → every entry that carries it. */
  byId: Map<string, Entry[]>;
  /** ekey → the entries that cite it, in snapshot order. */
  backlinks: Map<string, Entry[]>;
}

const REF_KIND_OF: Record<string, EntryKind> = {
  finding: "finding",
  decision: "decision",
  learning: "learning",
  convention: "convention",
  todo: "todo",
};

/** A title without the date the heading repeats at its end ("… (2026-09-28)"):
 *  the date is shown on its own. */
export function cleanTitle(title: string): string {
  return title.replace(/\s*[([]\d{4}-\d{2}-\d{2}[)\]]\s*$/, "").trim() || title;
}

function join(...parts: (string | readonly string[] | undefined)[]): string {
  const out: string[] = [];
  for (const p of parts) {
    if (p === undefined) continue;
    if (typeof p === "string") out.push(p);
    else out.push(...p);
  }
  return out.join("\n").toLowerCase();
}

function fromFinding(f: Finding, t: Topic): Entry {
  return {
    ekey: `finding:${f.key}`,
    kind: "finding",
    id: f.id,
    title: cleanTitle(f.claim),
    date: f.date || f.updated,
    topic: t.slug,
    // An older provider sends no `stated`: its normalized word is all there is.
    stated: f.stated || (f.status !== "unknown" ? f.status : ""),
    state: f.state,
    amends: f.amends,
    refs: f.refs,
    cites: f.cites,
    span: f.span,
    file: f.span?.path || t.path,
    line: f.span?.line || f.line,
    recordedBy: f.recorded_by ?? null,
    haystack: join(
      f.id,
      f.claim,
      f.stated,
      f.implications,
      f.tags,
      f.questions,
      t.slug,
      t.description,
      f.addenda.map((a) => `${a.title}\n${a.text}`),
      f.ledger.map((r) => `${r.date} ${r.run} ${r.dataset} ${r.project} ${r.result}`),
      f.cites.map((c) => c.text),
    ),
    finding: f,
    topicOf: t,
  };
}

function fromDecision(d: Decision): Entry {
  return {
    ekey: `decision:${d.key}`,
    kind: "decision",
    id: d.id,
    title: cleanTitle(d.title),
    date: d.date,
    topic: "",
    stated: d.stated,
    state: d.state,
    amends: d.amends,
    refs: d.refs,
    cites: d.cites,
    span: d.span,
    file: d.span?.path || ".living/decisions.md",
    line: d.span?.line || d.line,
    recordedBy: d.recorded_by ?? null,
    haystack: join(
      d.id,
      d.title,
      d.stated,
      d.context,
      d.decision,
      d.rationale,
      d.consequences,
      d.alternatives,
      d.tags,
      d.cites.map((c) => c.text),
    ),
    decision: d,
  };
}

function fromLearning(l: Learning): Entry {
  return {
    ekey: `learning:${l.key}`,
    kind: "learning",
    id: l.id,
    title: l.title,
    date: l.date,
    topic: "",
    stated: "",
    state: null,
    amends: [],
    refs: l.refs,
    cites: l.cites,
    span: l.span,
    file: l.span?.path || ".living/learnings.md",
    line: l.span?.line || l.line,
    recordedBy: l.recorded_by ?? null,
    haystack: join(l.id, l.title, l.category, l.what, l.why, l.resolution, l.tags, l.cites.map((c) => c.text)),
    learning: l,
  };
}

function fromConvention(c: Convention): Entry {
  return {
    ekey: `convention:${c.key}`,
    kind: "convention",
    id: c.id,
    title: c.title,
    date: "",
    topic: "",
    stated: c.status,
    state: null,
    amends: [],
    refs: c.refs,
    cites: c.cites,
    span: c.span,
    file: c.span?.path || ".living/conventions.md",
    line: c.span?.line || 0,
    recordedBy: null,
    haystack: join(c.id, c.title, c.status, c.cites.map((x) => x.text)),
    convention: c,
  };
}

function fromTodo(t: Todo): Entry {
  return {
    ekey: `todo:${t.key}`,
    kind: "todo",
    id: t.id,
    title: t.title,
    date: t.date,
    topic: "",
    stated: t.status,
    state: null,
    amends: [],
    refs: t.refs,
    cites: [],
    span: t.span,
    file: t.span?.path || "todo/TODO_REGISTRY.md",
    line: t.span?.line || 0,
    recordedBy: null,
    haystack: join(t.id, t.item, t.title, t.status, t.priority, t.category, t.author),
    todo: t,
  };
}

function fromSession(s: SessionRow): Entry {
  return {
    ekey: `session:${s.key}`,
    kind: "session",
    id: s.id,
    title: s.summary || s.id,
    date: s.date,
    topic: "",
    stated: s.status,
    state: null,
    amends: [],
    refs: [],
    cites: [],
    span: s.log !== "" ? { path: s.log, line: 1, end_line: 0 } : null,
    file: s.log,
    line: s.log !== "" ? 1 : 0,
    recordedBy: null,
    haystack: join(s.id, s.date, s.branch, s.summary, s.outputs, s.status),
    session: s,
  };
}

/** Build the index. Pure; one pass per kind plus one over every ref. */
export function buildIndex(k: Knowledge): KnowledgeIndex {
  const entries: Entry[] = [];
  for (const t of k.topics) for (const f of t.findings) entries.push(fromFinding(f, t));
  for (const d of k.decisions) entries.push(fromDecision(d));
  for (const l of k.learnings) entries.push(fromLearning(l));
  for (const c of k.conventions) entries.push(fromConvention(c));
  for (const t of k.todos) entries.push(fromTodo(t));
  for (const s of k.sessions) entries.push(fromSession(s));

  const byKey = new Map<string, Entry>();
  const byId = new Map<string, Entry[]>();
  for (const e of entries) {
    byKey.set(e.ekey, e);
    if (e.id !== "" && e.kind !== "session") {
      const id = e.id.toUpperCase();
      const list = byId.get(id);
      if (list === undefined) byId.set(id, [e]);
      else list.push(e);
    }
  }
  const idx: KnowledgeIndex = { entries, byKey, byId, backlinks: new Map() };
  for (const e of entries) {
    const seen = new Set<string>();
    for (const r of e.refs) {
      for (const target of resolveRef(idx, r, e)) {
        if (target === e || seen.has(target.ekey)) continue;
        seen.add(target.ekey);
        const list = idx.backlinks.get(target.ekey);
        if (list === undefined) idx.backlinks.set(target.ekey, [e]);
        else list.push(e);
      }
    }
  }
  return idx;
}

/**
 * The entries an id names — narrowed by kind when the ref says one, and to
 * the citing finding's own topic when that topic has it (a reused id means
 * the one next door). Several entries come back only when the id is
 * genuinely ambiguous.
 */
export function resolveRef(idx: KnowledgeIndex, ref: Ref, from?: Entry): Entry[] {
  const all = idx.byId.get(ref.id.toUpperCase()) ?? [];
  const kind = REF_KIND_OF[ref.kind];
  const ofKind = kind !== undefined ? all.filter((e) => e.kind === kind) : all;
  const pool = ofKind.length > 0 ? ofKind : all;
  if (pool.length <= 1 || from === undefined || from.topic === "") return pool;
  const near = pool.filter((e) => e.topic === from.topic);
  return near.length > 0 ? near : pool;
}

/** Every entry an id names, by the raw id alone (a chat chip, a search). */
export function entriesForId(idx: KnowledgeIndex, id: string): Entry[] {
  return idx.byId.get(id.toUpperCase()) ?? [];
}

/** The one search box over the envelope: every word must appear. An empty
 *  query matches everything (same array back). */
export function searchEntries(entries: readonly Entry[], query: string): readonly Entry[] {
  const words = query.trim().toLowerCase().split(/\s+/).filter((w) => w !== "");
  if (words.length === 0) return entries;
  return entries.filter((e) => words.every((w) => e.haystack.includes(w)));
}

/** A finding's display id where ids collide: "F-177 · cell-qc". */
export function qualifiedId(idx: KnowledgeIndex, e: Entry): string {
  if (e.id === "") return "";
  const same = idx.byId.get(e.id.toUpperCase());
  return same !== undefined && same.length > 1 && e.topic !== "" ? `${e.id} · ${e.topic}` : e.id;
}

/** Newest first; undated last; stable. */
export function byDateDesc<T extends { date: string }>(items: readonly T[]): T[] {
  return [...items].sort((a, b) => {
    if (a.date === b.date) return 0;
    if (a.date === "") return 1;
    if (b.date === "") return -1;
    return b.date.localeCompare(a.date);
  });
}
