/**
 * Insert-into-composer registry: the chat counterpart of typing into a PTY's
 * input. The workbench's reference flows (selection "reference in agent",
 * copy provenance tags, @term: grants) compose text for a session's input —
 * for chat sessions that input is the mounted Composer, not a socket.
 */

import type { ImageAttachment } from "./images";

const registry = new Map<string, (text: string) => void>();
/** Text queued for a session whose composer hasn't mounted yet (bounded per
 *  session) — a reference dropped onto a chat pane that is still opening must
 *  not be lost to a mount race. */
const pending = new Map<string, string[]>();
const MAX_PENDING = 8;

/** Register a mounted composer's insert function; drains anything buffered
 *  before it mounted. Returns the unregister. */
export function registerComposer(sessionId: string, insert: (text: string) => void): () => void {
  registry.set(sessionId, insert);
  const queued = pending.get(sessionId);
  if (queued !== undefined) {
    pending.delete(sessionId);
    for (const text of queued) insert(text);
  }
  return () => {
    if (registry.get(sessionId) === insert) registry.delete(sessionId);
  };
}

/** Insert text into a session's composer, buffering until one mounts. Always
 *  accepted: a not-yet-mounted composer keeps the text and drains it on
 *  registration, so a reference/@term grant onto a slow-to-open chat pane is
 *  never dropped. */
export function insertIntoComposer(sessionId: string, text: string): boolean {
  const insert = registry.get(sessionId);
  if (insert !== undefined) {
    insert(text);
    return true;
  }
  const queued = pending.get(sessionId) ?? [];
  queued.push(text);
  while (queued.length > MAX_PENDING) queued.shift();
  pending.set(sessionId, queued);
  return true;
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
