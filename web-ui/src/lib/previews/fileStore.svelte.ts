/**
 * The file-content store: one reactive `FileEntry` per open path, keyed in a
 * module-level LRU. Durable state that lives OUTSIDE the Svelte component tree.
 *
 * With `layout/Pane.svelte` now keeping preview VIEWS alive across a tab switch
 * (hidden, not destroyed — the terminal/chat keep-alive model, bounded by its
 * own live-set LRU), this store is no longer what makes switching back instant;
 * the mounted view already holds its rendered DOM. What it still earns:
 *   1. Instant REOPEN of a view the keep-alive set evicted — its bytes are
 *      cached here (CACHE_CAP > the view live-set), so re-mounting re-renders
 *      warm instead of re-hitting the network. The complement to view
 *      keep-alive in the Chrome-tabs model.
 *   2. Live-on-disk update: the daemon monitors only mounted paths and tells the
 *      store exactly which file metadata moved — including repeated edits to an
 *      already-dirty file, ignored/non-repo files, and paths outside the
 *      workspace. Agent/in-app git nudges remain the zero-latency fast path.
 *      The editor guards its own unsaved buffer (see CodeView); the store carries
 *      last-known-on-disk only.
 *
 * Memory lives in the browser tab, not the daemon: each entry holds at most the
 * first 256KB chunk (+ small rendered payloads), and the LRU caps the count.
 * Large files stay streamed: CodeView/TableView still page beyond the first
 * chunk on their own.
 */

import { get } from "svelte/store";
import { fsFile, fsMarkdown, fsRawUrl, fsTable, type FileChunk, type TablePage } from "./files";
import { fsEpoch, lastFsMutation, type FsMutation } from "../workspace/fsEvents";
import {
  lastDiskChange,
  releaseDiskFile,
  retainDiskFile,
} from "../workspace/diskWatch";
import { gitStatus, type GitStatus } from "../workspace/git";
import { getSetting } from "../settings/store.svelte";

/** Max cached paths. Small: only a handful are ever open, the rest is history. */
const CACHE_CAP = 32;
/** Coalesce a burst of fs/git bumps into one revalidation pass. */
const REVALIDATE_DEBOUNCE_MS = 250;
/** Re-mint a `/raw` ticket before the daemon's ~10-minute expiry bites. */
const TICKET_TTL_MS = 8 * 60 * 1000;

/**
 * One cached path. The payload fields are `$state`, so a mounted view that reads
 * `entry.markdown` (etc.) re-renders when a background revalidation swaps it.
 * Only the payloads a view actually asked for are ever populated.
 */
export class FileEntry {
  readonly path: string;
  /** Opaque on-disk mtime token (X-Mtime); the invalidation key. */
  mtime = $state<string | null>(null);
  /** The mounted path was reported absent after it was loaded. */
  missing = $state(false);

  chunk = $state<FileChunk | null>(null);
  chunkError = $state<string | null>(null);

  markdown = $state<string | null>(null);
  markdownError = $state<string | null>(null);

  table = $state<TablePage | null>(null);
  tableError = $state<string | null>(null);

  /** Unauthenticated `/raw` URL for <img>/<iframe>/pdf.js. */
  rawUrl = $state<string | null>(null);
  rawError = $state<string | null>(null);

  /** Whether any payload is cached — i.e. the entry is "warm", not cold. */
  get hasPayload(): boolean {
    return (
      this.chunk !== null || this.markdown !== null || this.table !== null || this.rawUrl !== null
    );
  }

  // --- non-reactive bookkeeping -------------------------------------------
  /** Mounted views holding this entry; an entry with refs>0 is "on screen". */
  refs = 0;
  lastUsed = 0;
  /** When the current rawUrl ticket was minted (for TTL re-mint). */
  rawMintedAt = 0;
  /** Last global unknown-change generation this entry has checked against. */
  seenAllStaleEpoch = 0;
  /** In-flight guards so concurrent readers don't double-fetch. */
  private loading = { mtime: false, chunk: false, md: false, table: false };
  /** Raw consumers must await the same ticket mint, not merely suppress duplicates. */
  private rawLoad: Promise<void> | null = null;

  constructor(path: string) {
    this.path = path;
  }

  private adoptMtime(m: string | null): void {
    if (m !== null) {
      this.mtime = m;
      this.missing = false;
    }
  }

  /** Seed the invalidation token for preview kinds whose payload endpoint does
   *  not carry X-Mtime (rendered markdown/tables, tickets, spreadsheets). */
  async ensureMtime(): Promise<void> {
    if (this.mtime !== null || this.loading.mtime) return;
    this.loading.mtime = true;
    try {
      this.adoptMtime((await fsFile(this.path, 0, 1)).mtime);
    } catch {
      // The payload request owns the visible error; metadata is best-effort.
    } finally {
      this.loading.mtime = false;
    }
  }

  /** Fetch the first 256KB chunk (text/binary sniff) if not already present. */
  async ensureChunk(): Promise<void> {
    if (this.chunk !== null || this.loading.chunk) return;
    this.loading.chunk = true;
    this.chunkError = null;
    try {
      const c = await fsFile(this.path);
      this.chunk = c;
      this.adoptMtime(c.mtime);
    } catch (e) {
      this.chunkError = e instanceof Error ? e.message : "failed to load file";
    } finally {
      this.loading.chunk = false;
    }
  }

  /** Fetch server-rendered markdown HTML if not already present. */
  async ensureMarkdown(): Promise<void> {
    if (this.markdown !== null || this.loading.md) return;
    this.loading.md = true;
    this.markdownError = null;
    try {
      this.markdown = await fsMarkdown(this.path);
    } catch (e) {
      this.markdownError = e instanceof Error ? e.message : "failed to render markdown";
    } finally {
      this.loading.md = false;
    }
  }

  /** Fetch the first table page if not already present. */
  async ensureTable(): Promise<void> {
    if (this.table !== null || this.loading.table) return;
    this.loading.table = true;
    this.tableError = null;
    try {
      this.table = await fsTable(this.path, 0, getSetting("files.tableRowsPerPage"));
    } catch (e) {
      this.tableError = e instanceof Error ? e.message : "failed to load table";
    } finally {
      this.loading.table = false;
    }
  }

  /** Mint (or reuse a still-fresh) `/raw` URL for image/pdf/html surfaces. */
  async ensureRawUrl(): Promise<void> {
    const fresh = this.rawUrl !== null && Date.now() - this.rawMintedAt < TICKET_TTL_MS;
    if (fresh) return;
    if (this.rawLoad !== null) {
      await this.rawLoad;
      return;
    }

    const load = (async (): Promise<void> => {
      this.rawError = null;
      try {
        this.rawUrl = await fsRawUrl(this.path);
        this.rawMintedAt = Date.now();
      } catch (e) {
        this.rawError = e instanceof Error ? e.message : "failed to load file";
      }
    })();
    this.rawLoad = load;
    try {
      await load;
    } finally {
      if (this.rawLoad === load) this.rawLoad = null;
    }
  }

  /**
   * Refetch whatever payloads are populated, swapping each IN PLACE — never
   * null-then-fetch. A null `chunk` would unmount a CodeView keyed off it (via
   * FileView's probe), and a null anything flashes a spinner over a live view.
   * A failed refetch leaves the last-known value. `chunk` is refreshed so a
   * later reopen is correct, but a mounted CodeView owns its own buffer and does
   * not react to it (it live-updates via the mtime watch instead).
   */
  private async refreshPayloads(): Promise<void> {
    // The populated payloads are independent reads — refetch them concurrently
    // so a live refresh costs one round-trip, not the sum. Each keeps its
    // last-known value on failure.
    const jobs: Promise<void>[] = [];
    if (this.chunk !== null) {
      jobs.push(
        fsFile(this.path)
          .then((c) => {
            this.chunk = c;
          })
          .catch(() => {
            /* keep the last-known chunk */
          }),
      );
    }
    if (this.markdown !== null) {
      jobs.push(
        fsMarkdown(this.path)
          .then((h) => {
            this.markdown = h;
          })
          .catch(() => {
            /* keep the last-known html */
          }),
      );
    }
    if (this.table !== null) {
      jobs.push(
        fsTable(this.path, 0, getSetting("files.tableRowsPerPage"))
          .then((t) => {
            this.table = t;
          })
          .catch(() => {
            /* keep the last-known page */
          }),
      );
    }
    if (this.rawUrl !== null) {
      jobs.push(
        fsRawUrl(this.path)
          .then((u) => {
            this.rawUrl = u;
            this.rawMintedAt = Date.now();
          })
          .catch(() => {
            /* keep the last-known url */
          }),
      );
    }
    await Promise.all(jobs);
  }

  /**
   * A disk change was signalled. Cheaply probe the mtime (1-byte read carries
   * X-Mtime); if it moved, refresh the payloads this entry holds so mounted
   * views update in place. Unchanged → no fetch, no view disturbance.
   */
  async revalidate(): Promise<void> {
    try {
      const probed = (await fsFile(this.path, 0, 1)).mtime;
      if (probed === null || probed === this.mtime) return;
      await this.refreshPayloads();
      // Raw-ticket consumers such as PdfView remount on this token. Publish it
      // only after refreshed payloads land, so the remount cannot reuse the old
      // still-fresh ticket in the gap between these two operations.
      this.mtime = probed;
      this.missing = false;
    } catch {
      return; // unreachable/deleted — leave content; the tab-prune path handles death
    } finally {
      this.seenAllStaleEpoch = allStaleEpoch;
      stalePaths.delete(this.path);
    }
  }

  /**
   * An in-app save landed `mtime` on disk. Adopt it (so the live-watch does not
   * treat our own write as an external change) and refresh cached payloads to
   * the saved content, so a reopen / a second pane on this path stays correct.
   */
  noteWrite(mtime: string | null): void {
    this.missing = false;
    if (mtime !== null) this.mtime = mtime;
    void this.refreshPayloads();
  }

  /** Preserve a mounted editor entry while recording that its disk path died. */
  noteMissing(): void {
    this.missing = true;
  }
}

// Plain (non-reactive) module map: entries are looked up imperatively on mount;
// each entry's OWN fields carry the reactivity.
const cache = new Map<string, FileEntry>();
let clock = 0;

/** Get or create the entry for `path`, touching its LRU recency. */
function entryFor(path: string): FileEntry {
  let e = cache.get(path);
  if (e === undefined) {
    e = new FileEntry(path);
    cache.set(path, e);
  }
  e.lastUsed = ++clock;
  return e;
}

/** Evict least-recently-used entries past the cap, never one that's on screen. */
function evict(): void {
  while (cache.size > CACHE_CAP) {
    let oldest: FileEntry | null = null;
    for (const e of cache.values()) {
      if (e.refs === 0 && (oldest === null || e.lastUsed < oldest.lastUsed)) oldest = e;
    }
    if (oldest === null) break; // everything is pinned on screen
    cache.delete(oldest.path);
    stalePaths.delete(oldest.path); // the stale mark dies with the entry
  }
}

/**
 * Claim the entry for `path` for a mounting view (pins it against eviction and
 * marks it on-screen for live revalidation). Pair with `release` on unmount.
 */
export function retain(path: string): FileEntry {
  const e = entryFor(path);
  e.refs += 1;
  retainDiskFile(path);
  evict();
  // A warm entry reclaimed after time off-screen (the keep-alive live set
  // evicted its view, but the bytes are still cached) may have missed a change.
  // Re-probe only when this exact path was marked stale, is currently dirty, or
  // an unknown-path change happened while it was away.
  if (e.hasPayload && shouldRevalidateOnRetain(e)) void e.revalidate();
  return e;
}

/** Drop one view's claim on `path` (an unmounting view). */
export function release(path: string): void {
  const e = cache.get(path);
  if (e !== undefined) e.refs = Math.max(0, e.refs - 1);
  releaseDiskFile(path);
}

/** Tell the store an in-app save changed `path` on disk (see FileEntry.noteWrite). */
export function noteWrite(path: string, mtime: string | null): void {
  cache.get(path)?.noteWrite(mtime);
}

/**
 * A board-epoch nudge arrived on /ws/events (invalidate-and-pull): re-probe
 * every cached board entry through the same coalesced revalidation path disk
 * changes take, so an external board mutation lands in the pane immediately
 * instead of trailing the ~2s disk watcher (which stays as the fallback).
 * The frame names workspaces, not paths, so every board re-probes — each
 * probe is a 1-byte mtime read, and board mutations are rare. The debounce
 * also keeps a pane's OWN /board/edit quiet: its response adopts the new
 * mtime via noteWrite before the coalesced probe runs, which then no-ops.
 */
export function revalidateBoardPaths(): void {
  const boards: string[] = [];
  for (const path of cache.keys()) {
    if (path.toLowerCase().endsWith(".board.json")) boards.push(path);
  }
  if (boards.length > 0) scheduleRevalidate(boards);
}

/** Forget a path entirely (deleted/renamed away — nothing left to cache). */
function forget(path: string): void {
  const e = cache.get(path);
  if (e === undefined) return;
  // A mounted view still holds this entry (refs>0): dropping it now would
  // orphan that view — a later retain() of the same path would mint a SECOND
  // entry while the orphan's release() decrements the new one, driving a live
  // entry's refs to 0. Leave it; a re-created file heals via retain()'s
  // revalidate, and once unreferenced the LRU reaps it.
  if (e.refs > 0) return;
  cache.delete(path);
  stalePaths.delete(path); // the stale mark dies with the entry
}

// --- invalidation: revalidate on-screen paths when the disk changes ----------

let revalidateTimer: ReturnType<typeof setTimeout> | null = null;
let pendingRevalidatePaths = new Set<string>();
let pendingRevalidateAll = false;

/**
 * Paths with warm payloads that may have changed while no view was mounted.
 * They stay marked until the entry revalidates, so an evicted keep-alive view
 * reopens fresh even after the debounce already flushed.
 */
const stalePaths = new Set<string>();
let allStaleEpoch = 0;

function shouldRevalidateOnRetain(e: FileEntry): boolean {
  return (
    dirtyPaths.has(e.path) || stalePaths.has(e.path) || e.seenAllStaleEpoch < allStaleEpoch
  );
}

function markStale(paths: Iterable<string> | "all"): void {
  if (paths === "all") {
    allStaleEpoch += 1;
    pendingRevalidateAll = true;
    return;
  }
  for (const path of paths) {
    if (cache.has(path)) stalePaths.add(path);
    pendingRevalidatePaths.add(path);
  }
}

/** Coalesced revalidation of mounted entries that may have changed. */
function scheduleRevalidate(paths: Iterable<string> | "all"): void {
  markStale(paths);
  if (revalidateTimer !== null) return;
  revalidateTimer = setTimeout(() => {
    revalidateTimer = null;
    const all = pendingRevalidateAll;
    const paths = pendingRevalidatePaths;
    pendingRevalidateAll = false;
    pendingRevalidatePaths = new Set();
    for (const e of cache.values()) {
      if (e.refs > 0 && (all || paths.has(e.path) || stalePaths.has(e.path))) {
        void e.revalidate();
      }
    }
  }, REVALIDATE_DEBOUNCE_MS);
}

/** Apply a precise mutation (create/rename/delete gives us the exact path). */
function applyMutation(m: FsMutation): void {
  if (m.kind === "delete") {
    forget(m.path);
    scheduleRevalidate([m.path]);
  } else if (m.kind === "rename") {
    forget(m.from);
    scheduleRevalidate([m.from, m.to]);
  } else {
    scheduleRevalidate([m.path]);
  }
}

let wired = false;
let lastEpoch = 0;
let lastGitEpoch = -1;
let lastMutationSeq = 0;
let fsMutationPendingEpoch = false;
let lastGitPaths = new Set<string>();
/**
 * Absolute paths the repo currently reports dirty — the gate for git-nudge
 * revalidation (mounted-path fs frames already carry exact changed paths).
 * Captured straight from each git-status snapshot's entries (the same source the
 * file tree's badges read), so only files an agent/tool ACTUALLY changed are
 * ever re-probed; a clean file cannot have moved and is never touched. This is
 * what stops the per-git-tick mass-probe across every open preview.
 */
let dirtyPaths = new Set<string>();

function gitStatusPaths(s: GitStatus): Set<string> {
  const out = new Set<string>();
  for (const e of s.entries) {
    out.add(e.path);
    if (e.orig !== null) out.add(e.orig);
  }
  return out;
}

/**
 * Subscribe once to the fs + git change buses. Idempotent — the first mount that
 * touches the store wires it for the app's life (the subscriptions are process-
 * lived, like the store itself).
 */
function ensureWired(): void {
  if (wired) return;
  wired = true;
  fsEpoch.subscribe((n) => {
    if (n === lastEpoch) return;
    lastEpoch = n;
    if (n === 0) return;
    if (fsMutationPendingEpoch) {
      fsMutationPendingEpoch = false;
      return;
    }
    // Defensive fallback for any future fsEpoch producer that does not publish
    // lastFsMutation with exact paths.
    scheduleRevalidate("all");
  });
  lastFsMutation.subscribe((m) => {
    if (m === null || m.seq === lastMutationSeq) return;
    lastMutationSeq = m.seq;
    fsMutationPendingEpoch = true;
    applyMutation(m);
  });
  lastDiskChange.subscribe((change) => {
    if (change === null) return;
    // The daemon already did the expensive discrimination: only exact mounted
    // files whose metadata moved arrive here. Re-probe X-Mtime before swapping
    // cached payloads so editor conflict handling keeps one source of truth.
    scheduleRevalidate(change.files);
    for (const path of change.removed) {
      cache.get(path)?.noteMissing();
      forget(path);
    }
    // App will close mounted tabs for absent files. Keep their warm entries
    // stale (without probing the known-missing path) so a later recreation at
    // the same name cannot flash the deleted file's cached payload on reopen.
    markStale(change.removed);
  });
  gitStatus.subscribe((s) => {
    // Refresh the dirty set on every status (even a same-epoch replay), so the
    // gate below always reflects the latest snapshot.
    dirtyPaths = s === null ? new Set() : gitStatusPaths(s);
    const epoch = s?.epoch ?? -1;
    if (epoch < 0) {
      lastGitEpoch = -1;
      lastGitPaths = new Set();
      return;
    }
    if (s === null) return;
    if (epoch === lastGitEpoch) return;
    const first = lastGitEpoch < 0;
    lastGitEpoch = epoch;
    const nextPaths = gitStatusPaths(s);
    if (first) {
      lastGitPaths = nextPaths; // the first status is already current
      return;
    }
    const paths = new Set([...lastGitPaths, ...nextPaths]);
    lastGitPaths = nextPaths;
    // When git says the dirty path set is empty but the epoch moved, the write
    // may have been ignored/untracked-by-config; without a path payload from the
    // daemon, mounted previews must fall back to a bounded all-open recheck.
    scheduleRevalidate(paths.size > 0 ? paths : "all");
  });
}

ensureWired();

// Seed the epoch baselines so the very first bump (not the initial value) is what
// triggers a revalidate.
lastEpoch = get(fsEpoch);
