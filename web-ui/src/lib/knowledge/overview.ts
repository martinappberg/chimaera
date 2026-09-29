/**
 * Pure derivations for the Knowledge Overview and To do screens: what
 * changed recently (bad news first), what is put to the user, open work,
 * to-do grouping, and the provider's words (labels, status words) over
 * neutral defaults. Nothing here judges or computes a
 * status: a status is shown as the agent wrote it. Unit-tested in
 * overview.test.ts.
 */
import { tone as toTone, type Tone } from "../shared/ui/tone";
import type { Ask, Knowledge, KnowledgeLabels, StatusWord, Todo } from "../workspace/knowledge";
import { byDateDesc, type Entry, type KnowledgeIndex } from "./entries";

export type { Tone };

// ---- the provider's words ---------------------------------------------------

/** Section names when the provider gives none: the documented kinds' own
 *  names (knowledge/1), never a provider's vocabulary. */
const DEFAULT_SECTIONS: Record<string, string> = {
  overview: "Overview",
  left_off: "Where we left off",
  asks: "Waiting on you",
  changed: "What changed",
  open_work: "Open work",
  findings: "Findings",
  decisions: "Decisions",
  learnings: "Learnings",
  conventions: "Conventions",
  todos: "To-dos",
  sessions: "Sessions",
  tidy: "Tidy up",
};

const DEFAULT_KINDS: Record<string, string> = {
  finding: "finding",
  decision: "decision",
  learning: "learning",
  convention: "convention",
  todo: "to-do",
  session: "session",
};

/** The provider's labels over the neutral defaults. Core names no provider,
 *  file or status word: without the provider's status words a status shows
 *  as its text alone. */
export function providerLabels(k: Knowledge): KnowledgeLabels {
  const own = k.labels;
  return {
    source: own?.source ?? "",
    sections: { ...DEFAULT_SECTIONS, ...(own?.sections ?? {}) },
    kinds: { ...DEFAULT_KINDS, ...(own?.kinds ?? {}) },
    status_words: own?.status_words ?? [],
    status_note: own?.status_note ?? "",
  };
}

/** The provider's singular word for a kind. */
export function kindWord(labels: KnowledgeLabels, kind: string): string {
  return labels.kinds[kind] ?? kind;
}

/** The provider's word a stated status starts with, if it is one. */
export function statusWord(labels: KnowledgeLabels, stated: string): StatusWord | null {
  const first = stated.trim().toLowerCase().match(/^[a-z-]+/)?.[0] ?? "";
  if (first === "") return null;
  return labels.status_words.find((w) => w.word.toLowerCase() === first) ?? null;
}

export function toneOf(t: string): Tone {
  return toTone(t);
}

// ---- states -----------------------------------------------------------------

/** An entry's standing as a short phrase and its tone (colour never alone). */
export function stateLabel(e: Entry, idOf: (id: string) => string = (id) => id): { text: string; tone: Tone } | null {
  const s = e.state;
  if (s === null) return null;
  const by = s.by !== "" ? ` ${s.kind === "superseded" ? "→" : "by"} ${idOf(s.by)}` : "";
  switch (s.kind) {
    case "superseded":
      return { text: `superseded${by}`, tone: "warn" };
    case "corrected":
      return { text: `corrected${by}`, tone: "bad" };
    case "retracted":
      return { text: `retracted${by}`, tone: "bad" };
    case "suspect":
      return { text: "suspect", tone: "warn" };
    case "resolved":
      return { text: "resolved", tone: "good" };
    default:
      return { text: s.kind, tone: "neutral" };
  }
}

/** Retired entries dim in lists (still there, still findable). */
export function isRetired(e: Entry): boolean {
  return e.state !== null && (e.state.kind === "superseded" || e.state.kind === "corrected" || e.state.kind === "retracted");
}

/** Entries that are bad news for "What changed": they change what was
 *  believed (they correct or supersede something, or were). */
export function isBadNews(e: Entry): boolean {
  return e.amends.length > 0 || isRetired(e) || e.state?.kind === "suspect";
}

// ---- what changed -----------------------------------------------------------

/** YYYY-MM-DD in local time. */
/** The provider's status note cut to its first sentence, for tight spots; the
 *  whole note goes in a tooltip. */
export function shortStatusNote(note: string): string {
  return note.match(/^[^.!?]*[.!?]/)?.[0] ?? note;
}

export function isoDay(ms: number): string {
  const d = new Date(ms);
  const p = (n: number): string => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

function dayMinus(day: string, n: number): string {
  const [y, m, d] = day.split("-").map(Number);
  return isoDay(new Date(y, m - 1, d - n).getTime());
}

export interface ChangedDay {
  day: string;
  entries: Entry[];
}

const CHANGE_KINDS: ReadonlySet<string> = new Set(["finding", "decision", "learning", "convention"]);
const KIND_ORDER: Record<string, number> = { finding: 0, decision: 1, learning: 2, convention: 3 };

/**
 * Entries dated within the last `days` days of `today`, by day, newest day
 * first; within a day bad news leads, then findings, decisions, learnings.
 * Dates are what the entries say (no clock of ours is involved).
 */
export function whatChanged(idx: KnowledgeIndex, today: string, days = 7): ChangedDay[] {
  const from = dayMinus(today, days - 1);
  const recent = idx.entries.filter((e) => CHANGE_KINDS.has(e.kind) && e.date.slice(0, 10) >= from && e.date.slice(0, 10) <= today);
  const byDay = new Map<string, Entry[]>();
  for (const e of recent) {
    const d = e.date.slice(0, 10);
    const list = byDay.get(d);
    if (list === undefined) byDay.set(d, [e]);
    else list.push(e);
  }
  return [...byDay.keys()]
    .sort((a, b) => b.localeCompare(a))
    .map((day) => ({
      day,
      entries: (byDay.get(day) ?? []).sort(
        (a, b) => Number(isBadNews(b)) - Number(isBadNews(a)) || (KIND_ORDER[a.kind] ?? 9) - (KIND_ORDER[b.kind] ?? 9),
      ),
    }));
}

/** The newest date any entry carries ("" when none): the quiet line's "last
 *  entry" when nothing changed this week. */
export function newestDate(idx: KnowledgeIndex): string {
  let best = "";
  for (const e of idx.entries) if (e.kind !== "session" && e.date.slice(0, 10) > best) best = e.date.slice(0, 10);
  return best;
}

// ---- to-dos -----------------------------------------------------------------

export type TodoGroup = "progress" | "blocked" | "open" | "later" | "done";

const PRIORITY_RANK: Record<string, number> = { critical: 0, urgent: 0, high: 1, medium: 2, normal: 2, low: 3, idea: 4 };

export function priorityRank(p: string): number {
  const w = p.trim().toLowerCase().replace(/\*/g, "").match(/^[a-z]+/)?.[0] ?? "";
  return PRIORITY_RANK[w] ?? 5;
}

export function priorityTone(p: string): Tone {
  const r = priorityRank(p);
  return r === 0 ? "bad" : r === 1 ? "warn" : "neutral";
}

/** Which group a to-do sits in, by its status as written. */
export function todoGroup(t: Todo): TodoGroup {
  if (t.closed) return "done";
  const s = t.status.toLowerCase().replace(/\*/g, "").trim();
  if (/^(in[- ]?progress|active|doing|started|wip)\b/.test(s)) return "progress";
  if (/^blocked\b/.test(s)) return "blocked";
  if (/^(on[- ]?hold|deferred|parked|recurring|later|someday|paused|waiting)\b/.test(s)) return "later";
  return "open";
}

export const TODO_GROUP_ORDER: TodoGroup[] = ["progress", "blocked", "open", "later", "done"];

export const TODO_GROUP_LABEL: Record<TodoGroup, string> = {
  progress: "In progress",
  blocked: "Blocked",
  open: "Open",
  later: "On hold",
  done: "Done",
};

/** Grouped and ordered: groups in `TODO_GROUP_ORDER`, each by priority then
 *  newest; the groups that are empty are left out. */
export function todoGroups<T extends { todo?: Todo; date: string }>(items: readonly T[]): { group: TodoGroup; items: T[] }[] {
  const by = new Map<TodoGroup, T[]>();
  for (const it of items) {
    if (it.todo === undefined) continue;
    const g = todoGroup(it.todo);
    const list = by.get(g);
    if (list === undefined) by.set(g, [it]);
    else list.push(it);
  }
  return TODO_GROUP_ORDER.filter((g) => by.has(g)).map((group) => ({
    group,
    items: byDateDesc(by.get(group) ?? []).sort(
      (a, b) => priorityRank(a.todo?.priority ?? "") - priorityRank(b.todo?.priority ?? ""),
    ),
  }));
}

/** Open work for the Overview: in progress, blocked, then critical/high open. */
export function openWork(idx: KnowledgeIndex, limit = 6): { items: Entry[]; open: number; progress: number; blocked: number } {
  const todos = idx.entries.filter((e) => e.kind === "todo" && e.todo !== undefined && !e.todo.closed);
  const rank = (e: Entry): number => {
    const g = todoGroup(e.todo as Todo);
    if (g === "progress") return 0;
    if (g === "blocked") return 1;
    if (g === "open" && priorityRank(e.todo?.priority ?? "") <= 1) return 2;
    return 9;
  };
  const items = byDateDesc(todos.filter((e) => rank(e) < 9)).sort(
    (a, b) => rank(a) - rank(b) || priorityRank(a.todo?.priority ?? "") - priorityRank(b.todo?.priority ?? ""),
  );
  return {
    items: items.slice(0, limit),
    open: todos.length,
    progress: todos.filter((e) => todoGroup(e.todo as Todo) === "progress").length,
    blocked: todos.filter((e) => todoGroup(e.todo as Todo) === "blocked").length,
  };
}

// ---- waiting on you ---------------------------------------------------------

export interface Waiting {
  text: string;
  date: string;
  /** The entry it came from, when the index has it. */
  entry: Entry | null;
  /** A handoff or another source without an entry. */
  sourceLabel: string;
  span: Ask["span"];
}

/** What is put to the user: the provider's asks, newest first. */
export function waitingOnYou(k: Knowledge, idx: KnowledgeIndex, limit = 5): { items: Waiting[]; total: number } {
  const items: Waiting[] = k.asks.map((a) => {
    const kind = a.source.kind;
    let entry: Entry | null = null;
    if (a.source.key !== "") entry = idx.byKey.get(`${kind}:${a.source.key}`) ?? null;
    if (entry === null && a.source.id !== "") entry = idx.byId.get(a.source.id.toUpperCase())?.[0] ?? null;
    return {
      text: a.text,
      // A handoff ask carries the file's day as the provider saw it (UTC);
      // the handoff's own write time gives the reader's local day.
      date: kind === "handoff" && k.left_off !== null && k.left_off.written_ms > 0 ? isoDay(k.left_off.written_ms) : a.date,
      entry,
      sourceLabel: entry === null ? (kind === "handoff" ? "handoff" : a.source.id || kind) : "",
      span: a.span,
    };
  });
  const sorted = byDateDesc(items);
  return { items: sorted.slice(0, limit), total: sorted.length };
}
