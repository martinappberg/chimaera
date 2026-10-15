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

/** One delivery into a composer: text for the draft ("" for none) and, with
 *  it, an image for the attachment strip. Text and image travel together so a
 *  region reference's locator and its pixels always land in the SAME composer
 *  — split across two mounted views of a chat, the image would leave later
 *  as a message of its own, with no words. */
type InsertFn = (text: string, placement: InsertPlacement, image?: ImageAttachment) => void;

interface Pending {
  text: string;
  placement: InsertPlacement;
  image?: ImageAttachment;
}

const registry = new Map<string, InsertFn>();
/** The same composers by the view that mounted them (ChatView's token), for
 *  an insert that belongs to one of two mounted views of a chat. */
const byView = new WeakMap<object, InsertFn>();
/** Deliveries queued for a session whose composer hasn't mounted yet (bounded
 *  per session) — a reference dropped onto a chat pane that is still opening
 *  must not be lost to a mount race. */
const pending = new Map<string, Pending[]>();
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
    for (const item of queued) insert(item.text, item.placement, item.image);
  }
  return () => {
    if (registry.get(sessionId) === insert) registry.delete(sessionId);
    if (view !== undefined && byView.get(view) === insert) byView.delete(view);
  };
}

/** Insert text — and optionally an image attachment with it — into a
 *  session's composer: the one in `view` when that view is mounted, else the
 *  session's latest, buffering until one mounts. Always accepted: a
 *  not-yet-mounted composer keeps the delivery and drains it on registration,
 *  so a reference/@term grant onto a slow-to-open chat pane is never dropped. */
export function insertIntoComposer(
  sessionId: string,
  text: string,
  placement: InsertPlacement = "inline",
  view?: object,
  image?: ImageAttachment,
): boolean {
  const insert = (view !== undefined ? byView.get(view) : undefined) ?? registry.get(sessionId);
  if (insert !== undefined) {
    insert(text, placement, image);
    return true;
  }
  const queued = pending.get(sessionId) ?? [];
  queued.push(image !== undefined ? { text, placement, image } : { text, placement });
  while (queued.length > MAX_PENDING) queued.shift();
  pending.set(sessionId, queued);
  return true;
}

/** Attach an image alone (an OS drop onto a chat pane: pixels to the model
 *  now, its uploaded path follows as text) — the same composer and buffer as
 *  text inserts. */
export function attachImageToComposer(sessionId: string, image: ImageAttachment): void {
  insertIntoComposer(sessionId, "", "inline", undefined, image);
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
