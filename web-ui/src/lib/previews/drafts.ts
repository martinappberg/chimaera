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
 *
 * Mirror writes for one path run in issue order (`inLane`): the DELETE that
 * follows a save waits for the journal PUT before it, which could otherwise
 * land last and re-create a stale draft. (IndexedDB orders its own
 * readwrite transactions.)
 *
 * Both layers hold ONE draft per path, but several windows may hold the same
 * file dirty. Every record names its `writer` (this page's `WRITER`), and a
 * clear removes only a record this window wrote — or one whose text is known
 * safe to drop (it is on disk, or the user discarded exactly it) — so one
 * window's save or discard never deletes another window's draft.
 */

import {
  draftPutBody,
  fsDraftDelete,
  fsDraftGet,
  fsDraftList,
  fsDraftPut,
  type DraftBody,
  type DraftPutResult,
  type DraftSummary,
} from "./files";

export interface DraftRecord {
  /** Absolute path; the IndexedDB key. */
  path: string;
  /** The window that journaled it (`WRITER`); absent from older records. */
  writer?: string;
  /** Content hash of the disk version the draft was typed against ("" = unknown). */
  baseHash: string;
  /** That version's text, when this origin journaled it (the daemon keeps none). */
  baseText: string | null;
  text: string;
  updatedMs: number;
}

export interface JournalResult {
  local: boolean;
  /** "superseded": a newer write or clear for the path was issued before
   *  this one's turn came, so it was never sent (the newer one counts). */
  remote: DraftPutResult | "failed" | "superseded";
}

/** This page's writer id: stamped on every record it journals. */
export const WRITER = Math.random().toString(36).slice(2) + Date.now().toString(36);

/** Which record a clear may remove: one `writer` wrote, or one holding
 *  exactly `text`. Records naming no writer match only by text. */
export interface DraftMatch {
  writer?: string;
  text?: string;
}

function matches(rec: DraftRecord, m: DraftMatch): boolean {
  return (
    (m.writer !== undefined && rec.writer === m.writer) || (m.text !== undefined && rec.text === m.text)
  );
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

/** Delete `path`'s record if it matches, read and deleted in one transaction. */
async function localDelete(path: string, m: DraftMatch): Promise<void> {
  await withStore("readwrite", async (store) => {
    const rec = (await promised(store.get(path))) as DraftRecord | undefined;
    if (rec !== undefined && typeof rec.text === "string" && matches(rec, m)) {
      await promised(store.delete(path));
    }
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

/** One path's mirror ops: `tail` settles once every op issued so far has;
 *  `seq` counts them, so a queued op can tell a newer one was issued. */
interface Lane {
  tail: Promise<void>;
  seq: number;
}

/** Only paths with an op still pending (a lane is dropped once idle). */
const lanes = new Map<string, Lane>();

const noop = (): void => {};

/**
 * Run `op` once every earlier mirror op for `path` has settled — or at once
 * with `now` (a closing page cannot wait), while later ops still wait for
 * it. `stale()` turns true as soon as a newer op for the path is issued.
 */
function inLane<T>(path: string, op: (stale: () => boolean) => Promise<T>, now = false): Promise<T> {
  let lane = lanes.get(path);
  if (lane === undefined) {
    lane = { tail: Promise.resolve(), seq: 0 };
    lanes.set(path, lane);
  }
  const l = lane;
  const mine = ++l.seq;
  const stale = () => l.seq !== mine;
  const prev = l.tail;
  const run = now ? op(stale) : prev.then(() => op(stale));
  const tail = prev.then(() => run.then(noop, noop));
  l.tail = tail;
  void tail.then(() => {
    if (l.tail === tail && lanes.get(path) === l) lanes.delete(path);
  });
  return run;
}

/**
 * The browser caps the bodies of ALL in-flight keepalive requests together
 * (~64 KiB per page) — and the pagehide flush sends every dirty buffer at
 * once, beside the layout and settings flushes. Drafts use at most this much
 * of it at a time, counted in encoded body bytes; a body that does not fit
 * goes as a normal request (it lands if the page lives long enough), with
 * the IndexedDB copy written regardless.
 */
export const KEEPALIVE_BUDGET_BYTES = 56 * 1024;
let keepaliveInFlight = 0;

/** Reserve keepalive quota for `rec`'s PUT body: its size, or 0 (no fit). */
function reserveKeepalive(rec: DraftRecord): number {
  const room = KEEPALIVE_BUDGET_BYTES - keepaliveInFlight;
  // UTF-8 never has fewer bytes than UTF-16 code units: skip encoding a
  // body that cannot fit anyway.
  if (rec.text.length > room) return 0;
  const bytes = new TextEncoder().encode(
    draftPutBody(rec.path, rec.baseHash, rec.text, rec.updatedMs, WRITER),
  ).length;
  if (bytes > room) return 0;
  keepaliveInFlight += bytes;
  return bytes;
}

async function sendPut(rec: DraftRecord, keepalive: boolean): Promise<JournalResult["remote"]> {
  const reserved = keepalive ? reserveKeepalive(rec) : 0;
  try {
    const r = await mirrorWrite(() =>
      fsDraftPut(rec.path, rec.baseHash, rec.text, rec.updatedMs, WRITER, reserved > 0),
    );
    if (r === "unsupported") remoteUnsupported = true;
    return r;
  } catch {
    return "failed";
  } finally {
    keepaliveInFlight -= reserved;
  }
}

async function remotePut(rec: DraftRecord, keepalive: boolean): Promise<JournalResult["remote"]> {
  if (remoteUnsupported) return "unsupported";
  // A write still queued when a newer write or a clear is issued is moot.
  const put = (alive: boolean) => (stale: () => boolean) =>
    stale() ? Promise.resolve("superseded" as const) : sendPut(rec, alive);
  if (!keepalive || !lanes.has(rec.path)) return inLane(rec.path, put(keepalive), keepalive);
  // Hidden or closing with an op still in flight: queued behind it, this
  // write might never leave a closing page, so send it now — and, if the
  // page lives on, once more in order (the in-flight op may land after it).
  const [first, again] = await Promise.all([
    inLane(rec.path, () => sendPut(rec, true), true),
    inLane(rec.path, put(false)),
  ]);
  return again === "superseded" ? first : again;
}

/**
 * A listing is reused this long: every editable file open looks for a draft,
 * and a restored layout opens several at once. Shared while in flight, and
 * dropped by any mirror write from this window (`mirrorWrite`), so it never
 * hides this window's own drafts; another origin's show up within seconds.
 */
export const LIST_TTL_MS = 3_000;
let listing: { at: number; gen: number; list: Promise<DraftSummary[] | null> } | null = null;
/** Bumped as each of this window's mirror writes starts and settles. */
let writeGen = 0;

function listDrafts(): Promise<DraftSummary[] | null> {
  const now = Date.now();
  if (listing !== null && listing.gen === writeGen && now - listing.at < LIST_TTL_MS) {
    return listing.list;
  }
  const entry = { at: now, gen: writeGen, list: fsDraftList() };
  listing = entry;
  // A failed listing is never reused.
  entry.list.catch(() => {
    if (listing === entry) listing = null;
  });
  return entry.list;
}

/** Run a mirror write, invalidating the cached listing around it. */
async function mirrorWrite<T>(write: () => Promise<T>): Promise<T> {
  writeGen++;
  try {
    return await write();
  } finally {
    writeGen++;
  }
}

async function remoteFind(path: string): Promise<DraftBody | null> {
  if (remoteUnsupported) return null;
  try {
    const list = await listDrafts();
    if (list === null) {
      remoteUnsupported = true;
      return null;
    }
    return list.some((d) => d.path === path) ? await fsDraftGet(path) : null;
  } catch {
    return null;
  }
}

/** Journal a dirty buffer to both layers, as this window's record. */
export async function journal(rec: DraftRecord, keepalive = false): Promise<JournalResult> {
  const mine: DraftRecord = { ...rec, writer: WRITER };
  const [local, remote] = await Promise.all([localPut(mine), remotePut(mine, keepalive)]);
  return { local, remote };
}

/** Whether a journal result leaves the text recoverable somewhere. */
export function journaled(r: JournalResult): boolean {
  return r.local || r.remote === "ok";
}

/**
 * Drop `path`'s draft from both layers where it matches `m` — by default only
 * a record this window wrote. The mirror, which cannot compare text, drops
 * only `m.writer`'s record (any record when `m` names no writer: an older
 * client's draft found on open). The DELETE waits for any journal PUT issued
 * before it.
 */
export async function clear(path: string, m: DraftMatch = { writer: WRITER }, keepalive = false): Promise<void> {
  await Promise.all([
    localDelete(path, m),
    remoteUnsupported
      ? undefined
      : inLane(path, () =>
          mirrorWrite(() => fsDraftDelete(path, m.writer ?? null, keepalive)).catch(() => {
            // offline or refused: a stale mirror is recognized on reopen (it
            // matches the disk and is dropped then) or offered, never applied
          }),
        ),
  ]);
}

/**
 * The newest journaled draft for `path` across both layers, or null. When
 * both hold the same text the local copy wins (it may carry the base text).
 * "Newest" compares writer clocks: the local record's `updatedMs` against the
 * time the mirror's writer sent (`client_updated_ms`) — never the daemon's
 * arrival stamp, which runs on another machine's clock and lags a queued or
 * retried PUT. A mirror record without one (an older writer) falls back to
 * that stamp.
 */
export async function find(path: string): Promise<DraftRecord | null> {
  const [local, remote] = await Promise.all([localGet(path), remoteFind(path)]);
  if (remote === null) return local;
  const remoteMs = remote.client_updated_ms ?? remote.updated_ms;
  const fromRemote: DraftRecord = {
    path,
    baseHash: remote.base_hash,
    baseText:
      local !== null && local.baseHash !== "" && local.baseHash === remote.base_hash
        ? local.baseText
        : null,
    text: remote.text,
    updatedMs: remoteMs,
    writer: remote.writer ?? undefined,
  };
  if (local === null) return fromRemote;
  if (local.text === remote.text) return local;
  return remoteMs > local.updatedMs ? fromRemote : local;
}
