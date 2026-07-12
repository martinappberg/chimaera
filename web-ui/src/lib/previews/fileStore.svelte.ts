/**
 * The file-content store: one reactive `FileEntry` per open path, keyed in a
 * module-level LRU. It is the file-side analogue of `terminal/termPool` and the
 * `git.ts` store — durable state that lives OUTSIDE the Svelte component tree, so
 * a preview surviving a pane-tab switch (which unmounts its component,
 * `layout/Pane.svelte`) re-attaches to warm content instead of re-hitting the
 * network. This is what makes "switching back to a file you were just on" instant
 * over an ssh daemon.
 *
 * It also closes a real gap: today no open preview ever reflects a disk change
 * (every view depends only on `path`). The store subscribes to the fs/git change
 * buses and, on a coalesced bump, re-probes the mtime of the paths that are
 * currently ON SCREEN and refreshes their payloads in place — so an agent editing
 * a file you have open updates the view live. The editor guards its own unsaved
 * buffer (see CodeView); the store only carries the last-known-on-disk content.
 *
 * Memory lives in the browser tab, not the daemon: each entry holds at most the
 * first 256KB chunk (+ small rendered payloads), and the LRU caps the count — so
 * this is ~single-digit MB, not the multi-MB-per-tab cost of keeping heavy
 * CodeMirror/PDF views mounted. Large files stay streamed: CodeView/TableView
 * still page beyond the first chunk on their own.
 */

import { get } from "svelte/store";
import { fsFile, fsMarkdown, fsRawUrl, fsTable, type FileChunk, type TablePage } from "./files";
import { fsEpoch, lastFsMutation, type FsMutation } from "../workspace/fsEvents";
import { gitStatus } from "../workspace/git";
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
  /** In-flight guards so concurrent readers don't double-fetch. */
  private loading = { chunk: false, md: false, table: false, raw: false };

  constructor(path: string) {
    this.path = path;
  }

  private adoptMtime(m: string | null): void {
    if (m !== null) this.mtime = m;
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
    if (fresh || this.loading.raw) return;
    this.loading.raw = true;
    this.rawError = null;
    try {
      this.rawUrl = await fsRawUrl(this.path);
      this.rawMintedAt = Date.now();
    } catch (e) {
      this.rawError = e instanceof Error ? e.message : "failed to load file";
    } finally {
      this.loading.raw = false;
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
    let probed: string | null;
    try {
      probed = (await fsFile(this.path, 0, 1)).mtime;
    } catch {
      return; // unreachable/deleted — leave content; the tab-prune path handles death
    }
    if (probed === null || probed === this.mtime) return;
    this.mtime = probed;
    await this.refreshPayloads();
  }

  /**
   * An in-app save landed `mtime` on disk. Adopt it (so the live-watch does not
   * treat our own write as an external change) and refresh cached payloads to
   * the saved content, so a reopen / a second pane on this path stays correct.
   */
  noteWrite(mtime: string | null): void {
    if (mtime !== null) this.mtime = mtime;
    void this.refreshPayloads();
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
  }
}

/**
 * Claim the entry for `path` for a mounting view (pins it against eviction and
 * marks it on-screen for live revalidation). Pair with `release` on unmount.
 */
export function retain(path: string): FileEntry {
  const e = entryFor(path);
  e.refs += 1;
  evict();
  // A warm entry reclaimed after time off-screen may have missed a disk change
  // (scheduleRevalidate only covers entries that were on screen when the bump
  // fired). Re-probe on claim so reopening a file reflects edits made while it
  // sat on another tab. Keyed on any cached payload — NOT on `mtime`, which only
  // the chunk fetch populates (a table/markdown/image/pdf entry is warm with a
  // null mtime, and would otherwise never revalidate on reopen). A cold entry
  // has nothing to revalidate — its ensure*() fetches fresh.
  if (e.hasPayload) void e.revalidate();
  return e;
}

/** Drop one view's claim on `path` (an unmounting view). */
export function release(path: string): void {
  const e = cache.get(path);
  if (e !== undefined) e.refs = Math.max(0, e.refs - 1);
}

/** Tell the store an in-app save changed `path` on disk (see FileEntry.noteWrite). */
export function noteWrite(path: string, mtime: string | null): void {
  cache.get(path)?.noteWrite(mtime);
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
}

// --- invalidation: revalidate on-screen paths when the disk changes ----------

let revalidateTimer: ReturnType<typeof setTimeout> | null = null;

/** Coalesced revalidation of every on-screen entry (refs>0). */
function scheduleRevalidate(): void {
  if (revalidateTimer !== null) return;
  revalidateTimer = setTimeout(() => {
    revalidateTimer = null;
    for (const e of cache.values()) {
      if (e.refs > 0) void e.revalidate();
    }
  }, REVALIDATE_DEBOUNCE_MS);
}

/** Apply a precise mutation (create/rename/delete gives us the exact path). */
function applyMutation(m: FsMutation): void {
  if (m.kind === "delete") forget(m.path);
  else if (m.kind === "rename") forget(m.from);
  // create/rename-target: nothing to invalidate (no stale entry yet); the
  // coarse epoch below revalidates anything on screen under a changed dir.
  scheduleRevalidate();
}

let wired = false;
let lastEpoch = 0;
let lastGitEpoch = -1;
let lastMutationSeq = 0;

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
    if (n !== 0) scheduleRevalidate();
  });
  lastFsMutation.subscribe((m) => {
    if (m === null || m.seq === lastMutationSeq) return;
    lastMutationSeq = m.seq;
    applyMutation(m);
  });
  gitStatus.subscribe((s) => {
    const epoch = s?.epoch ?? -1;
    if (epoch < 0 || epoch === lastGitEpoch) return;
    const first = lastGitEpoch < 0;
    lastGitEpoch = epoch;
    if (!first) scheduleRevalidate(); // the first status is already current
  });
}

ensureWired();

// Seed the epoch baselines so the very first bump (not the initial value) is what
// triggers a revalidate.
lastEpoch = get(fsEpoch);
