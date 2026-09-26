/**
 * Pure derivations over Timeline entries, shared by the dashboard's "Since
 * you left" and the Timeline view (one row anatomy, one grouping rule, one
 * "bad news leads" order — design §3/§9). No DOM, no store: everything here
 * is unit-tested in timelineModel.test.ts.
 */
import type { TimelineEntry, TimelineRecorded } from "./timeline.svelte";

/** Consecutive turns of ONE session within this gap merge into one row
 *  ("+N follow-ups"); the store stays per turn. */
export const GROUP_GAP_MS = 10 * 60_000;

export interface TimelineGroup {
  /** Stable render key (kind + the first entry's seq). */
  key: string;
  kind: string;
  /** Oldest first. */
  entries: TimelineEntry[];
  first: TimelineEntry;
  last: TimelineEntry;
  /** Newest timestamp in the group (sort key). */
  ts: number;
  /** When the work began (the first entry's start, else its stamp). */
  startTs: number;
  /** Highest seq in the group. */
  seq: number;
  followUps: number;
  /** A failure or a contradiction — sorts ahead of good news. */
  bad: boolean;
}

/** Slurm end states that mean the job did not do its work. Slurm's own
 *  vocabulary (never relabeled): matched as prefixes because squeue/sacct
 *  can suffix them ("CANCELLED by 1234"). */
const JOB_BAD_PREFIXES = ["FAILED", "CANCELLED", "TIMEOUT", "OUT_OF_MEMORY", "NODE_FAIL", "BOOT_FAIL", "DEADLINE", "PREEMPTED"];

export function jobFailed(state: string | undefined): boolean {
  if (state === undefined) return false;
  const s = state.toUpperCase();
  return JOB_BAD_PREFIXES.some((p) => s.startsWith(p));
}

/** Bad news: a failed command, a failed job, a crashed session, an errored
 *  turn, or a finding that was contradicted. */
export function isBadNews(e: TimelineEntry): boolean {
  switch (e.kind) {
    case "command":
      return e.command !== undefined && e.command.exit !== undefined && e.command.exit !== 0;
    case "job":
      return jobFailed(e.job?.state);
    case "session":
      return true;
    case "episode":
      return e.end === "errored";
    case "knowledge":
      return e.knowledge?.to === "contradicted";
    default:
      return false;
  }
}

function startOf(e: TimelineEntry): number {
  return e.start_ts ?? e.ts;
}

/**
 * Group entries into rows. Episodes of one session chain while each starts
 * within GROUP_GAP_MS of the previous one's end (entries of OTHER sessions
 * in between don't break the chain — they are their own rows); a `session`
 * entry (crash/exit) closes that session's chain. Every other kind is its
 * own row. Output is newest first.
 */
export function groupTimeline(entries: readonly TimelineEntry[]): TimelineGroup[] {
  const asc = [...entries].sort((a, b) => a.ts - b.ts || a.seq - b.seq);
  const groups: TimelineGroup[] = [];
  const open = new Map<string, TimelineGroup>();
  for (const e of asc) {
    if (e.kind === "episode" && e.sid !== undefined) {
      const g = open.get(e.sid);
      if (g !== undefined && startOf(e) - g.last.ts <= GROUP_GAP_MS) {
        g.entries.push(e);
        g.last = e;
        g.ts = Math.max(g.ts, e.ts);
        g.seq = Math.max(g.seq, e.seq);
        g.followUps = g.entries.length - 1;
        g.bad = g.bad || isBadNews(e);
        continue;
      }
      const fresh = single(e);
      open.set(e.sid, fresh);
      groups.push(fresh);
      continue;
    }
    if (e.kind === "session" && e.sid !== undefined) open.delete(e.sid);
    groups.push(single(e));
  }
  return groups.sort((a, b) => b.ts - a.ts || b.seq - a.seq);
}

function single(e: TimelineEntry): TimelineGroup {
  return {
    key: `${e.kind}:${e.seq}`,
    kind: e.kind,
    entries: [e],
    first: e,
    last: e,
    ts: e.ts,
    startTs: startOf(e),
    seq: e.seq,
    followUps: 0,
    bad: isBadNews(e),
  };
}

/** Stable partition: bad news first (each half keeps its newest-first order). */
export function badNewsFirst(groups: readonly TimelineGroup[]): TimelineGroup[] {
  return [...groups.filter((g) => g.bad), ...groups.filter((g) => !g.bad)];
}

/**
 * "Since you left": the rows past this viewer's last look (`baselineSeq`),
 * grouped, bad news leading, capped. `total` is the uncapped row count so
 * the surface can say "+N more · open timeline".
 */
export function sinceYouLeft(
  entries: readonly TimelineEntry[],
  baselineSeq: number,
  limit = 8,
): { rows: TimelineGroup[]; total: number } {
  const fresh = entries.filter((e) => e.seq > baselineSeq);
  const groups = badNewsFirst(groupTimeline(fresh));
  return { rows: groups.slice(0, limit), total: groups.length };
}

/** Evidence folded across a group: files (deduped, capped at 10), the
 *  server's total file count, tool calls, turns, and what got recorded. */
export function groupEvidence(g: TimelineGroup): {
  files: string[];
  filesN: number;
  tools: number;
  turns: number;
  recorded: TimelineRecorded | null;
} {
  const files: string[] = [];
  let filesN = 0;
  let tools = 0;
  let turns = 0;
  // `null as …`: an initializer of plain `null` narrows the binding to
  // `never` inside the loop's self-referencing assignment below.
  let recorded = null as TimelineRecorded | null;
  for (const e of g.entries) {
    const ev = e.evidence;
    turns += ev?.turns ?? 1;
    if (ev === undefined) continue;
    for (const f of ev.files ?? []) if (!files.includes(f) && files.length < 10) files.push(f);
    filesN += ev.files_n ?? ev.files?.length ?? 0;
    tools += ev.tools ?? 0;
    if (ev.recorded !== undefined) {
      const r = ev.recorded;
      recorded = {
        findings: [...(recorded?.findings ?? []), ...(r.findings ?? [])],
        learnings: (recorded?.learnings ?? 0) + (r.learnings ?? 0),
        decisions: (recorded?.decisions ?? 0) + (r.decisions ?? 0),
      };
    }
  }
  // A grouped session's file count is at least the distinct files we saw.
  return { files, filesN: Math.max(filesN, files.length), tools, turns, recorded };
}

/** The group's wall-clock duration: summed turn durations when the wire has
 *  them, else the span from first start to last stamp. */
export function groupDurationMs(g: TimelineGroup): number {
  let sum = 0;
  let known = false;
  for (const e of g.entries) {
    if (typeof e.ms === "number") {
      sum += e.ms;
      known = true;
    }
  }
  if (known) return sum;
  return Math.max(0, g.last.ts - g.startTs);
}

/** "2h 13m" · "38 min" · "12 s" — the dashboard's duration voice. */
export function formatDuration(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s} s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min`;
  const h = Math.floor(m / 60);
  const rem = m % 60;
  return rem === 0 ? `${h}h` : `${h}h ${String(rem).padStart(2, "0")}m`;
}

/** Local clock, "13:41". */
export function formatClock(ts: number, locale?: string): string {
  return new Intl.DateTimeFormat(locale, { hour: "2-digit", minute: "2-digit", hour12: false }).format(
    new Date(ts),
  );
}

const DAY_MS = 86_400_000;

function localDay(ts: number): number {
  const d = new Date(ts);
  return Date.UTC(d.getFullYear(), d.getMonth(), d.getDate()) / DAY_MS;
}

/** "Today" · "Yesterday" · "Mon 22 Sep" · "12 Aug 2025" for older years. */
export function dayLabel(ts: number, nowMs: number, locale?: string): string {
  const diff = localDay(nowMs) - localDay(ts);
  if (diff === 0) return "Today";
  if (diff === 1) return "Yesterday";
  const d = new Date(ts);
  const sameYear = d.getFullYear() === new Date(nowMs).getFullYear();
  return new Intl.DateTimeFormat(locale, {
    weekday: diff < 7 ? "short" : undefined,
    day: "numeric",
    month: "short",
    year: sameYear ? undefined : "numeric",
  }).format(d);
}

export interface DayGroup {
  key: string;
  label: string;
  groups: TimelineGroup[];
}

/** Rows bucketed by local calendar day, newest day first. */
export function dayGroups(groups: readonly TimelineGroup[], nowMs: number, locale?: string): DayGroup[] {
  const out: DayGroup[] = [];
  for (const g of groups) {
    const key = String(localDay(g.ts));
    const last = out[out.length - 1];
    if (last !== undefined && last.key === key) last.groups.push(g);
    else out.push({ key, label: dayLabel(g.ts, nowMs, locale), groups: [g] });
  }
  return out;
}

export type TimelineFilter = "all" | "agents" | "commands" | "jobs" | "knowledge" | "notes" | "problems";

export const FILTER_LABELS: Record<TimelineFilter, string> = {
  all: "All",
  agents: "Agents",
  commands: "Commands",
  jobs: "Jobs",
  knowledge: "Knowledge",
  notes: "Notes",
  problems: "Problems",
};

function filterMatches(g: TimelineGroup, f: TimelineFilter): boolean {
  switch (f) {
    case "all":
      return true;
    case "agents":
      return g.kind === "episode" || g.kind === "session";
    case "commands":
      return g.kind === "command";
    case "jobs":
      return g.kind === "job";
    case "knowledge":
      return g.kind === "knowledge";
    case "notes":
      return g.kind === "note";
    case "problems":
      return g.bad;
  }
}

export function filterGroups(groups: readonly TimelineGroup[], f: TimelineFilter): TimelineGroup[] {
  return groups.filter((g) => filterMatches(g, f));
}

/** Only filters with at least one row are worth a chip ("All" always). */
export function filtersPresent(groups: readonly TimelineGroup[]): TimelineFilter[] {
  const order: TimelineFilter[] = ["all", "agents", "commands", "jobs", "knowledge", "notes", "problems"];
  return order.filter((f) => f === "all" || groups.some((g) => filterMatches(g, f)));
}

/** The command's program name ("snakemake"), env assignments and paths
 *  stripped. */
export function commandHead(text: string): string {
  const words = text.trim().split(/\s+/);
  const first = words.find((w) => !/^[A-Za-z_][A-Za-z0-9_]*=/.test(w)) ?? "";
  const slash = first.lastIndexOf("/");
  return slash >= 0 ? first.slice(slash + 1) : first;
}

/** The command as the row shows it: program + arguments, env assignments
 *  dropped, the program's path stripped, capped for a one-line identity. */
export function commandLabel(text: string, max = 48): string {
  const words = text.trim().split(/\s+/);
  const start = words.findIndex((w) => !/^[A-Za-z_][A-Za-z0-9_]*=/.test(w));
  const rest = start < 0 ? [] : words.slice(start);
  if (rest.length > 0) {
    const slash = rest[0].lastIndexOf("/");
    if (slash >= 0) rest[0] = rest[0].slice(slash + 1);
  }
  const label = rest.join(" ");
  return label.length > max ? `${label.slice(0, max - 1)}…` : label;
}

/** Unread notes addressed to the Mastermind past `seenSeq`. */
export function mastermindInbox(entries: readonly TimelineEntry[], seenSeq: number): TimelineEntry[] {
  return entries.filter((e) => e.kind === "note" && e.note?.to === "mastermind" && e.seq > seenSeq);
}
