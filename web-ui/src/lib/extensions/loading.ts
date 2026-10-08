/**
 * Parts of a workspace that are not here yet, as an optional extension says
 * (through its host indicator mount, `PlaceMount.filesLoading` and
 * `PlaceMount.sessionState`). The file tree shows placeholder rows while the
 * workspace's files are loading, and a conversation shows a placeholder over
 * its transcript (or one quiet line) while it is; everything else stays
 * usable. Nothing but an owner from `loadingOwner` ever writes here, so a
 * build without the extension always reads "nothing loading".
 */
import { derived, writable, type Readable } from "svelte/store";

/** One conversation's state, as the extension says it. `loading`: it is
 *  arriving and its transcript shows a placeholder; `note`: one quiet line
 *  in the extension's own words (shown where the connection line goes). */
export type SessionState = { kind: "loading" } | { kind: "note"; text: string };

interface OwnerState { workspaceId: string; files: boolean; sessions: Map<string, SessionState> }

/** Every live owner's state; the views read the union. */
const owners = writable<ReadonlySet<OwnerState>>(new Set());

/** Workspace ids whose files are still loading. */
export const filesLoading: Readable<ReadonlySet<string>> = derived(owners, (all) =>
  new Set([...all].filter(owner => owner.files).map(owner => owner.workspaceId)));

/** Conversations that are not here yet, by session id. */
export const sessionStates: Readable<ReadonlyMap<string, SessionState>> = derived(owners, (all) => {
  const merged = new Map<string, SessionState>();
  for (const owner of all) for (const [id, state] of owner.sessions) merged.set(id, state);
  return merged;
});

const NOTE_MAX = 240;

/** Reads one `sessionState` argument defensively: anything else clears. */
export function parseSessionState(value: unknown): SessionState | null {
  if (typeof value !== "object" || value === null) return null;
  const state = value as { kind?: unknown; text?: unknown };
  if (state.kind === "loading") return { kind: "loading" };
  if (state.kind === "note" && typeof state.text === "string") {
    const text = state.text.trim();
    // A sentence, bounded and printable; never markup.
    if (text !== "" && text.length <= NOTE_MAX && !/[\u0000-\u001f\u007f]/.test(text)) return { kind: "note", text };
  }
  return null;
}

/** One mount's writer for one workspace. Disposal clears everything it
 *  said, so a retired or failed extension never leaves a placeholder up. */
export function loadingOwner(workspaceId: string): {
  files(loading: boolean): void;
  session(sessionId: string, state: unknown): void;
  dispose(): void;
} {
  const own: OwnerState = { workspaceId, files: false, sessions: new Map() };
  let live = true;
  const publish = () => owners.update(all => new Set(all).add(own));
  return {
    files(loading) {
      if (!live || own.files === (loading === true)) return;
      own.files = loading === true;
      publish();
    },
    session(sessionId, state) {
      if (!live || typeof sessionId !== "string" || sessionId === "") return;
      const next = parseSessionState(state);
      const now = own.sessions.get(sessionId);
      if (next === null ? now === undefined : now !== undefined && now.kind === next.kind && (now.kind !== "note" || (next.kind === "note" && now.text === next.text))) return;
      if (next === null) own.sessions.delete(sessionId); else own.sessions.set(sessionId, next);
      publish();
    },
    dispose() {
      if (!live) return;
      live = false;
      owners.update(all => { const rest = new Set(all); rest.delete(own); return rest; });
    },
  };
}

const CODE = /^[a-z_]{1,64}$/;

/** A session row's additive `transfer` field, passed to the extension as
 *  sent: two closed codes, null when absent or malformed. */
export function sessionTransfer(row: unknown): { state: string; reason: string | null } | null {
  const value = typeof row === "object" && row !== null ? (row as { transfer?: unknown }).transfer : null;
  if (typeof value !== "object" || value === null) return null;
  const { state, reason } = value as { state?: unknown; reason?: unknown };
  if (typeof state !== "string" || !CODE.test(state)) return null;
  return Object.freeze({ state, reason: typeof reason === "string" && CODE.test(reason) ? reason : null });
}
