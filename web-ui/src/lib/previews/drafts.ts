/**
 * The unsaved-edit journal: dirty editor text, written about a second after
 * typing stops (and on hide/pagehide) to two places.
 *
 *   1. IndexedDB — fast, works with the tunnel down, and can carry the base
 *      text the draft was typed against so a restore can three-way merge.
 *   2. The daemon (`/fs/drafts`, size-capped under ~/.chimaera) — because a
 *      re-established tunnel is often a new port, i.e. a new browser origin
 *      whose IndexedDB is empty.
 *
 * Every storage call is guarded: a private window, a blocked database, a
 * quota error or an older daemon degrades to "not journaled", which the
 * editor SHOWS — a failure here must never read as "safe".
 */

import {
  fsDraftDelete,
  fsDraftGet,
  fsDraftList,
  fsDraftPut,
  type DraftBody,
  type DraftPutResult,
} from "./files";

export interface DraftRecord {
  /** Absolute path; the IndexedDB key. */
  path: string;
  /** Content hash of the disk version the draft was typed against ("" = unknown). */
  baseHash: string;
  /** That version's text, when this origin journaled it (the daemon keeps none). */
  baseText: string | null;
  text: string;
  updatedMs: number;
}

export interface JournalResult {
  local: boolean;
  remote: DraftPutResult | "failed";
}

const DB_NAME = "chimaera-drafts";
const STORE = "drafts";
/** Bound the local journal: stale drafts (a file deleted elsewhere, a
 *  workspace never reopened) must not accumulate for ever. */
const MAX_LOCAL = 48;
const MAX_AGE_MS = 30 * 24 * 60 * 60 * 1000;

let dbOpen: Promise<IDBDatabase | null> | null = null;

function promised<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error ?? new Error("indexeddb request failed"));
  });
}

function openDb(): Promise<IDBDatabase | null> {
  if (dbOpen !== null) return dbOpen;
  const opening = new Promise<IDBDatabase | null>((resolve) => {
    try {
      if (typeof indexedDB === "undefined") {
        resolve(null);
        return;
      }
      const req = indexedDB.open(DB_NAME, 1);
      req.onupgradeneeded = () => {
        if (!req.result.objectStoreNames.contains(STORE)) {
          req.result.createObjectStore(STORE, { keyPath: "path" });
        }
      };
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => resolve(null);
      req.onblocked = () => resolve(null);
    } catch {
      resolve(null);
    }
  });
  dbOpen = opening.then((db) => {
    // A failed open is retried on the next call (storage may come back);
    // a successful one is pruned once per page.
    if (db === null) dbOpen = null;
    else void prune(db);
    return db;
  });
  return dbOpen;
}

async function withStore<T>(
  mode: IDBTransactionMode,
  work: (store: IDBObjectStore) => Promise<T>,
): Promise<T | null> {
  try {
    const db = await openDb();
    if (db === null) return null;
    const tx = db.transaction(STORE, mode);
    const done = new Promise<void>((resolve, reject) => {
      tx.oncomplete = () => resolve();
      tx.onabort = () => reject(tx.error ?? new Error("indexeddb transaction aborted"));
      tx.onerror = () => reject(tx.error ?? new Error("indexeddb transaction failed"));
    });
    const out = await work(tx.objectStore(STORE));
    await done;
    return out;
  } catch {
    return null;
  }
}

async function prune(db: IDBDatabase): Promise<void> {
  try {
    const tx = db.transaction(STORE, "readwrite");
    const store = tx.objectStore(STORE);
    const all = (await promised(store.getAll())) as DraftRecord[];
    const cutoff = Date.now() - MAX_AGE_MS;
    const byAge = [...all].sort((a, b) => b.updatedMs - a.updatedMs);
    byAge.forEach((r, i) => {
      if (i >= MAX_LOCAL || r.updatedMs < cutoff) store.delete(r.path);
    });
  } catch {
    // pruning is housekeeping; a failure leaves the journal as it was
  }
}

async function localPut(rec: DraftRecord): Promise<boolean> {
  const ok = await withStore("readwrite", async (store) => {
    await promised(store.put(rec));
    return true;
  });
  return ok === true;
}

async function localGet(path: string): Promise<DraftRecord | null> {
  const rec = await withStore("readonly", (store) => promised(store.get(path)));
  return rec !== null && rec !== undefined && typeof (rec as DraftRecord).text === "string"
    ? (rec as DraftRecord)
    : null;
}

async function localDelete(path: string): Promise<void> {
  await withStore("readwrite", async (store) => {
    await promised(store.delete(path));
    return true;
  });
}

/**
 * This daemon answered a drafts route with "no such route": skip the mirror
 * for the rest of the page's life rather than 404 on every journal write and
 * open (a daemon upgrade reloads the page — the build-id check — so the memo
 * cannot outlive the daemon it describes).
 */
let remoteUnsupported = false;

async function remotePut(rec: DraftRecord, keepalive: boolean): Promise<JournalResult["remote"]> {
  if (remoteUnsupported) return "unsupported";
  try {
    // keepalive bodies are capped (~64 KiB) by the browser; larger ones go
    // as a normal request and may not outlive a closing page.
    const small = rec.text.length < 60 * 1024;
    const r = await fsDraftPut(rec.path, rec.baseHash, rec.text, keepalive && small);
    if (r === "unsupported") remoteUnsupported = true;
    return r;
  } catch {
    return "failed";
  }
}

async function remoteFind(path: string): Promise<DraftBody | null> {
  if (remoteUnsupported) return null;
  try {
    const list = await fsDraftList();
    if (list === null) {
      remoteUnsupported = true;
      return null;
    }
    return list.some((d) => d.path === path) ? await fsDraftGet(path) : null;
  } catch {
    return null;
  }
}

/** Journal a dirty buffer to both layers. */
export async function journal(rec: DraftRecord, keepalive = false): Promise<JournalResult> {
  const [local, remote] = await Promise.all([localPut(rec), remotePut(rec, keepalive)]);
  return { local, remote };
}

/** Whether a journal result leaves the text recoverable somewhere. */
export function journaled(r: JournalResult): boolean {
  return r.local || r.remote === "ok";
}

/** Drop both copies (a save of exactly this text landed, or the user discarded). */
export async function clear(path: string, keepalive = false): Promise<void> {
  await Promise.all([
    localDelete(path),
    remoteUnsupported
      ? undefined
      : fsDraftDelete(path, keepalive).catch(() => {
          // offline or refused: a stale mirror is recognized on reopen (it
          // matches the disk and is dropped then) or offered, never applied
        }),
  ]);
}

/**
 * The newest journaled draft for `path` across both layers, or null. When
 * both hold the same text the local copy wins (it may carry the base text).
 */
export async function find(path: string): Promise<DraftRecord | null> {
  const [local, remote] = await Promise.all([localGet(path), remoteFind(path)]);
  if (remote === null) return local;
  const fromRemote: DraftRecord = {
    path,
    baseHash: remote.base_hash,
    baseText:
      local !== null && local.baseHash !== "" && local.baseHash === remote.base_hash
        ? local.baseText
        : null,
    text: remote.text,
    updatedMs: remote.updated_ms,
  };
  if (local === null) return fromRemote;
  if (local.text === remote.text) return local;
  return remote.updated_ms > local.updatedMs ? fromRemote : local;
}
