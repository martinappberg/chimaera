/**
 * The chat pool's registry: the warm store+transport entries and the three
 * lifecycle calls the application shell makes (dispose one, drop the dead,
 * drop all). Split from `chatPool.ts` so the always-loaded entry bundle
 * carries only this: acquiring an entry constructs a ChatStore and a socket
 * (the chat chunk), which only a mounted chat/dashboard view ever does.
 */
import type { NativeUiTransport } from "./nativeUi";
import type { ChatStore } from "./store.svelte";
import { parseSubagentChatId } from "./subagentView";

/** What feeds a pooled store: a session's chat socket, or the read-only
 *  reader of one subagent's conversation (keyed by `subagentChatId`) — known
 *  here by shape only. */
export interface ChatTransport {
  readonly nativeUi: NativeUiTransport;
  readonly healthy: boolean;
  send(command: Record<string, unknown>): boolean;
  close(): void;
  /** Reconnect now instead of sitting out a backoff (a chat socket only). */
  retrySoon?(): void;
}

export interface ChatEntry {
  store: ChatStore;
  socket: ChatTransport;
  /** Saved transcript scroll position, restored on the next mount. */
  scrollTop: number;
  atBottom: boolean;
  /** Block range last rendered by ChatView, in trim-stable VIRTUAL
   *  coordinates (array index + the store's trimmedCount at save time), so a
   *  reducer cap trim while the view is unmounted cannot leave the cursor
   *  naming the wrong rows. `epoch` stamps the transcript generation the
   *  coordinates belong to — a journal reset restarts the numbering, so a
   *  cursor from another generation must be discarded, never converted.
   *  Keeping this tiny view cursor separate from the reducer lets an evicted
   *  view restore the same reading window without remounting the entire
   *  transcript. */
  renderWindow: { start: number; end: number; tail: boolean; epoch: number } | null;
  /** Transcript revision the reader has actually followed. This is separate
   *  from the socket sequence: model/rate-limit/control events should not
   *  manufacture a "new activity" badge, and an MRU eviction must not forget
   *  that background output is still unread. */
  followedVersion: number | null;
  /** performance.now() when the current turn started (null when idle). Kept
   *  here, NOT in the reducer, so the elapsed-turn counter survives a remount
   *  (a tab switch mid-turn) without ever leaking a clock into journal replay. */
  turnStart: number | null;
  /** Outstanding acquireChat holds (mounted ChatViews, dashboard rich cards).
   *  LRU eviction only ever touches entries at zero — disposing a held
   *  entry's socket would silently kill a mounted view's event stream. */
  refs: number;
  lastUsed: number;
}

/** Warm entries beyond this many (not currently mounted) are LRU-evicted. */
export const POOL_CAP = 8;

export const pool = new Map<string, ChatEntry>();
/** Monotonic clock stand-in (Date.now is unavailable in some contexts and
 *  irrelevant here — we only need ordering). */
export const tick = {
  value: 0,
  next(): number {
    this.value += 1;
    return this.value;
  },
};

/** Close the socket and drop the entry (a session that ended, toggled to a
 *  terminal, or the app unmounting). Idempotent. */
export function disposeChat(sessionId: string): void {
  const entry = pool.get(sessionId);
  if (entry === undefined) return;
  entry.socket.close();
  entry.store.dispose();
  pool.delete(sessionId);
}

/** Drop every pooled chat whose session is no longer live (mirrors termPool's
 *  syncSessions). Called from App's session-snapshot effect. */
export function syncChatSessions(liveIds: ReadonlySet<string>): void {
  for (const id of [...pool.keys()]) {
    // A subagent view lives exactly as long as the chat that spawned it.
    const owner = parseSubagentChatId(id)?.parentId ?? id;
    if (!liveIds.has(owner)) disposeChat(id);
  }
}

/** Tear the whole pool down (app unmount). */
export function disposeAllChats(): void {
  for (const id of [...pool.keys()]) disposeChat(id);
}
