/**
 * Per-session composer drafts. A recent chat view remains mounted across an
 * ordinary tab switch, but pane live-set eviction, moves, and page reloads can
 * still remount it. This module keys the draft to the SESSION instead, like
 * composerBus keys its insert registry.
 *
 * Text also layers into sessionStorage (the auth token's precedent) so a
 * page reload keeps it; image attachments stay in-memory only — each is
 * base64 up to 2 MB against a ~5 MB sessionStorage quota.
 */

import type { ImageAttachment } from "./images";
import { writable } from "svelte/store";

export interface Draft {
  text: string;
  images: ImageAttachment[];
}

const drafts = new Map<string, Draft>();

/** Sessions whose current composer state cannot survive a page navigation.
 *  Small text drafts are mirrored to sessionStorage; images, oversized text,
 *  and storage failures stay memory-only and must hold an asset transition. */
export const volatileChatDrafts = writable<Set<string>>(new Set());

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

function setVolatile(sessionId: string, volatile: boolean): void {
  volatileChatDrafts.update((current) => {
    if (volatile === current.has(sessionId)) return current;
    const next = new Set(current);
    if (volatile) next.add(sessionId);
    else next.delete(sessionId);
    return next;
  });
}

function storeText(sessionId: string, text: string): boolean {
  const key = STORAGE_PREFIX + sessionId;
  if (text.length === 0) {
    try {
      sessionStorage.removeItem(key);
    } catch {
      // There is no content to lose even when storage itself is unavailable.
    }
    return true;
  }
  try {
    if (text.length > MAX_STORED_TEXT) {
      sessionStorage.removeItem(key);
      return false;
    }
    const keys = storedKeys();
    if (keys.length >= MAX_STORED_KEYS && !keys.includes(key)) {
      // Over budget: sessionStorage keeps no age order, so shed arbitrary
      // other drafts — the active one is the one that matters.
      for (const k of keys.filter((k2) => k2 !== key).slice(0, keys.length - MAX_STORED_KEYS + 1)) {
        sessionStorage.removeItem(k);
        const evictedSession = k.slice(STORAGE_PREFIX.length);
        // A loaded draft still exists across chat switches but no longer
        // survives navigation after the bounded persistence layer sheds it.
        if (drafts.has(evictedSession)) setVolatile(evictedSession, true);
      }
    }
    sessionStorage.setItem(key, text);
    return true;
  } catch {
    // Quota or storage disabled: the in-memory draft still covers tab switches,
    // but an asset-transition reload must wait or ask explicitly.
    return false;
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
    setVolatile(sessionId, false);
  } else {
    drafts.set(sessionId, { text, images });
    // Over budget: Map keeps insertion order, so shed the oldest OTHER draft
    // (the active session is the one that matters). Add-one-evict-one keeps
    // the map bounded at MAX_MEMORY_DRAFTS.
    if (drafts.size > MAX_MEMORY_DRAFTS) {
      for (const k of drafts.keys()) {
        if (k !== sessionId) {
          drafts.delete(k);
          setVolatile(k, false);
          break;
        }
      }
    }
  }
  const textStored = storeText(sessionId, text);
  setVolatile(sessionId, images.length > 0 || !textStored);
}
