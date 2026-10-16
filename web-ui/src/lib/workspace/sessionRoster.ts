/**
 * The pure half of App's roster reconciliation (`applySessions`): kill
 * tombstones, the first-known `created_at` per session and the rail order,
 * and the agent-set change that makes Recents refetch.
 *
 * App keeps the writes (`sessions`, `gotSessions`, `layout`) and the side
 * effects (unread fold, pool disposal, Recents refetch, prune); this module
 * only decides what list it writes and whether the agent set moved. The
 * book's collections are plain (not `$state`): nothing renders them.
 */
import type { Session } from "./sessions";

/**
 * Drop tombstoned sessions from `list`. A session being optimistically killed
 * is never re-added by an intervening snapshot (the daemon hasn't reaped it
 * yet), so the row can't flicker back before it's really gone. A daemon
 * `snapshot` that no longer lists an id is the daemon confirming the retire:
 * that tombstone lifts (mutating `killing`), so a later resume under the same
 * id isn't filtered away. A locally filtered list (`snapshot` false) lacks the
 * id by construction and must not lift its own tombstone.
 */
export function dropTombstoned(
  list: Session[],
  killing: Set<string>,
  snapshot: boolean,
): Session[] {
  if (killing.size === 0) return list;
  if (snapshot) {
    const present = new Set(list.map((s) => s.id));
    for (const id of killing) if (!present.has(id)) killing.delete(id);
  }
  return killing.size > 0 ? list.filter((s) => !killing.has(s.id)) : list;
}

/**
 * Sort `list` in place by each session's first-known non-zero `created_at`
 * (ties by id). The daemon's mid-switch placeholder row carries
 * `created_at: 0` (a sentinel); sorting by it verbatim would teleport a
 * switching session to the rail top, so the first real value is kept in
 * `firstSeen` through placeholders and respawns. `firstSeen` is pruned to the
 * ids in `list`. Returns `list`.
 */
export function sortByFirstCreated(list: Session[], firstSeen: Map<string, number>): Session[] {
  for (const s of list) {
    if (s.created_at !== 0 && !firstSeen.has(s.id)) firstSeen.set(s.id, s.created_at);
  }
  const ids = new Set(list.map((s) => s.id));
  for (const id of firstSeen.keys()) if (!ids.has(id)) firstSeen.delete(id);
  const sortKey = (s: Session): number => firstSeen.get(s.id) ?? s.created_at;
  list.sort((a, b) => sortKey(a) - sortKey(b) || a.id.localeCompare(b.id));
  return list;
}

/** The agent ids in `list`, and whether they differ from `prev` as a set. */
export function agentSetChange(
  prev: ReadonlySet<string>,
  list: readonly Session[],
): { ids: Set<string>; changed: boolean } {
  const ids = new Set(list.filter((s) => s.kind === "agent").map((s) => s.id));
  const changed = ids.size !== prev.size || [...prev].some((id) => !ids.has(id));
  return { ids, changed };
}

/** A create's response joins the roster unless a racing events snapshot
 *  already delivered it: true when `s` is not yet in `list` (App appends). */
export function needsAppend(list: readonly Session[], s: Session): boolean {
  return !list.some((x) => x.id === s.id);
}

/** The bookkeeping `applySessions` carries between snapshots. */
export class SessionRoster {
  /** Sessions being optimistically killed (see `dropTombstoned`). */
  readonly killing = new Set<string>();
  /** First-known non-zero created_at per live id (see `sortByFirstCreated`). */
  readonly #firstCreatedAt = new Map<string, number>();
  /** Agent ids in the previous snapshot. A vanished agent just retired into
   *  the workspace's recents; an appeared one may be a resumed conversation
   *  leaving them — either way the rail section refetches. */
  #prevAgentIds = new Set<string>();

  /** The list App should store, in rail order, and whether the agent set
   *  changed since the last call. */
  reconcile(list: Session[], snapshot: boolean): { list: Session[]; agentsChanged: boolean } {
    const kept = sortByFirstCreated(dropTombstoned(list, this.killing, snapshot), this.#firstCreatedAt);
    const { ids, changed } = agentSetChange(this.#prevAgentIds, kept);
    this.#prevAgentIds = ids;
    return { list: kept, agentsChanged: changed };
  }

  /** Tombstone `ids`. */
  bury(ids: readonly string[]): void {
    for (const id of ids) this.killing.add(id);
  }

  /** The bounded fallback: drop these tombstones; true when any was still
   *  standing (no snapshot confirmed it), so the roster should be refetched. */
  expire(ids: readonly string[]): boolean {
    let lingering = false;
    for (const id of ids) if (this.killing.delete(id)) lingering = true;
    return lingering;
  }
}
