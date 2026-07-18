/**
 * The chat-session pool: one warm ChatStore + ChatSocket per session id,
 * kept alive across ChatView remounts (a tab switch, a pane move) so the
 * transcript never re-fetches the whole journal and the socket never drops.
 * The DOM analogue of termPool for the xterm surface — but here the state is
 * plain JS (a reducer + a socket), so we keep the objects, not the DOM.
 *
 * Why a pool and not keep-alive DOM: a Svelte component tree can't be parked
 * and re-parented the way an xterm element can, and rendering every chat tab
 * hidden would hold a full transcript DOM (up to BLOCK_CAP blocks of markdown
 * + tool cards) per tab. Keeping the bounded store + one socket is far cheaper;
 * the remount then only re-renders already-in-memory state.
 *
 * Non-reactive module state (like termPool): the ChatStore's own $state fields
 * carry reactivity; the pool map itself must never be $state (it holds sockets
 * and whole transcripts — proxying it would be pure cost). `generation` below
 * is the one reactive escape hatch, so a view can depend on MEMBERSHIP without
 * the map itself becoming reactive.
 */

import { ChatSocket, type ChatSessionInfo, type SeqEvent } from "./chatWs";
import { ChatStore } from "./store.svelte";

interface ChatEntry {
  store: ChatStore;
  socket: ChatSocket;
  /** Saved transcript scroll position, restored on the next mount. */
  scrollTop: number;
  atBottom: boolean;
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
const POOL_CAP = 8;

const pool = new Map<string, ChatEntry>();
/** Monotonic clock stand-in (Date.now is unavailable in some contexts and
 *  irrelevant here — we only need ordering). */
let tick = 0;
/** Bumped whenever an entry is added or dropped. Reactive readers of pool
 *  membership (hasBackgroundWork) touch this so they re-derive when a store
 *  warms or is evicted — the map itself stays plain, deliberately. */
let generation = $state(0);

/** Wire a fresh socket to `store` for `sessionId`, moving the handler set that
 *  used to live in ChatView. The store IS the sink — every handler is a pure
 *  store mutation, so the same wiring works whether the store is new or warm. */
function makeSocket(sessionId: string, store: ChatStore): ChatSocket {
  return new ChatSocket(sessionId, {
    onReady: (info: ChatSessionInfo, replayFrom: number, head: number | undefined) =>
      store.onReady(info, replayFrom, head),
    onEvent: (entry: SeqEvent) => store.apply(entry),
    onDegraded: () => (store.degraded = true),
    onExited: (status: number | null) => (store.exited = { status }),
    onError: (message: string) => (store.fatalError = message),
    // A refused command is a notice, not a dead pane — the socket keeps
    // reconnecting and the user keeps their transcript.
    onCommandFailed: (message: string) => store.notice(message, "error"),
    onDisconnected: () => store.onDisconnected(),
    lastSeq: () => store.lastSeq,
  });
}

/**
 * Acquire the warm store + socket for `sessionId`, creating them on first use.
 * When a pooled socket is no longer healthy (a prior fatal error, or an
 * exit/degrade that stopped it reconnecting) it is recreated against the
 * surviving store — lastSeq is preserved, so the re-attach gap-replays from
 * the ring rather than refetching the whole journal.
 */
export function acquireChat(sessionId: string): { store: ChatStore; socket: ChatSocket } {
  let entry = pool.get(sessionId);
  if (entry === undefined) {
    const store = new ChatStore();
    entry = {
      store,
      socket: makeSocket(sessionId, store),
      scrollTop: 0,
      atBottom: true,
      turnStart: null,
      refs: 0,
      lastUsed: ++tick,
    };
    pool.set(sessionId, entry);
    generation += 1;
  } else if (!entry.socket.healthy) {
    // The socket died while parked; heal it without losing the transcript.
    entry.socket.close();
    entry.socket = makeSocket(sessionId, entry.store);
    entry.lastUsed = ++tick;
  } else {
    entry.lastUsed = ++tick;
  }
  entry.refs += 1;
  return { store: entry.store, socket: entry.socket };
}

/** Release one hold on the entry, keeping it warm (the socket stays open).
 *  Evicts the least-recently-used PARKED entries (refs === 0) past the cap —
 *  never a held one, so the pool may transiently exceed the cap while more
 *  than POOL_CAP views hold entries at once (dashboard lane + open tabs). */
export function releaseChat(sessionId: string): void {
  const entry = pool.get(sessionId);
  if (entry !== undefined) {
    entry.refs = Math.max(0, entry.refs - 1);
    entry.lastUsed = ++tick;
  }
  if (pool.size > POOL_CAP) {
    const parked = [...pool.entries()]
      .filter(([, e]) => e.refs === 0)
      .sort((a, b) => a[1].lastUsed - b[1].lastUsed);
    for (const [id] of parked.slice(0, Math.min(parked.length, pool.size - POOL_CAP))) {
      disposeChat(id);
    }
  }
}

/** Save the transcript scroll position for restore on the next mount. */
export function saveChatScroll(sessionId: string, scrollTop: number, atBottom: boolean): void {
  const entry = pool.get(sessionId);
  if (entry !== undefined) {
    entry.scrollTop = scrollTop;
    entry.atBottom = atBottom;
  }
}

/** The saved scroll position (defaults to pinned-at-bottom for a fresh entry). */
export function chatScroll(sessionId: string): { scrollTop: number; atBottom: boolean } {
  const entry = pool.get(sessionId);
  return entry !== undefined
    ? { scrollTop: entry.scrollTop, atBottom: entry.atBottom }
    : { scrollTop: 0, atBottom: true };
}

/**
 * Elapsed-turn clock, kept per session so the counter survives a remount.
 * When a turn is running, returns the existing start (stamping `now` on the
 * first call of a turn); when idle, clears and returns null. The caller passes
 * performance.now() so the pool never touches the clock itself.
 */
export function chatTurnStart(sessionId: string, running: boolean, now: number): number | null {
  const entry = pool.get(sessionId);
  if (entry === undefined) return null;
  if (!running) {
    entry.turnStart = null;
    return null;
  }
  entry.turnStart ??= now;
  return entry.turnStart;
}

/** Close the socket and drop the entry (a session that ended, toggled to a
 *  terminal, or the app unmounting). Idempotent. */
export function disposeChat(sessionId: string): void {
  const entry = pool.get(sessionId);
  if (entry === undefined) return;
  entry.socket.close();
  pool.delete(sessionId);
  generation += 1;
}

/**
 * Does this session have background work (backgrounded Bash / workflows)
 * running right now? Warm-store truth ONLY — the wire session row doesn't
 * carry background tasks, so a session with no pooled store answers false
 * rather than guessing. That's the same tier the dashboard card reads, so the
 * rail and the card agree by construction: both cue exactly when the store
 * that knows is warm.
 *
 * Reactive: reads `generation` (membership) and the store's own `$state`, so a
 * `$derived` over it re-runs when either changes.
 */
export function hasBackgroundWork(sessionId: string): boolean {
  void generation;
  const entry = pool.get(sessionId);
  return entry !== undefined && entry.store.backgroundTasks.some((t) => t.status === "running");
}

/** Drop every pooled chat whose session is no longer live (mirrors termPool's
 *  syncSessions). Called from App's session-snapshot effect. */
export function syncChatSessions(liveIds: ReadonlySet<string>): void {
  for (const id of [...pool.keys()]) {
    if (!liveIds.has(id)) disposeChat(id);
  }
}

/** Tear the whole pool down (app unmount). */
export function disposeAllChats(): void {
  for (const id of [...pool.keys()]) disposeChat(id);
}
