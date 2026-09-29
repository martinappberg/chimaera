/**
 * Client for session history and cost (daemon `history/`):
 *
 *   GET /workspaces/{id}/history?before=&q=&agent=&limit=&acts=   every past session
 *   GET /sessions/{id}/edits?workspace_id=                        the agent's own edits
 *   GET /usage?workspace_id=&tz=&days=&weeks=                     cost and token totals
 *   GET /usage/csv?workspace_id=&tz=                              the same, as a file
 *
 * Pulled, never pushed: views fetch while visible and refetch on the rail's
 * `recents` nudge (a session ending is what changes the list). The pure
 * helpers below are the honest-number rules every surface shares — unknown
 * cost reads "—", never $0 — and have their own vitest suite.
 */
import { writable } from "svelte/store";
import { api, ApiError } from "../net/api";
import type { Session } from "./sessions";

/** Bumped by App on the `/ws/events` recents nudge (a session ended): the
 *  history surfaces refetch on it while visible — never a poll. */
export const historyNudge = writable(0);

export function nudgeHistory(): void {
  historyNudge.update((n) => n + 1);
}

export interface HistoryUsage {
  cost_usd: number | null;
  tokens_in: number | null;
  tokens_out: number | null;
  turns: number | null;
}

export interface HistoryReopen {
  /** The native conversation handle to resume, when the agent still has it. */
  resume: string | null;
  ui: "chat" | "term";
  /** Why it can't be reopened, in plain words. */
  gone: string | null;
  /** chimaera's chat journal still exists (the edits can be read). */
  journal: boolean;
}

/** One session record (crates/chimaera-server/src/history `Record`). */
export interface HistoryRecord {
  rid: string;
  id: string;
  agent: string;
  ui: "chat" | "term";
  title?: string;
  first_prompt?: string;
  models?: string[];
  /** "you" · "mastermind" · "restart" · another session's id (a fork). */
  started_by: string;
  started: number;
  ended?: number;
  outcome?: "exited" | "crashed" | "retired";
  files: { n: number; top?: string[] };
  usage: HistoryUsage;
  transcript?: {
    kind: string;
    journal?: string;
    path?: string;
    native?: string;
  };
  /** Git anchors `{start, current, commits[]}` in a repository; null elsewhere. */
  git: { commits?: unknown[] } | null;
  mastermind?: boolean;
  /** Still running (an open record). */
  live: boolean;
  reopen: HistoryReopen | null;
}

export interface HistoryAct {
  ts: number;
  by: string;
  act: string;
  target?: string;
  detail?: string;
}

export interface HistoryPage {
  records: HistoryRecord[];
  more: boolean;
  acts: HistoryAct[] | null;
  epoch: number;
}

export interface FileEdit {
  ts?: number;
  old_text?: string;
  new_text: string;
  truncated?: boolean;
}

export interface SessionEdits {
  /** "chat" (the journal) · "claude" (claude's transcript) · null (no record). */
  source: "chat" | "claude" | null;
  agent: string;
  files: { path: string; edits: FileEdit[] }[];
  edits: number;
  truncated: boolean;
}

export interface UsageAgg {
  sessions: number;
  cost_usd: number;
  cost_sessions: number;
  unknown_cost_sessions: number;
  tokens_in: number;
  tokens_out: number;
  token_sessions: number;
}

export interface UsageReport {
  basis: string;
  now: number;
  totals: UsageAgg;
  today: UsageAgg;
  week: UsageAgg;
  workspaces: (UsageAgg & { id: string; name: string })[];
  by_agent_model: (UsageAgg & { agent: string; model: string | null })[];
  days: (UsageAgg & { day: string })[];
  weeks: (UsageAgg & { week: string })[];
  months: (UsageAgg & { month: string; folded: boolean })[];
  since: number | null;
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

/** The browser's offset in minutes east of UTC (the daemon buckets days by it). */
export function tzOffset(): number {
  return -new Date().getTimezoneOffset();
}

export async function fetchHistory(
  workspaceId: string,
  opts: {
    before?: number;
    q?: string;
    agent?: string;
    limit?: number;
    acts?: boolean;
  } = {},
): Promise<HistoryPage> {
  const q = new URLSearchParams();
  if (opts.before !== undefined) q.set("before", String(opts.before));
  if (opts.q !== undefined && opts.q.trim() !== "") q.set("q", opts.q.trim());
  if (opts.agent !== undefined && opts.agent !== "") q.set("agent", opts.agent);
  if (opts.limit !== undefined) q.set("limit", String(opts.limit));
  if (opts.acts === true) q.set("acts", "true");
  const qs = q.toString();
  const page = await json<Partial<HistoryPage>>(
    await api(
      `/workspaces/${encodeURIComponent(workspaceId)}/history${qs ? `?${qs}` : ""}`,
    ),
  );
  return {
    records: Array.isArray(page.records) ? page.records.filter(isRecord) : [],
    more: page.more === true,
    acts: Array.isArray(page.acts) ? page.acts : null,
    epoch: typeof page.epoch === "number" ? page.epoch : 0,
  };
}

/** Defensive row parse: a malformed row is dropped, never the whole list. */
export function isRecord(raw: unknown): raw is HistoryRecord {
  if (typeof raw !== "object" || raw === null) return false;
  const r = raw as Record<string, unknown>;
  return (
    typeof r.rid === "string" &&
    typeof r.id === "string" &&
    typeof r.agent === "string" &&
    typeof r.started === "number" &&
    typeof r.usage === "object" &&
    r.usage !== null &&
    typeof r.files === "object" &&
    r.files !== null
  );
}

export async function fetchSessionEdits(
  sessionId: string,
  workspaceId: string,
): Promise<SessionEdits> {
  const q = new URLSearchParams({ workspace_id: workspaceId });
  return json<SessionEdits>(
    await api(
      `/sessions/${encodeURIComponent(sessionId)}/edits?${q.toString()}`,
    ),
  );
}

export async function fetchUsage(
  opts: { workspaceId?: string; days?: number; weeks?: number } = {},
): Promise<UsageReport> {
  const q = new URLSearchParams({ tz: String(tzOffset()) });
  if (opts.workspaceId !== undefined) q.set("workspace_id", opts.workspaceId);
  if (opts.days !== undefined) q.set("days", String(opts.days));
  if (opts.weeks !== undefined) q.set("weeks", String(opts.weeks));
  return json<UsageReport>(await api(`/usage?${q.toString()}`));
}

/** Download the CSV export (bearer-authed fetch → a blob the browser saves). */
export async function downloadUsageCsv(workspaceId?: string): Promise<void> {
  const q = new URLSearchParams({ tz: String(tzOffset()) });
  if (workspaceId !== undefined) q.set("workspace_id", workspaceId);
  const res = await api(`/usage/csv?${q.toString()}`);
  if (!res.ok)
    throw new ApiError(res.status, `export failed with status ${res.status}`);
  const blob = await res.blob();
  const url = URL.createObjectURL(blob);
  try {
    const a = document.createElement("a");
    a.href = url;
    a.download = "chimaera-usage.csv";
    a.rel = "noopener";
    document.body.appendChild(a);
    a.click();
    a.remove();
  } finally {
    // Revoke after the click has handed the blob to the download.
    setTimeout(() => URL.revokeObjectURL(url), 10_000);
  }
}

// ---- the honest-number rules -------------------------------------------------

/** Dollars, or "—" when the agent reports no cost. Never "$0.00" for unknown. */
export function formatCost(v: number | null | undefined): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return "—";
  if (v > 0 && v < 0.01) return "<$0.01";
  return `$${v.toFixed(2)}`;
}

/** A token count, compact ("1.2M", "34k"), or "—" when unknown. */
export function formatTokens(v: number | null | undefined): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return "—";
  if (v >= 1_000_000)
    return `${(v / 1_000_000).toFixed(v >= 10_000_000 ? 0 : 1)}M`;
  if (v >= 1_000) return `${(v / 1_000).toFixed(v >= 10_000 ? 0 : 1)}k`;
  return String(Math.round(v));
}

/** "4 min", "1h 20m", "2 d" — how long a session ran. */
export function formatDuration(ms: number): string {
  const mins = Math.max(0, Math.round(ms / 60_000));
  if (mins < 1) return "<1 min";
  if (mins < 60) return `${mins} min`;
  if (mins < 48 * 60) {
    const h = Math.floor(mins / 60);
    const m = mins % 60;
    return m === 0 ? `${h}h` : `${h}h ${String(m).padStart(2, "0")}m`;
  }
  return `${Math.round(mins / (24 * 60))} d`;
}

/** An aggregate's cost, honest about sessions with no cost telemetry. */
export function aggCost(
  a: Pick<UsageAgg, "cost_usd" | "cost_sessions">,
): string {
  return a.cost_sessions === 0 ? "—" : formatCost(a.cost_usd);
}

/** Who started a session, in words. A session id is a fork of that session. */
export function startedByLabel(
  startedBy: string,
  nameOf: (id: string) => string | undefined,
): string {
  switch (startedBy) {
    case "you":
      return "you";
    case "mastermind":
      return "the Mastermind";
    case "restart":
      return "a restart";
    default: {
      const name = nameOf(startedBy);
      return name !== undefined ? `fork of ${name}` : "a fork";
    }
  }
}

/** A record's display title: its own, else its first prompt, else its role
 *  (a Mastermind) or the agent. */
export function recordTitle(
  r: Pick<HistoryRecord, "title" | "first_prompt" | "agent"> & { mastermind?: boolean },
): string {
  return r.title ?? r.first_prompt ?? (r.mastermind === true ? "Mastermind" : r.agent);
}

/** How many commits the record's git anchors carry, or null outside a repository. */
export function commitCount(r: Pick<HistoryRecord, "git">): number | null {
  const commits = r.git?.commits;
  return Array.isArray(commits) ? commits.length : null;
}

// ---- two live sessions, one file --------------------------------------------

export interface SameFile {
  /** The other live session that also wrote `path`. */
  other: string;
  path: string;
}

/**
 * Which live agent sessions share a written file with another live session
 * in the SAME workspace (both `files_touched` lists already ride the wire).
 * Per session, its overlaps — at most `cap`, the first file per partner.
 * Nothing locks: this only feeds a quiet chip.
 */
export function sameFileOverlaps(
  sessions: Iterable<Session>,
  cap = 4,
): Map<string, SameFile[]> {
  const byWs = new Map<string, Session[]>();
  for (const s of sessions) {
    if (s.kind !== "agent" || !s.alive || (s.files_touched?.length ?? 0) === 0)
      continue;
    const list = byWs.get(s.workspace_id) ?? [];
    list.push(s);
    byWs.set(s.workspace_id, list);
  }
  const out = new Map<string, SameFile[]>();
  for (const list of byWs.values()) {
    if (list.length < 2) continue;
    const sets = list.map((s) => new Set(s.files_touched ?? []));
    for (let i = 0; i < list.length; i++) {
      for (let j = i + 1; j < list.length; j++) {
        let shared: string | null = null;
        // The newest shared file of the first list (files_touched is oldest-first).
        const files = list[i].files_touched ?? [];
        for (let k = files.length - 1; k >= 0; k--) {
          if (sets[j].has(files[k])) {
            shared = files[k];
            break;
          }
        }
        if (shared === null) continue;
        for (const [me, other] of [
          [list[i].id, list[j].id],
          [list[j].id, list[i].id],
        ]) {
          const mine = out.get(me) ?? [];
          if (mine.length < cap) mine.push({ other, path: shared });
          out.set(me, mine);
        }
      }
    }
  }
  return out;
}
