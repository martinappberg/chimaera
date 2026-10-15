/**
 * Another live agent session in the workspace wrote a file this one wrote,
 * recently or in the turn it is running now: the quiet notice the chat's line
 * above the input and the dashboard card show ("qc.py also edited by 'fix
 * normalization' · 3 min ago"). Never on the rail. The roster's
 * `files_touched` lists say which live sessions overlap at all (either
 * session ending clears it); the daemon (`GET /workspaces/{id}/same-file`)
 * says which writes are still current and when they happened — fetched only
 * when the overlapping pairs or those sessions' turn state change, never
 * polled. A note ages out {@link SAME_FILE_FRESH_MS} after the write unless
 * the writer's running turn made it (`history.ts`, the same rule as the
 * daemon's). App feeds the roster and registers how to open a session (the
 * `openPath` idiom).
 */
import { api } from "../net/api";
import {
  currentSameFileNotes,
  nextSameFileExpiry,
  sameFileOverlaps,
  type SameFile,
  type SameFileWrite,
} from "./history";
import type { Session } from "./sessions";

export type SameFileNote = SameFileWrite;

type Opener = (sessionId: string) => void;
let opener: Opener | null = null;

/** App-level wiring (null unregisters). */
export function setSameFileOpener(fn: Opener | null): void {
  opener = fn;
}

function pairKey(session: string, other: string, path: string): string {
  return `${session}\u0000${other}\u0000${path}`;
}

/** What, about a session in an overlap, can change which of its writes are
 *  current: its turn state and the file it last wrote. Nothing that moves
 *  per snapshot (`output_active` flips while a terminal agent streams), or
 *  an overlap would refetch the route continuously. */
function writerSignature(s: Session): string {
  return [s.id, s.agent_state ?? "", s.files_touched?.at(-1) ?? ""].join("\u0000");
}

class SameFileState {
  overlaps = $state(new Map<string, SameFile[]>());
  names = $state(new Map<string, string>());
  /** Session id → the daemon's current writes for it (pairs it overlaps). */
  writes = $state(new Map<string, SameFileWrite[]>());
  /** The clock notes are judged against; advanced when the next one ages out. */
  now = $state(Date.now());
  #key = "";
  #timer: ReturnType<typeof setTimeout> | null = null;
  #expiry: ReturnType<typeof setTimeout> | null = null;

  /** Feed the live roster (App, on every snapshot) and its display names. */
  update(sessions: Session[], names: ReadonlyMap<string, string>): void {
    const overlaps = sameFileOverlaps(sessions);
    const involved = new Set<string>();
    for (const [sid, list] of overlaps) {
      involved.add(sid);
      for (const o of list) involved.add(o.other);
    }
    const key = [
      ...[...overlaps.entries()].flatMap(([s, list]) =>
        list.map((o) => pairKey(s, o.other, o.path)),
      ),
      ...sessions.filter((s) => involved.has(s.id)).map(writerSignature),
    ]
      .sort()
      .join("\n");
    const wanted = new Map<string, string>();
    for (const id of involved) {
      const s = sessions.find((x) => x.id === id);
      if (s !== undefined) wanted.set(id, names.get(id) ?? s.display_name ?? s.name);
    }
    if (key === this.#key) {
      // Same pairs and writers: refresh names only when one changed (a rename).
      if ([...wanted].some(([id, n]) => this.names.get(id) !== n)) this.names = wanted;
      return;
    }
    this.#key = key;
    this.overlaps = overlaps;
    this.names = wanted;
    if (overlaps.size === 0) {
      this.writes = new Map();
      this.#schedule();
      return;
    }
    const workspaces = new Set<string>();
    for (const sid of overlaps.keys()) {
      const ws = sessions.find((s) => s.id === sid)?.workspace_id;
      if (ws !== undefined) workspaces.add(ws);
    }
    if (this.#timer !== null) clearTimeout(this.#timer);
    // Coalesce a burst of snapshots into one ask.
    this.#timer = setTimeout(() => {
      this.#timer = null;
      void this.#fetchWrites([...workspaces], key, overlaps);
    }, 300);
  }

  async #fetchWrites(
    workspaces: string[],
    key: string,
    overlaps: Map<string, SameFile[]>,
  ): Promise<void> {
    const writes = new Map<string, SameFileWrite[]>();
    for (const ws of workspaces) {
      try {
        const res = await api(`/workspaces/${encodeURIComponent(ws)}/same-file`);
        if (!res.ok) continue;
        const body = (await res.json()) as {
          pairs?: { session: string; other: string; path: string; at: number; active?: boolean }[];
        };
        for (const p of body.pairs ?? []) {
          if (typeof p.at !== "number") continue;
          // Only pairs the live roster still overlaps (a session that ended
          // in between is gone from the notice at once).
          if (!(overlaps.get(p.session) ?? []).some((o) => o.other === p.other)) continue;
          const list = writes.get(p.session) ?? [];
          list.push({ other: p.other, path: p.path, at: p.at, active: p.active === true });
          writes.set(p.session, list);
        }
      } catch {
        // A blip: no notes until the next change asks again.
      }
    }
    if (key !== this.#key) return;
    this.writes = writes;
    this.now = Date.now();
    this.#schedule();
  }

  /** One timer, for the next note to age out — not a poll: it fires once per
   *  expiry and re-arms only while a shown note will still expire. */
  #schedule(): void {
    if (this.#expiry !== null) clearTimeout(this.#expiry);
    this.#expiry = null;
    const next = nextSameFileExpiry([...this.writes.values()].flat(), this.now);
    if (next === null) return;
    this.#expiry = setTimeout(
      () => {
        this.#expiry = null;
        this.now = Date.now();
        this.#schedule();
      },
      Math.max(0, next - this.now) + 50,
    );
  }

  /** This session's current notes, newest first. */
  notesFor(sessionId: string): SameFileNote[] {
    return currentSameFileNotes(this.writes.get(sessionId) ?? [], this.now);
  }

  nameOf(id: string): string {
    return this.names.get(id) ?? id;
  }

  open(sessionId: string): void {
    opener?.(sessionId);
  }
}

export const sameFile = new SameFileState();
