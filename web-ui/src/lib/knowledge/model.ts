/**
 * Pure derivations over the Knowledge payload: the client-side search that
 * filters every section, the "Where things stand" pick (contradicted first,
 * then the strongest and most recent), the confidence ladder and evidence
 * strip inputs, and the tone tables that pair colour with a word (design
 * §9: colour is never alone). Unit-tested in model.test.ts.
 */
import type {
  Finding,
  FindingStatus,
  Knowledge,
  LedgerDirection,
  LedgerRow,
} from "../workspace/knowledge";
import type { TimelineEntry } from "../workspace/timeline.svelte";

/** mycelium's own ladder, explained once by the legend beside the list. */
export const STATUS_ORDER: FindingStatus[] = ["contradicted", "robust", "supported", "preliminary", "unknown"];

/** Filled marks out of three (`null` = contradicted, drawn as ✕). */
export function ladderFill(status: FindingStatus | string): number | null {
  switch (status) {
    case "preliminary":
      return 1;
    case "supported":
      return 2;
    case "robust":
      return 3;
    case "contradicted":
      return null;
    default:
      return 0;
  }
}

/** The ladder legend copy, verbatim from the mockup. */
export const LADDER_LEGEND: { status: FindingStatus; text: string }[] = [
  { status: "preliminary", text: "preliminary · 1 run" },
  { status: "supported", text: "supported · 2+ agree" },
  { status: "robust", text: "robust · 3+ datasets" },
  { status: "contradicted", text: "contradicted" },
];

/** Sort rank for "strongest first" — contradicted leads (a contradiction is
 *  the most valuable line a scientist can be shown), then robust down. */
export function statusRank(status: FindingStatus | string): number {
  const i = STATUS_ORDER.indexOf(status as FindingStatus);
  return i < 0 ? STATUS_ORDER.length : i;
}

export interface LedgerSummary {
  total: number;
  supports: number;
  contradicts: number;
  refines: number;
}

export function ledgerSummary(ledger: readonly LedgerRow[]): LedgerSummary {
  const s: LedgerSummary = { total: ledger.length, supports: 0, contradicts: 0, refines: 0 };
  for (const row of ledger) {
    if (row.direction === "supports") s.supports += 1;
    else if (row.direction === "contradicts") s.contradicts += 1;
    else if (row.direction === "refines") s.refines += 1;
  }
  return s;
}

/** "4 runs" · "3 runs · 1 contradicts" — the strip's caption. */
export function ledgerCaption(ledger: readonly LedgerRow[]): string {
  const s = ledgerSummary(ledger);
  if (s.total === 0) return "no evidence yet";
  const runs = `${s.total} run${s.total === 1 ? "" : "s"}`;
  return s.contradicts > 0 ? `${runs} · ${s.contradicts} contradict${s.contradicts === 1 ? "s" : ""}` : runs;
}

export type Tone = "accent" | "err" | "warn" | "neutral";

export function directionTone(d: LedgerDirection | string): Tone {
  if (d === "supports") return "accent";
  if (d === "contradicts") return "err";
  return "neutral";
}

/** Learning categories: gotcha → warn, failure → err, insight → accent. */
export function categoryTone(category: string): Tone {
  switch (category) {
    case "gotcha":
      return "warn";
    case "failure":
      return "err";
    case "insight":
      return "accent";
    default:
      return "neutral";
  }
}

/** Todo status pills: blocked → warn, in-progress/active → accent. */
export function todoTone(status: string): Tone {
  const s = status.toLowerCase();
  if (s === "blocked") return "warn";
  if (s === "in-progress" || s === "in progress" || s === "active" || s === "doing") return "accent";
  return "neutral";
}

export function statusTone(status: FindingStatus | string): Tone {
  if (status === "contradicted") return "err";
  if (status === "supported" || status === "robust") return "accent";
  return "neutral";
}

/** Every finding across topics, with its topic for the meta line. */
export function allFindings(k: Knowledge): { finding: Finding; topic: string; path: string }[] {
  const out: { finding: Finding; topic: string; path: string }[] = [];
  for (const t of k.topics) for (const f of t.findings) out.push({ finding: f, topic: t.slug, path: t.path });
  return out;
}

/** Contradicted first, then by strength, then most recently updated. */
export function whereThingsStand(k: Knowledge, limit = 3): { finding: Finding; topic: string }[] {
  return allFindings(k)
    .sort(
      (a, b) =>
        statusRank(a.finding.status) - statusRank(b.finding.status) ||
        b.finding.updated.localeCompare(a.finding.updated),
    )
    .slice(0, limit);
}

function has(hay: string | undefined, q: string): boolean {
  return hay !== undefined && hay.toLowerCase().includes(q);
}

function hasAny(list: readonly string[] | undefined, q: string): boolean {
  return list !== undefined && list.some((s) => has(s, q));
}

/**
 * The one search box: a case-insensitive substring over every section's
 * text. Returns the same shape with non-matching items dropped (topics
 * with no surviving finding vanish; counts follow). An empty query returns
 * the input untouched (same reference — no re-render churn).
 */
export function searchKnowledge(k: Knowledge, query: string): Knowledge {
  const q = query.trim().toLowerCase();
  if (q === "") return k;
  const topics = k.topics
    .map((t) => ({
      ...t,
      findings: t.findings.filter(
        (f) =>
          has(f.id, q) ||
          has(f.claim, q) ||
          has(f.implications, q) ||
          hasAny(f.tags, q) ||
          hasAny(f.questions, q) ||
          f.ledger.some((r) => has(r.result, q) || has(r.dataset, q) || has(r.run, q)) ||
          has(t.slug, q),
      ),
    }))
    .filter((t) => t.findings.length > 0);
  const decisions = k.decisions.filter(
    (d) =>
      has(d.title, q) ||
      has(d.decision, q) ||
      has(d.context, q) ||
      has(d.rationale, q) ||
      hasAny(d.alternatives, q) ||
      hasAny(d.tags, q),
  );
  const learnings = k.learnings.filter(
    (l) =>
      has(l.title, q) ||
      has(l.what, q) ||
      has(l.why, q) ||
      has(l.resolution, q) ||
      has(l.category, q) ||
      hasAny(l.tags, q),
  );
  const todos = k.todos.filter((t) => has(t.item, q) || has(t.category, q) || has(t.author, q));
  const questions = k.questions.filter((o) => has(o.text, q) || has(o.finding, q));
  const guidance = k.guidance.filter((g) => has(g.label, q) || has(g.description, q) || has(g.path, q));
  // The handoff is searched too — a query it doesn't match hides it, so an
  // empty result reads as empty instead of leaving an unrelated card up.
  const lo = k.left_off;
  const left_off =
    lo !== null &&
    (has(lo.current, q) || has(lo.worked_on, q) || has(lo.decisions, q) || hasAny(lo.next, q) || hasAny(lo.blockers, q))
      ? lo
      : null;
  return {
    ...k,
    left_off,
    topics,
    decisions,
    learnings,
    todos,
    questions,
    guidance,
    counts: {
      findings: topics.reduce((n, t) => n + t.findings.length, 0),
      decisions: decisions.length,
      learnings: learnings.length,
      open: todos.length + questions.length,
    },
  };
}

export type SectionKey = "left" | "found" | "decided" | "watch" | "open" | "guide";

export interface SectionNav {
  key: SectionKey;
  label: string;
  count: number | null;
}

/** The left nav: only sections with content earn a row (empty never shows
 *  chrome); "Where we left off" and Guidance carry no count. */
export function sectionNav(k: Knowledge): SectionNav[] {
  const out: SectionNav[] = [];
  if (k.left_off !== null) out.push({ key: "left", label: "Where we left off", count: null });
  if (k.counts.findings > 0) out.push({ key: "found", label: "What we found", count: k.counts.findings });
  if (k.decisions.length > 0) out.push({ key: "decided", label: "What we decided", count: k.decisions.length });
  if (k.learnings.length > 0) out.push({ key: "watch", label: "Watch out for", count: k.learnings.length });
  if (k.todos.length + k.questions.length > 0)
    out.push({ key: "open", label: "Open", count: k.todos.length + k.questions.length });
  if (k.guidance.length > 0) out.push({ key: "guide", label: "Guidance & memory", count: k.guidance.length });
  return out;
}

/**
 * A finding's recent status move from the Timeline (the cross-link the
 * design asks for): "now supported" / "contradicted today" within 24 h.
 */
export function recentStatusMove(
  findingId: string,
  entries: readonly TimelineEntry[],
  nowMs: number,
): { text: string; tone: Tone } | null {
  const dayAgo = nowMs - 86_400_000;
  for (const e of entries) {
    if (e.kind !== "knowledge" || e.knowledge === undefined || e.knowledge.id !== findingId) continue;
    if (e.ts < dayAgo) break;
    const to = e.knowledge.to;
    if (e.knowledge.change === "new") return { text: "new today", tone: "accent" };
    return {
      text: to === "contradicted" ? "contradicted today" : `now ${to}`,
      tone: to === "contradicted" ? "err" : "accent",
    };
  }
  return null;
}

/** Newest first: ISO dates sort lexically. */
export function byDateDesc<T extends { date: string }>(items: readonly T[]): T[] {
  return [...items].sort((a, b) => b.date.localeCompare(a.date));
}
