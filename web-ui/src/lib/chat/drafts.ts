/**
 * Per-session composer drafts. The layout mounts exactly one surface per pane
 * and deliberately remounts ChatView (Composer included) per session, so a
 * component-local draft dies on every tab switch — this module keys the draft
 * to the SESSION instead, like composerBus keys its insert registry.
 *
 * Text also layers into sessionStorage (the auth token's precedent) so a
 * page reload keeps it; image attachments stay in-memory only — each is
 * base64 up to 2 MB against a ~5 MB sessionStorage quota.
 */

import type { ImageAttachment } from "./images";

export interface Draft {
  text: string;
  images: ImageAttachment[];
}

const drafts = new Map<string, Draft>();

const STORAGE_PREFIX = "chimaera.chatDraft.";
/** Per-draft sessionStorage cap: a paste larger than this survives tab
 *  switches (in-memory) but not a reload — bounded footprint over lost data. */
const MAX_STORED_TEXT = 64 * 1024;
/** Bound the key count so long-lived tabs touching many sessions can't
 *  accumulate storage without end. */
const MAX_STORED_KEYS = 24;
/** Same bound for the in-memory map — image attachments (up to ~2 MB base64
 *  each) live only here, so an unbounded map is a real memory leak over a
 *  long-lived tab that visits many sessions. */
const MAX_MEMORY_DRAFTS = 24;

function storedKeys(): string[] {
  const keys: string[] = [];
  for (let i = 0; i < sessionStorage.length; i++) {
    const k = sessionStorage.key(i);
    if (k !== null && k.startsWith(STORAGE_PREFIX)) keys.push(k);
  }
  return keys;
}

function storeText(sessionId: string, text: string) {
  const key = STORAGE_PREFIX + sessionId;
  try {
    if (text.length === 0 || text.length > MAX_STORED_TEXT) {
      sessionStorage.removeItem(key);
      return;
    }
    const keys = storedKeys();
    if (keys.length >= MAX_STORED_KEYS) {
      // Over budget: sessionStorage keeps no age order, so shed arbitrary
      // other drafts — the active one is the one that matters.
      for (const k of keys.filter((k2) => k2 !== key).slice(0, keys.length - MAX_STORED_KEYS + 1)) {
        sessionStorage.removeItem(k);
      }
    }
    sessionStorage.setItem(key, text);
  } catch {
    // Quota or storage disabled: the in-memory draft still covers tab switches.
  }
}

/** The saved draft for a session (a reload falls back to the stored text). */
export function loadDraft(sessionId: string): Draft {
  const hit = drafts.get(sessionId);
  if (hit !== undefined) return hit;
  try {
    const text = sessionStorage.getItem(STORAGE_PREFIX + sessionId);
    if (text !== null && text.length > 0) return { text, images: [] };
  } catch {
    // storage disabled — nothing persisted
  }
  return { text: "", images: [] };
}

/** Write-through on every draft change; an empty draft clears both layers
 *  (so a successful send leaves nothing behind). */
export function saveDraft(sessionId: string, text: string, images: ImageAttachment[]) {
  if (text.length === 0 && images.length === 0) {
    drafts.delete(sessionId);
  } else {
    drafts.set(sessionId, { text, images });
    // Over budget: Map keeps insertion order, so shed the oldest OTHER draft
    // (the active session is the one that matters). Add-one-evict-one keeps
    // the map bounded at MAX_MEMORY_DRAFTS.
    if (drafts.size > MAX_MEMORY_DRAFTS) {
      for (const k of drafts.keys()) {
        if (k !== sessionId) {
          drafts.delete(k);
          break;
        }
      }
    }
  }
  storeText(sessionId, text);
}
