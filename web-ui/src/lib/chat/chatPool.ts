/**
 * The chat-session pool: one warm ChatStore + ChatSocket per session id,
 * kept alive across ChatView remounts (live-set eviction, a pane move) so the
 * transcript never re-fetches the whole journal and the socket never drops.
 * The DOM analogue of termPool for the xterm surface — but here the state is
 * plain JS (a reducer + a socket), so we keep the objects, not the DOM.
 *
 * The pane also retains a bounded MRU of bottom-windowed chat DOM for seamless
 * normal tab switches. The pool is the cheaper fallback beyond that cap: a
 * Svelte tree cannot be parked/re-parented like xterm, so an evicted view
 * releases its rendered page while this bounded store + socket remain warm.
 *
 * Non-reactive module state (like termPool): the ChatStore's own $state fields
 * carry reactivity; the pool map itself must never be $state.
 */

import { ChatSocket, type ChatSessionInfo, type SeqEvent } from "./chatWs";
import { disposeChat, POOL_CAP, pool, tick, type ChatTransport } from "./chatPoolRegistry";
import { ChatStore } from "./store.svelte";
import { parseSubagentChatId, type SubagentRef } from "./subagentView";

export { disposeAllChats, disposeChat, syncChatSessions, type ChatTransport } from "./chatPoolRegistry";

let subagentTransport: ((ref: SubagentRef, store: ChatStore) => ChatTransport) | null = null;

/** Registered by the chat chunk at load: how to feed a subagent view's
 *  store. Only a mounted ChatView ever acquires a subagent id, so the
 *  factory is always in place by the time one is needed. */
export function provideSubagentTransport(
  factory: (ref: SubagentRef, store: ChatStore) => ChatTransport,
): void {
  subagentTransport = factory;
}

/** Wire a fresh socket to `store` for `sessionId`, moving the handler set that
 *  used to live in ChatView. The store IS the sink — every handler is a pure
 *  store mutation, so the same wiring works whether the store is new or warm. */
function makeSocket(sessionId: string, store: ChatStore): ChatTransport {
  const subagent = parseSubagentChatId(sessionId);
  if (subagent !== null) {
    if (subagentTransport === null) throw new Error("subagent view opened before the chat chunk loaded");
    return subagentTransport(subagent, store);
  }
  return new ChatSocket(sessionId, {
    onReady: (info: ChatSessionInfo, replayFrom: number, head: number | undefined) =>
      store.onReady(info, replayFrom, head),
    onEvent: (entry: SeqEvent) => store.apply(entry),
    onDegraded: () => store.onDegraded(),
    onExited: (status: number | null) => store.onExited(status),
    onError: (message: string) => store.onFatalError(message),
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
export function acquireChat(sessionId: string): { store: ChatStore; socket: ChatTransport } {
  let entry = pool.get(sessionId);
  if (entry === undefined) {
    const store = new ChatStore();
    entry = {
      store,
      socket: makeSocket(sessionId, store),
      scrollTop: 0,
      atBottom: true,
      renderWindow: null,
      followedVersion: null,
      turnStart: null,
      refs: 0,
      lastUsed: tick.next(),
    };
    pool.set(sessionId, entry);
  } else if (!entry.socket.healthy) {
    // The socket died while parked; heal it without losing the transcript.
    entry.socket.close();
    entry.socket = makeSocket(sessionId, entry.store);
    entry.lastUsed = tick.next();
  } else {
    entry.lastUsed = tick.next();
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
    entry.lastUsed = tick.next();
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

/** Save the bounded block window currently mounted by a ChatView (virtual
 *  coordinates + transcript generation — see the entry field's doc). */
export function saveChatRenderWindow(
  sessionId: string,
  start: number,
  end: number,
  tail: boolean,
  epoch: number,
): void {
  const entry = pool.get(sessionId);
  if (entry !== undefined) entry.renderWindow = { start, end, tail, epoch };
}

/** Last mounted block window, if this session had a rendered view before. */
export function chatRenderWindow(
  sessionId: string,
): { start: number; end: number; tail: boolean; epoch: number } | null {
  const saved = pool.get(sessionId)?.renderWindow;
  return saved === undefined || saved === null ? null : { ...saved };
}

/** Save/read the transcript revision whose live edge the reader reached. */
export function saveChatFollowedVersion(sessionId: string, version: number): void {
  const entry = pool.get(sessionId);
  if (entry !== undefined) entry.followedVersion = version;
}

export function chatFollowedVersion(sessionId: string): number | null {
  return pool.get(sessionId)?.followedVersion ?? null;
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
