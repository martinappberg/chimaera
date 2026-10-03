/**
 * Insert-into-composer registry: the chat counterpart of typing into a PTY's
 * input. The workbench's reference flows (selection "reference in agent",
 * copy provenance tags, @term: grants) compose text for a session's input —
 * for chat sessions that input is the mounted Composer, not a socket.
 */

import type { ImageAttachment } from "./images";

/** Where inserted text goes in the draft: `inline` joins the draft's last
 *  line after a space (a mention, a provenance tag); `block` starts its own
 *  paragraph (a quoted passage, which must begin a line to read as one). */
export type InsertPlacement = "inline" | "block";

type InsertFn = (text: string, placement: InsertPlacement) => void;

const registry = new Map<string, InsertFn>();
/** The same composers by the view that mounted them (ChatView's token), for
 *  an insert that belongs to one of two mounted views of a chat. */
const byView = new WeakMap<object, InsertFn>();
/** Text queued for a session whose composer hasn't mounted yet (bounded per
 *  session) — a reference dropped onto a chat pane that is still opening must
 *  not be lost to a mount race. */
const pending = new Map<string, { text: string; placement: InsertPlacement }[]>();
const MAX_PENDING = 8;

/** Register a mounted composer's insert function (and, when given, the view
 *  it belongs to); drains anything buffered before it mounted. Returns the
 *  unregister. */
export function registerComposer(sessionId: string, insert: InsertFn, view?: object): () => void {
  registry.set(sessionId, insert);
  if (view !== undefined) byView.set(view, insert);
  const queued = pending.get(sessionId);
  if (queued !== undefined) {
    pending.delete(sessionId);
    for (const item of queued) insert(item.text, item.placement);
  }
  return () => {
    if (registry.get(sessionId) === insert) registry.delete(sessionId);
    if (view !== undefined && byView.get(view) === insert) byView.delete(view);
  };
}

/** Insert text into a session's composer — the one in `view` when that view
 *  is mounted, else the session's latest — buffering until one mounts.
 *  Always accepted: a not-yet-mounted composer keeps the text and drains it
 *  on registration, so a reference/@term grant onto a slow-to-open chat pane
 *  is never dropped. */
export function insertIntoComposer(
  sessionId: string,
  text: string,
  placement: InsertPlacement = "inline",
  view?: object,
): boolean {
  const insert = (view !== undefined ? byView.get(view) : undefined) ?? registry.get(sessionId);
  if (insert !== undefined) {
    insert(text, placement);
    return true;
  }
  const queued = pending.get(sessionId) ?? [];
  queued.push({ text, placement });
  while (queued.length > MAX_PENDING) queued.shift();
  pending.set(sessionId, queued);
  return true;
}

// --- follow the send ------------------------------------------------------------
// A prompt sent from OUTSIDE the composer (the Mastermind panel's Brief me and
// suggestion chips go straight over the socket) is still the user's send: the
// mounted transcript jumps to the bottom exactly as the composer's own submit
// does. No buffering — a transcript that isn't mounted has nothing to scroll.

// A set per session: two mounted views of one chat (the panel and a pane, or
// a remount overlapping its predecessor) both follow, and either unmounting
// leaves the other registered.
const followRegistry = new Map<string, Set<() => void>>();

/** A mounted transcript's "the user just sent" handler. Returns the unregister. */
export function registerFollow(sessionId: string, follow: () => void): () => void {
  let handlers = followRegistry.get(sessionId);
  if (handlers === undefined) {
    handlers = new Set();
    followRegistry.set(sessionId, handlers);
  }
  handlers.add(follow);
  return () => {
    const current = followRegistry.get(sessionId);
    if (current === undefined) return;
    current.delete(follow);
    if (current.size === 0) followRegistry.delete(sessionId);
  };
}

/** Bring a session's mounted transcripts to the bottom and keep them following. */
export function followToBottom(sessionId: string): void {
  const handlers = followRegistry.get(sessionId);
  if (handlers !== undefined) for (const follow of handlers) follow();
}

// --- image attachments (OS drops onto a chat pane) ---------------------------
// Same registry/pending shape as text inserts: the attachment channel exists
// so an image dropped on a chat pane can ride the composer's existing
// attachment plumbing (pixels to the model now) alongside its uploaded-path
// reference (the durable artifact).

const attachRegistry = new Map<string, (image: ImageAttachment) => void>();
const pendingAttach = new Map<string, ImageAttachment[]>();

/** Register a mounted composer's image-attach function; drains anything
 *  buffered before it mounted. Returns the unregister. */
export function registerComposerAttach(
  sessionId: string,
  attach: (image: ImageAttachment) => void,
): () => void {
  attachRegistry.set(sessionId, attach);
  const queued = pendingAttach.get(sessionId);
  if (queued !== undefined) {
    pendingAttach.delete(sessionId);
    for (const image of queued) attach(image);
  }
  return () => {
    if (attachRegistry.get(sessionId) === attach) attachRegistry.delete(sessionId);
  };
}

/** Attach an image to a session's composer, buffering until one mounts. */
export function attachImageToComposer(sessionId: string, image: ImageAttachment): void {
  const attach = attachRegistry.get(sessionId);
  if (attach !== undefined) {
    attach(image);
    return;
  }
  const queued = pendingAttach.get(sessionId) ?? [];
  queued.push(image);
  while (queued.length > MAX_PENDING) queued.shift();
  pendingAttach.set(sessionId, queued);
}

// --- messages that did not arrive ---------------------------------------------
// A send the agent never got comes back to the composer it was written in, by
// itself: nobody asked for it at that moment. So, unlike the inserts above, it
// never takes keyboard focus or moves a caret that is somewhere else, and it
// is all or nothing: a composer with no room for its pictures does not take
// the text either (its caller keeps the whole send until there is room; a
// message must not come back with pictures missing).

/** Messages coming back: their texts (already in the order they were sent,
 *  one paragraph each) and every picture they carried. */
export interface ReturnedSends {
  text: string;
  images: ImageAttachment[];
}

interface ReturnTarget {
  /** Pictures the composer can still take. */
  room(): number;
  /** Put the text above the draft and attach the pictures. Only called when
   *  they fit. */
  take(sends: ReturnedSends): void;
}

/** Every mounted composer of a session, newest last: a chat can be mounted
 *  twice (the Mastermind dock and a pane), and one of them unmounting must
 *  leave the other as the place its messages return to. */
const returnRegistry = new Map<string, ReturnTarget[]>();
/** The same targets by the view that mounted them (ChatView's token). */
const returnByView = new WeakMap<object, ReturnTarget>();

/** Register a mounted composer (and, when given, the view it belongs to) as
 *  where its session's undelivered messages return to. Returns the
 *  unregister. */
export function registerComposerReturn(sessionId: string, target: ReturnTarget, view?: object): () => void {
  returnRegistry.set(sessionId, [...(returnRegistry.get(sessionId) ?? []), target]);
  if (view !== undefined) returnByView.set(view, target);
  return () => {
    const rest = (returnRegistry.get(sessionId) ?? []).filter((mounted) => mounted !== target);
    if (rest.length > 0) returnRegistry.set(sessionId, rest);
    else returnRegistry.delete(sessionId);
    if (view !== undefined && returnByView.get(view) === target) returnByView.delete(view);
  };
}

/** The composer a view's returned messages go to: its own when it is
 *  mounted, else the session's newest. */
function returnTarget(sessionId: string, view?: object): ReturnTarget | undefined {
  return (view !== undefined ? returnByView.get(view) : undefined) ?? returnRegistry.get(sessionId)?.at(-1);
}

/** How many of `drafts` (oldest first) fit the composer now: the longest run
 *  from the front whose pictures it has room for. 0 while no composer is
 *  mounted. */
export function returnableCount(
  sessionId: string,
  drafts: readonly { images: readonly unknown[] }[],
  view?: object,
): number {
  const target = returnTarget(sessionId, view);
  if (target === undefined) return 0;
  let room = target.room();
  let count = 0;
  for (const draft of drafts) {
    if (draft.images.length > room) break;
    room -= draft.images.length;
    count += 1;
  }
  return count;
}

/** Give messages back to the composer. False (and nothing taken) when none
 *  is mounted or their pictures do not fit. */
export function returnToComposer(sessionId: string, sends: ReturnedSends, view?: object): boolean {
  const target = returnTarget(sessionId, view);
  if (target === undefined || sends.images.length > target.room()) return false;
  target.take(sends);
  return true;
}
