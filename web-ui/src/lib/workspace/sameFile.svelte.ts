/**
 * Two LIVE agent sessions in one workspace wrote the same file: the quiet
 * notice the chat's line above the input and the dashboard card show
 * ("qc.py also edited by 'fix normalization' · 3 min ago"). Never on the
 * rail. Derived from the `files_touched` lists already on the wire, so it
 * appears and disappears with the roster (either session ending clears it);
 * the times come from the daemon (`GET /workspaces/{id}/same-file`), fetched
 * only when the set of overlapping pairs changes — never polled. App feeds
 * the roster and registers how to open a session (the `openPath` idiom).
 */
import { api } from "../net/api";
import { sameFileOverlaps, type SameFile } from "./history";
import type { Session } from "./sessions";

export interface SameFileNote extends SameFile {
  /** When the other session wrote the file (ms), when the daemon said. */
  at: number | null;
}

type Opener = (sessionId: string) => void;
let opener: Opener | null = null;

/** App-level wiring (null unregisters). */
export function setSameFileOpener(fn: Opener | null): void {
  opener = fn;
}

function pairKey(session: string, other: string, path: string): string {
  return `${session}\u0000${other}\u0000${path}`;
}

class SameFileState {
  overlaps = $state(new Map<string, SameFile[]>());
  names = $state(new Map<string, string>());
  times = $state(new Map<string, number>());
  #key = "";
  #timer: ReturnType<typeof setTimeout> | null = null;

  /** Feed the live roster (App, on every snapshot) and its display names. */
  update(sessions: Session[], names: ReadonlyMap<string, string>): void {
    const overlaps = sameFileOverlaps(sessions);
    const key = [...overlaps.entries()]
      .flatMap(([s, list]) => list.map((o) => pairKey(s, o.other, o.path)))
      .sort()
      .join("\n");
    const wanted = new Map<string, string>();
    for (const [sid, list] of overlaps) {
      for (const o of list) {
        for (const id of [sid, o.other]) {
          const s = sessions.find((x) => x.id === id);
          if (s !== undefined) wanted.set(id, names.get(id) ?? s.display_name ?? s.name);
        }
      }
    }
    if (key === this.#key) {
      // Same pairs: refresh names only when one changed (a rename).
      if ([...wanted].some(([id, n]) => this.names.get(id) !== n)) this.names = wanted;
      return;
    }
    this.#key = key;
    this.overlaps = overlaps;
    this.names = wanted;
    if (overlaps.size === 0) {
      this.times = new Map();
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
      void this.#fetchTimes([...workspaces], key);
    }, 300);
  }

  async #fetchTimes(workspaces: string[], key: string): Promise<void> {
    const times = new Map<string, number>();
    for (const ws of workspaces) {
      try {
        const res = await api(`/workspaces/${encodeURIComponent(ws)}/same-file`);
        if (!res.ok) continue;
        const body = (await res.json()) as {
          pairs?: { session: string; other: string; path: string; at: number }[];
        };
        for (const p of body.pairs ?? []) {
          if (typeof p.at === "number") times.set(pairKey(p.session, p.other, p.path), p.at);
        }
      } catch {
        // An older daemon or a blip: the notice shows without its time.
      }
    }
    if (key === this.#key) this.times = times;
  }

  /** This session's notes, newest first where the daemon knows the time. */
  notesFor(sessionId: string): SameFileNote[] {
    const list = this.overlaps.get(sessionId) ?? [];
    return list
      .map((o) => ({ ...o, at: this.times.get(pairKey(sessionId, o.other, o.path)) ?? null }))
      .sort((a, b) => (b.at ?? 0) - (a.at ?? 0));
  }

  nameOf(id: string): string {
    return this.names.get(id) ?? id;
  }

  open(sessionId: string): void {
    opener?.(sessionId);
  }
}

export const sameFile = new SameFileState();
