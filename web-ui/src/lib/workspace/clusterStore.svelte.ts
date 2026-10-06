/**
 * One cache of cluster overviews per host, shared by the home's host row and
 * the cluster page (both read the same entry, so one fetch feeds both).
 *
 * Politeness lives in two places: the shell asks the scheduler at most once
 * a minute per host whatever we call (its cached read fills in between), and
 * callers here pass a floor — the home row never asks more than once a
 * minute; the cluster page asks right after an action. A failed fetch keeps
 * the last overview on screen with its error beside it, never a blank.
 */
import { untrack } from "svelte";
import { clusterOverview, type ClusterOverview } from "../net/native";

export interface ClusterOverviewEntry {
  /** The last overview that arrived (null until one does). */
  overview: ClusterOverview | null;
  /** Client clock when `overview` arrived — with `overview.now_ms`, the
   *  offset that turns the cluster's times into this machine's. */
  receivedAt: number;
  /** Why the last fetch failed (cleared by the next success). */
  error: string | null;
  loading: boolean;
}

let entries = $state.raw<Record<string, ClusterOverviewEntry>>({});
/** Client clock of each host's last ask (the floor's reference). */
const askedAt = new Map<string, number>();
/** Drops an overtaken response (a forced refresh racing the poll). */
const seqs = new Map<string, number>();

/** Untracked: `refresh` is called from polling effects, which must not
 *  come to depend on the entries they write. */
function put(alias: string, next: Partial<ClusterOverviewEntry>): void {
  untrack(() => {
    const prev = entries[alias] ?? { overview: null, receivedAt: 0, error: null, loading: false };
    entries = { ...entries, [alias]: { ...prev, ...next } };
  });
}

export const clusterOverviews = {
  /** The cached entry for a host (undefined until its first fetch starts). */
  entry(alias: string): ClusterOverviewEntry | undefined {
    return entries[alias];
  },

  /**
   * Fetch a host's overview unless one was asked for within `minAgeMs`
   * (0 = now: the page opened, an action finished, the shell said it
   * changed). Never throws — the error lands on the entry.
   */
  async refresh(alias: string, minAgeMs = 0): Promise<void> {
    const now = Date.now();
    const last = askedAt.get(alias);
    if (minAgeMs > 0 && last !== undefined && now - last < minAgeMs) return;
    askedAt.set(alias, now);
    const seq = (seqs.get(alias) ?? 0) + 1;
    seqs.set(alias, seq);
    put(alias, { loading: true });
    try {
      const overview = await clusterOverview(alias);
      if (seqs.get(alias) !== seq) return;
      put(alias, { overview, receivedAt: Date.now(), error: null, loading: false });
    } catch (e) {
      if (seqs.get(alias) !== seq) return;
      put(alias, { error: e instanceof Error ? e.message : String(e), loading: false });
    }
  },

  /** Forget a host (removed, or no longer a cluster). */
  forget(alias: string): void {
    askedAt.delete(alias);
    seqs.set(alias, (seqs.get(alias) ?? 0) + 1);
    untrack(() => {
      if (!(alias in entries)) return;
      const next = { ...entries };
      delete next[alias];
      entries = next;
    });
  },
};

/** The cluster's clock now, from an entry (its times are the cluster's). */
export function clusterNow(entry: ClusterOverviewEntry, clientNow = Date.now()): number {
  if (entry.overview === null) return clientNow;
  return entry.overview.now_ms + (clientNow - entry.receivedAt);
}
