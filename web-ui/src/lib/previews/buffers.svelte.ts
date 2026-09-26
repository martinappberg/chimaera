/**
 * Editor buffers that outlive their views ("never lose an edit", document
 * workbench plan Phase 0). One `Buffer` per path owns the CodeMirror
 * `EditorState` — document, undo history, selection — plus what the editor
 * knows about the file on disk: the base text it was loaded from (for the
 * three-way merge), that version's content hash and mtime token (the save
 * precondition), and the file's byte codec (line breaks, BOM).
 *
 * A mounted `CodeView` attaches a view to the buffer; unmounting detaches it
 * and the buffer lives on while it holds unsaved edits, so closing a pane, the
 * keep-alive cap, a split, zoom, a tab drag, a workspace switch or a parent
 * rename all come back to the same text and undo stack. A clean buffer is
 * forgotten with its last view. `shared/editing`'s `dirtyFiles` mirrors the
 * dirty set, which drives the tab dot, beforeunload and the reload gate.
 *
 * The buffer, not the view, owns the disk relationship: it retains the path's
 * store entry (so the daemon keeps watching it even with no view), reconciles
 * disk changes (clean → reload in place, dirty → merge or conflict), saves
 * with content-hash preconditions and save generations, retries once across
 * a dead link, and journals dirty text (drafts.ts).
 *
 * `EditorState`/`EditorView` are plain fields, never $state (the same rule as
 * xterm instances); the reactive fields are only what views render.
 */

import { untrack } from "svelte";
import { get } from "svelte/store";
import {
  Annotation,
  Compartment,
  EditorState,
  StateEffect,
  Text,
  Transaction,
  type Extension,
  type TransactionSpec,
} from "@codemirror/state";
import { EditorView, keymap, type ViewUpdate } from "@codemirror/view";
import {
  defaultKeymap,
  history,
  historyKeymap,
  indentWithTab,
  isolateHistory,
  undo,
  undoDepth,
} from "@codemirror/commands";
import { indentOnInput } from "@codemirror/language";
import { search, searchKeymap } from "@codemirror/search";
import {
  EDIT_MAX_BYTES,
  FILE_CHUNK,
  FileConflictError,
  fsFile,
  fsWrite,
  isGzipped,
  looksBinary,
  type FileChunk,
  type WriteResult,
} from "./files";
import { ApiError } from "../net/api";
import { noteWrite, release, retain, type FileEntry } from "./fileStore.svelte";
import { daemonLinkUp, setDirty, setEditingHost } from "../shared/editing";
import { lastFsMutation } from "../workspace/fsEvents";
import { getSetting } from "../settings/store.svelte";
import { decodeText, encodeText, StreamingDecoder, type TextCodec } from "./textCodec";
import { lineChanges, merge3 } from "./merge";
import * as drafts from "./drafts";

/** A hung PUT (dead tunnel mid-request) must not hold "saving…" for ever. */
export const SAVE_TIMEOUT_MS = 20_000;
/** A full-file read for a reload or merge. */
const READ_TIMEOUT_MS = 30_000;
/** Journal this long after the last keystroke. */
export const JOURNAL_DELAY_MS = 1_000;
/** The quiet "merged changes from disk" notice fades after this. */
const NOTICE_MS = 12_000;
/** A retry waits at least this long even when the link already reads up. */
const RETRY_MIN_DELAY_MS = 1_500;

export type SaveState = "idle" | "saving" | "retrying" | "offline" | "failed";

/** What the disk holds, read whole (up to the edit cap). */
export interface DiskVersion {
  /** Editor-form text; null when it cannot be shown whole (binary, over the cap). */
  text: string | null;
  codec: TextCodec;
  hash: string | null;
  mtime: string | null;
  /** Why this version is view-only; null = editable text. */
  note: string | null;
  chunk: FileChunk;
}

export type Conflict =
  /** The disk moved and could not be merged with the unsaved edits. */
  | { kind: "changed"; disk: DiskVersion }
  /** The file vanished from disk under the buffer. */
  | { kind: "deleted" };

export interface MergeNotice {
  before: string;
  after: string;
  /** Undo depth right after the merge: "undo" applies only while it is the top. */
  depth: number;
}

/** Tags our own programmatic transactions (loads, reloads, merges). */
const origin = Annotation.define<"load" | "reload" | "merge" | "restore">();

const NOTE_OVER_CAP = "over 1 MB — view only";
const NOTE_GZIP = "compressed file — view only";
const NOTE_BINARY = "binary content — view only";
const NOTE_INVALID = "not valid UTF-8 — view only (saving would corrupt it)";
const NOTE_MIXED = "mixed line endings — view only (saving would normalize them)";

function toDoc(text: string): Text {
  return Text.of(text.split("\n"));
}

function sameCodec(a: TextCodec, b: TextCodec): boolean {
  return a.bom === b.bom && a.eol === b.eol;
}

/** Interpret a whole-file read for `path`. */
export function diskVersion(path: string, chunk: FileChunk): DiskVersion {
  const base = { hash: chunk.hash, mtime: chunk.mtime, chunk };
  if (chunk.truncated || chunk.size > EDIT_MAX_BYTES) {
    return { ...base, text: null, codec: { bom: false, eol: "\n" }, note: NOTE_OVER_CAP };
  }
  if (looksBinary(chunk.bytes)) {
    return { ...base, text: null, codec: { bom: false, eol: "\n" }, note: NOTE_BINARY };
  }
  const d = decodeText(chunk.bytes);
  let note: string | null = null;
  if (isGzipped(path)) note = NOTE_GZIP;
  else if (d.failure === "invalid-utf8") note = NOTE_INVALID;
  else if (d.failure === "mixed-eol") note = NOTE_MIXED;
  return { ...base, text: d.text, codec: d.codec, note };
}

function errorMessage(e: unknown, fallback: string): string {
  return e instanceof Error && e.message !== "" ? e.message : fallback;
}

/** A fetch that never reached the daemon or timed out — the write may or
 *  may not have landed, which the precondition makes safe to retry. */
function isTransport(e: unknown): boolean {
  return !(e instanceof ApiError) && !(e instanceof FileConflictError);
}

/** Resolve once the daemon link reads up (at least `minMs` from now). No
 *  deadline: the save stays visibly "offline" until the link returns. */
function waitForLink(minMs: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(() => {
      const unsub = daemonLinkUp.subscribe((up) => {
        if (!up) return;
        resolve();
        // The first call is synchronous, before `unsub` is assigned.
        queueMicrotask(() => unsub());
      });
    }, minMs);
  });
}

export class Buffer {
  /** Current path (a rename of it or a parent re-keys the buffer). */
  path = $state("");

  // --- what views render ---------------------------------------------------
  /** The whole file is still arriving (a provisional first chunk shows). */
  loading = $state(false);
  editable = $state(false);
  /** Why the buffer is view-only (null while editable). */
  note = $state<string | null>(null);
  dirty = $state(false);
  saveState = $state<SaveState>("idle");
  saveError = $state<string | null>(null);
  /** Bumped by each save that left the buffer clean (the "saved" flash). */
  savedCount = $state(0);
  conflict = $state<Conflict | null>(null);
  /** Bumped when a save is refused because a conflict is open. */
  conflictNudge = $state(0);
  notice = $state<MergeNotice | null>(null);
  /** A journaled draft found on open, awaiting Restore / Discard. */
  recovered = $state<drafts.DraftRecord | null>(null);
  /** The newest dirty text is journaled nowhere (show it, never imply "safe"). */
  journalFailed = $state(false);
  /** View-only paging (files past the edit cap, compressed files). */
  loadedBytes = $state(0);
  totalBytes = $state(0);
  truncated = $state(false);
  loadingMore = $state(false);
  loadError = $state<string | null>(null);

  // --- plain bookkeeping ---------------------------------------------------
  /** Views holding this buffer (mounted CodeViews). */
  refs = 0;
  disposed = false;
  private st: EditorState;
  private view: EditorView | null = null;
  private onSuperseded: (() => void) | null = null;
  private onResume: (() => void) | null = null;
  /** Views a newer one superseded that are still mounted, newest last: when
   *  the live view goes, the newest of them resumes. */
  private standby: { view: EditorView; resume: () => void }[] = [];
  /** The last detached view's scroll position, restored on re-attach. */
  private scroll: StateEffect<unknown> | null = null;
  private codec: TextCodec = { bom: false, eol: "\n" };
  private baseDoc: Text = Text.empty;
  private baseText = "";
  private baseHash: string | null = null;
  private savedMtime: string | null = null;
  /** Bumped whenever the base moves (a save, a merge, a reload); a disk read
   *  that started under an older base is stale. */
  private baseGen = 0;
  /** "Keep mine" on a deleted file: the next save may create it unconditionally. */
  private recreate = false;
  private stream: StreamingDecoder | null = null;
  private readonly editComp = new Compartment();
  private entry: FileEntry;
  private stopWatch: () => void = () => {};
  private saveTask: Promise<boolean> | null = null;
  private checkTask: Promise<void> | null = null;
  private checkAgain = false;
  private journalTimer: ReturnType<typeof setTimeout> | null = null;
  private autosaveTimer: ReturnType<typeof setTimeout> | null = null;
  private noticeTimer: ReturnType<typeof setTimeout> | null = null;
  private journaledText: string | null = null;
  /** Bumped by every journal write and draft clear this buffer issues: a
   *  write that completes after a newer one (or a clear) changes nothing. */
  private journalEpoch = 0;

  constructor(path: string, first: FileChunk) {
    this.path = path;
    this.entry = retain(path);
    const gz = isGzipped(path);
    if (!first.truncated) {
      const disk = diskVersion(path, first);
      this.st = EditorState.create({ doc: this.adoptLoaded(disk), extensions: this.core() });
      if (this.editable) void this.lookForDraft();
    } else {
      // A provisional, view-only first chunk: an under-cap file then loads
      // whole in ONE request (a chunked fill split CRLFs across seams).
      this.st = EditorState.create({ doc: this.startPaging(first), extensions: this.core() });
      this.note = gz || first.size > EDIT_MAX_BYTES ? (gz ? NOTE_GZIP : NOTE_OVER_CAP) : null;
      if (!gz && first.size <= EDIT_MAX_BYTES) {
        this.loading = true;
        void this.loadWhole();
      }
    }
    this.watch();
  }

  /** The canonical state: the attached view's, else the stored one. */
  get current(): EditorState {
    return this.view?.state ?? this.st;
  }

  // --- views -------------------------------------------------------------

  /**
   * The state a new view should start from: this buffer's document, history
   * and selection, reconfigured with the view's own extensions (`viewExts`
   * first so a host language keeps precedence, as before).
   */
  stateFor(viewExts: Extension): EditorState {
    return this.current.update({
      effects: StateEffect.reconfigure.of([viewExts, this.core()]),
    }).state;
  }

  /** Make `view` the live surface. A previous view is superseded (its host
   *  hides it; its keystrokes would no longer reach this buffer) and, if it
   *  gave `onResume`, waits to take over again when this one detaches. */
  attach(view: EditorView, onSuperseded: () => void, onResume?: () => void): void {
    this.standby = this.standby.filter((s) => s.view !== view);
    if (this.view !== null && this.view !== view) {
      this.st = this.view.state;
      this.onSuperseded?.();
      if (this.onResume !== null) this.standby.push({ view: this.view, resume: this.onResume });
    }
    this.view = view;
    this.onSuperseded = onSuperseded;
    this.onResume = onResume ?? null;
    if (this.scroll !== null) {
      view.dispatch({ effects: this.scroll });
      this.scroll = null;
    }
  }

  detach(view: EditorView): void {
    const parked = this.standby.findIndex((s) => s.view === view);
    if (parked >= 0) {
      this.standby.splice(parked, 1);
      return;
    }
    if (this.view !== view) return;
    this.st = view.state;
    this.scroll = view.scrollSnapshot();
    this.view = null;
    this.onSuperseded = null;
    this.onResume = null;
    // A view this one superseded is still mounted (a tab mid-move, a second
    // host of the path): it takes over, re-attaching with the current state.
    this.standby.pop()?.resume();
  }

  /** Buffer-owned extensions, re-added whenever a view (re)configures. */
  private core(): Extension {
    return [
      history(),
      search({ top: true }),
      keymap.of(searchKeymap),
      this.editComp.of(this.editExtensions()),
      EditorView.updateListener.of((u) => this.onViewUpdate(u)),
    ];
  }

  private editExtensions(): Extension {
    return this.editable
      ? [
          keymap.of([
            { key: "Mod-s", run: () => (void this.save(), true), preventDefault: true },
            indentWithTab,
            ...defaultKeymap,
            ...historyKeymap,
          ]),
          indentOnInput(),
          EditorView.editable.of(true),
        ]
      : [EditorState.readOnly.of(true), EditorView.editable.of(false)];
  }

  private setEditable(editable: boolean, note: string | null): void {
    this.note = editable ? null : note;
    if (editable === this.editable) return;
    this.editable = editable;
    this.dispatch({ effects: this.editComp.reconfigure(this.editExtensions()) });
  }

  private onViewUpdate(u: ViewUpdate): void {
    if (u.view !== this.view || !u.docChanged) return;
    const ours = u.transactions.some((tr) => tr.annotation(origin) !== undefined);
    this.afterChange(ours);
  }

  /**
   * A user's edit made outside the editor (a task box ticked in the reading
   * view): through the attached view when there is one, else the stored
   * state — either way it counts as typing (dirty, undoable, autosaved,
   * journaled). False when the buffer can't take an edit now (view-only,
   * still loading).
   */
  edit(spec: TransactionSpec): boolean {
    if (!this.editable || this.loading || this.disposed) return false;
    if (this.view !== null) {
      this.view.dispatch({ ...spec, userEvent: "input" });
      return true;
    }
    const tr = this.st.update({ ...spec, userEvent: "input" });
    this.st = tr.state;
    if (tr.docChanged) this.afterChange(false);
    return true;
  }

  /** Route a transaction through the attached view, else the stored state. */
  private dispatch(spec: TransactionSpec): void {
    if (this.view !== null) {
      this.view.dispatch(spec);
      return;
    }
    const tr = this.st.update(spec);
    this.st = tr.state;
    if (tr.docChanged) this.afterChange(true);
  }

  private afterChange(ours: boolean): void {
    this.refreshDirty();
    if (!ours) {
      // The user typed: the merge notice has done its job.
      this.clearNotice();
      this.scheduleAutosave();
    }
    if (this.dirty) this.scheduleJournal();
  }

  /** Replace the document with `text` as minimal line changes (the cursor
   *  maps through untouched lines). */
  private setText(
    text: string,
    kind: "load" | "reload" | "merge" | "restore",
    undoable: boolean,
  ): void {
    const changes = lineChanges(this.current.doc.toString(), text);
    if (changes.length === 0) return;
    this.dispatch({
      changes,
      annotations: [
        origin.of(kind),
        Transaction.addToHistory.of(undoable),
        ...(undoable ? [isolateHistory.of("full")] : []),
      ],
    });
  }

  private refreshDirty(): void {
    const d = this.editable && !this.current.doc.eq(this.baseDoc);
    if (d === this.dirty) return;
    this.dirty = d;
    setDirty(this.path, d);
    presence.announce(this.path, d);
    if (!d) {
      if (this.journalTimer !== null) clearTimeout(this.journalTimer);
      this.journalTimer = null;
      this.journalFailed = false;
    }
  }

  // --- loading -------------------------------------------------------------

  /** Take `disk` as the loaded version: codec, base, editability. Returns the text. */
  private adoptLoaded(disk: DiskVersion): string {
    this.stream = null;
    this.loadedBytes = disk.chunk.bytes.length;
    this.totalBytes = disk.chunk.size;
    this.truncated = false;
    this.codec = disk.codec;
    this.adoptBase(disk);
    this.editable = disk.note === null;
    this.note = disk.note;
    return disk.text ?? "";
  }

  private adoptBase(disk: DiskVersion): void {
    this.baseGen++;
    this.baseText = disk.text ?? "";
    this.baseDoc = toDoc(this.baseText);
    this.baseHash = disk.hash;
    this.savedMtime = disk.mtime;
    this.recreate = false;
  }

  /** View-only paging from `first`; returns its text. The caller makes the
   *  buffer read-only (setEditable, so a live view is reconfigured too). */
  private startPaging(first: FileChunk): string {
    this.stream = new StreamingDecoder();
    this.loadedBytes = first.bytes.length;
    this.totalBytes = first.size;
    this.truncated = first.truncated;
    this.savedMtime = first.mtime;
    return this.stream.push(first.bytes, !first.truncated);
  }

  private async loadWhole(): Promise<void> {
    try {
      const c = await fsFile(this.path, 0, EDIT_MAX_BYTES, AbortSignal.timeout(READ_TIMEOUT_MS));
      if (this.disposed) return;
      const disk = diskVersion(this.path, c);
      if (disk.text === null) {
        // Grew past the cap (or turned binary) since the first chunk: keep paging.
        this.note = disk.note;
        return;
      }
      const text = this.adoptLoaded(disk);
      this.dispatch({ effects: this.editComp.reconfigure(this.editExtensions()) });
      this.setText(text, "load", false);
      this.refreshDirty();
      if (this.editable) void this.lookForDraft();
    } catch (e) {
      if (!this.disposed) this.loadError = errorMessage(e, "failed to load the file");
    } finally {
      this.loading = false;
      if (!this.disposed) this.diskSignal();
    }
  }

  /** Append the next chunk of a view-only file. */
  async loadMore(): Promise<void> {
    if (this.loadingMore || !this.truncated || this.stream === null) return;
    const stream = this.stream;
    this.loadingMore = true;
    this.loadError = null;
    try {
      const c = await fsFile(this.path, this.loadedBytes, FILE_CHUNK);
      if (this.disposed || this.stream !== stream) return;
      const text = stream.push(c.bytes, !c.truncated || c.bytes.length === 0);
      this.dispatch({
        changes: { from: this.current.doc.length, insert: text },
        annotations: [origin.of("load"), Transaction.addToHistory.of(false)],
      });
      this.loadedBytes += c.bytes.length;
      this.totalBytes = c.size;
      this.truncated = c.truncated && c.bytes.length > 0;
    } catch (e) {
      this.loadError = errorMessage(e, "failed to load more");
    } finally {
      this.loadingMore = false;
    }
  }

  // --- the disk ------------------------------------------------------------

  /** Follow this path's store entry: the daemon's watch moves its mtime token
   *  (or marks it missing) when the file changes under us. */
  private watch(): void {
    const entry = this.entry;
    this.stopWatch = $effect.root(() => {
      $effect(() => {
        void entry.mtime;
        void entry.missing;
        untrack(() => this.diskSignal());
      });
    });
  }

  /** The entry's token moved (or it went missing): check the disk if it no
   *  longer names the version this buffer is based on. */
  diskSignal(): void {
    if (this.disposed || this.loading) return;
    if (this.entry.missing) {
      if (this.dirty) this.conflict = { kind: "deleted" };
      return;
    }
    const m = this.entry.mtime;
    if (m === null || m === this.savedMtime) return;
    void this.checkDisk();
  }

  /** Read the disk and reconcile, one check at a time (and never mid-save:
   *  the save's own outcome decides what the disk now means). */
  private async checkDisk(): Promise<void> {
    if (this.saveTask !== null || this.checkTask !== null) {
      this.checkAgain = true;
      return;
    }
    const task = (async () => {
      const gen = this.baseGen;
      const disk = await this.readDisk();
      if (this.disposed || disk === null) return;
      if (gen !== this.baseGen) {
        // A save (or merge) moved the base while this read was in flight —
        // typically our own write, whose fs event lands within milliseconds.
        // The read may predate it: re-decide from the current tokens instead.
        this.checkAgain = true;
        return;
      }
      if (disk === "missing") {
        if (this.dirty) this.conflict = { kind: "deleted" };
        return;
      }
      this.reconcile(disk);
    })();
    this.checkTask = task;
    try {
      await task;
    } finally {
      this.checkTask = null;
      this.settle();
    }
  }

  /** After a save or check: run a check that was deferred behind it. */
  private settle(): void {
    if (this.disposed) return;
    if (this.checkAgain && this.saveTask === null && this.checkTask === null) {
      this.checkAgain = false;
      this.diskSignal();
    }
    this.maybeDispose();
  }

  private async readDisk(): Promise<DiskVersion | "missing" | null> {
    try {
      const c = await fsFile(this.path, 0, EDIT_MAX_BYTES, AbortSignal.timeout(READ_TIMEOUT_MS));
      return diskVersion(this.path, c);
    } catch (e) {
      if (e instanceof ApiError && (e.status === 404 || e.status === 400)) return "missing";
      return null; // transient: the next signal re-checks
    }
  }

  /** Whether `disk` is exactly the version this buffer was based on. */
  private isBase(disk: DiskVersion): boolean {
    if (disk.text === null || disk.note !== null || !sameCodec(disk.codec, this.codec)) return false;
    if (disk.hash !== null && this.baseHash !== null) return disk.hash === this.baseHash;
    return disk.text === this.baseText;
  }

  /**
   * The disk moved. The decision uses the document as it is NOW (after the
   * read's await), so a keystroke that landed during the fetch is never
   * clobbered, and the disk token is adopted only together with the content
   * it describes — never ahead of it.
   */
  private reconcile(disk: DiskVersion): void {
    if (this.conflict?.kind === "deleted") this.conflict = null;
    if (this.isBase(disk)) {
      // Metadata only (a touch, our own write seen late) — or the disk went
      // back to our base, which also settles an open conflict.
      this.savedMtime = disk.mtime;
      if (disk.hash !== null) this.baseHash = disk.hash;
      this.conflict = null;
      return;
    }
    if (!this.dirty) {
      this.takeDisk(disk, false);
      return;
    }
    const mine = this.current.doc.toString();
    if (disk.text === null || disk.note !== null) {
      this.conflict = { kind: "changed", disk };
      return;
    }
    if (disk.text === mine) {
      // The disk already holds exactly these edits.
      this.codec = disk.codec;
      this.adoptBase(disk);
      this.conflict = null;
      this.refreshDirty();
      this.afterClean();
      return;
    }
    const r = merge3(this.baseText, mine, disk.text);
    if (!r.clean) {
      this.conflict = { kind: "changed", disk };
      return;
    }
    this.conflict = null;
    this.codec = disk.codec;
    this.adoptBase(disk);
    this.setText(r.text, "merge", true);
    this.refreshDirty();
    this.showNotice({ before: mine, after: r.text, depth: undoDepth(this.current) });
    if (!this.dirty) this.afterClean();
  }

  /** Make the disk version the buffer's content and base. */
  private takeDisk(disk: DiskVersion, undoable: boolean): void {
    this.conflict = null;
    this.clearNotice();
    if (disk.text === null) {
      // No longer editable as a whole: show it view-only, paging as usual.
      const text = this.startPaging(disk.chunk);
      this.baseGen++;
      this.baseText = text;
      this.baseDoc = toDoc(text);
      this.baseHash = null;
      this.setEditable(false, disk.note);
      this.setText(text, "reload", undoable);
    } else {
      this.codec = disk.codec;
      this.adoptBase(disk);
      this.stream = null;
      this.truncated = false;
      this.loadedBytes = disk.chunk.bytes.length;
      this.totalBytes = disk.chunk.size;
      this.setEditable(disk.note === null, disk.note);
      this.setText(disk.text, "reload", undoable);
    }
    this.refreshDirty();
    if (!this.dirty) this.afterClean();
  }

  // --- conflict actions ----------------------------------------------------

  /** Keep my text; the disk version becomes the base, so the next save
   *  deliberately replaces it. */
  keepMine(): void {
    const c = this.conflict;
    if (c === null) return;
    if (c.kind === "deleted") {
      this.recreate = true;
    } else {
      // The disk's base, my codec: this save rewrites the file as I have it.
      this.adoptBase(c.disk);
    }
    this.conflict = null;
    this.refreshDirty();
    this.scheduleAutosave();
  }

  /** Throw my edits away for the disk version (undoable in the editor). */
  takeDiskVersion(): void {
    const c = this.conflict;
    if (c === null) return;
    if (c.kind === "deleted") {
      this.discard();
      this.conflict = { kind: "deleted" };
      return;
    }
    this.takeDisk(c.disk, true);
  }

  /** The unsaved edits are dropped: back to the base text, journal cleared. */
  discard(): void {
    this.recovered = null;
    this.conflict = null;
    this.clearNotice();
    if (this.editable) this.setText(this.baseText, "reload", true);
    this.refreshDirty();
    this.clearDraft(this.path);
    this.maybeDispose();
  }

  // --- merge notice ------------------------------------------------------------

  private showNotice(n: MergeNotice): void {
    this.notice = n;
    if (this.noticeTimer !== null) clearTimeout(this.noticeTimer);
    this.noticeTimer = setTimeout(() => this.clearNotice(), NOTICE_MS);
  }

  clearNotice(): void {
    if (this.noticeTimer !== null) clearTimeout(this.noticeTimer);
    this.noticeTimer = null;
    this.notice = null;
  }

  /** Undo the merge (only while it is still the newest undo step). */
  undoMerge(): void {
    const n = this.notice;
    if (n === null || undoDepth(this.current) !== n.depth) return;
    const target = this.view ?? {
      state: this.st,
      dispatch: (tr: Transaction) => {
        this.st = tr.state;
        this.afterChange(true);
      },
    };
    undo(target);
    this.clearNotice();
  }

  // --- saving --------------------------------------------------------------

  /**
   * Save the document as it is now. Resolves true once that text is on disk
   * (the buffer may still be dirty if keys landed meanwhile — those stay
   * unsaved). A save while a conflict is open is refused, never a silent
   * overwrite. `auto` = the autosave timer / blur (no nudge on refusal).
   */
  async save(auto = false): Promise<boolean> {
    while (this.saveTask !== null) await this.saveTask;
    if (this.disposed) return false;
    if (this.conflict !== null && !(this.conflict.kind === "deleted" && this.recreate)) {
      if (!auto) this.conflictNudge++;
      return false;
    }
    if (!this.editable) return !this.dirty;
    if (!this.dirty && !this.recreate) return true;
    const task = this.runSave();
    this.saveTask = task;
    let ok = false;
    try {
      ok = await task;
    } finally {
      this.saveTask = null;
      this.settle();
    }
    // Keys that landed mid-save get their own autosave; a FAILED save is not
    // re-armed here (the next keystroke re-arms it), or a refusal would repeat
    // every delay for ever.
    if (ok && this.dirty) this.scheduleAutosave();
    return ok;
  }

  private async runSave(): Promise<boolean> {
    if (this.autosaveTimer !== null) clearTimeout(this.autosaveTimer);
    this.autosaveTimer = null;
    const sent = this.current.doc;
    const codec = this.codec;
    const bytes = encodeText(sent.toString(), codec);
    this.saveError = null;
    // At most: the first try, one retry across a dead link or a lost reply,
    // and one refresh of a metadata-only precondition.
    for (let attempt = 0; attempt < 3; attempt++) {
      const pre = this.recreate
        ? {}
        : this.baseHash !== null
          ? { expectHash: this.baseHash }
          : { expectMtime: this.savedMtime };
      this.saveState = "saving";
      try {
        const r = await fsWrite(this.path, bytes, {
          ...pre,
          signal: AbortSignal.timeout(SAVE_TIMEOUT_MS),
        });
        if (this.disposed) return true;
        this.onSaved(sent, r);
        return true;
      } catch (e) {
        if (this.disposed) return false;
        if (e instanceof FileConflictError) {
          const disk = await this.readDisk();
          if (this.disposed) return false;
          if (disk === "missing") {
            this.saveState = "idle";
            this.conflict = { kind: "deleted" };
            return false;
          }
          if (disk === null) break;
          if (disk.text === sent.toString() && disk.note === null && sameCodec(disk.codec, codec)) {
            // Our own write, whose reply was lost (a daemon without idempotent
            // PUTs answers the retry with 409): it IS saved.
            this.onSaved(sent, { hash: disk.hash, mtime: disk.mtime });
            return true;
          }
          if (this.isBase(disk) && attempt < 2) {
            // Only the metadata token moved (a touch): retry with the new one.
            this.savedMtime = disk.mtime;
            continue;
          }
          this.saveState = "idle";
          this.reconcile(disk);
          return false;
        }
        if (!isTransport(e) || attempt > 0) {
          this.saveState = "failed";
          this.saveError = errorMessage(e, "save failed");
          return false;
        }
        this.saveState = get(daemonLinkUp) ? "retrying" : "offline";
        // The unsaved text must survive whatever the link does next.
        void this.journal();
        await waitForLink(RETRY_MIN_DELAY_MS);
        if (this.disposed) return false;
      }
    }
    this.saveState = "failed";
    this.saveError = "not saved — the daemon could not be reached";
    return false;
  }

  /** `sent` is on disk: it becomes the base. The buffer is clean only if
   *  nothing was typed since it was sent (save generations). */
  private onSaved(sent: Text, r: WriteResult): void {
    this.baseGen++;
    this.baseDoc = sent;
    this.baseText = sent.toString();
    this.baseHash = r.hash;
    this.savedMtime = r.mtime;
    this.recreate = false;
    if (this.conflict?.kind === "deleted") this.conflict = null;
    noteWrite(this.path, r.mtime);
    this.saveState = "idle";
    this.saveError = null;
    this.refreshDirty();
    if (!this.dirty) {
      this.savedCount++;
      this.afterClean();
    }
  }

  /** The buffer matches the disk: its journal has nothing left to protect
   *  (this window's draft, or anyone's holding exactly the disk text). */
  private afterClean(): void {
    this.clearDraft(this.path, { writer: drafts.WRITER, text: this.baseText });
    this.maybeDispose();
  }

  /** Drop `path`'s journaled draft — by default only the one this window
   *  wrote: another window of this origin may hold the same file dirty under
   *  the same key. A journal write still in flight then never marks its text
   *  journaled (drafts.ts orders the mirror ops), and edits still unsaved
   *  here are journaled afresh after the clear. */
  private clearDraft(path: string, match: drafts.DraftMatch = { writer: drafts.WRITER }): void {
    this.journalEpoch++;
    this.journaledText = null;
    void drafts.clear(path, match);
    if (this.dirty && path === this.path) this.scheduleJournal();
  }

  // --- autosave -------------------------------------------------------------

  private autosaveOn(): boolean {
    return getSetting("editor.autosave") === "afterDelay";
  }

  private scheduleAutosave(): void {
    if (this.autosaveTimer !== null) clearTimeout(this.autosaveTimer);
    this.autosaveTimer = null;
    if (!this.autosaveOn() || !this.dirty || this.conflict !== null || this.disposed) return;
    this.autosaveTimer = setTimeout(() => {
      this.autosaveTimer = null;
      void this.save(true);
    }, getSetting("editor.autosaveDelay"));
  }

  /** Blur / tab switch: with autosave on, save now instead of after the delay. */
  flushAutosave(): void {
    if (!this.autosaveOn() || !this.dirty || this.conflict !== null) return;
    void this.save(true);
  }

  // --- the journal ------------------------------------------------------------

  private scheduleJournal(): void {
    if (this.journalTimer !== null) clearTimeout(this.journalTimer);
    this.journalTimer = setTimeout(() => {
      this.journalTimer = null;
      void this.journal();
    }, JOURNAL_DELAY_MS);
  }

  /** Journal the dirty text now (hide/pagehide pass `keepalive`). */
  async journal(keepalive = false): Promise<void> {
    if (this.journalTimer !== null) clearTimeout(this.journalTimer);
    this.journalTimer = null;
    if (!this.dirty || this.disposed) return;
    const text = this.current.doc.toString();
    // The hide/pagehide flush writes even unchanged text: another window of
    // this origin may have overwritten the path's one record since, and this
    // page may not come back.
    if (text === this.journaledText && !keepalive) return;
    const epoch = ++this.journalEpoch;
    const r = await drafts.journal(
      {
        path: this.path,
        baseHash: this.baseHash ?? "",
        baseText: this.baseText,
        text,
        updatedMs: Date.now(),
      },
      keepalive,
    );
    // Only the newest write's outcome counts: a newer journal or a clear (a
    // save landed) was issued meanwhile. A clean buffer has nothing at risk.
    if (this.disposed || epoch !== this.journalEpoch) return;
    const ok = drafts.journaled(r);
    if (ok) this.journaledText = text;
    this.journalFailed = this.dirty && !ok;
  }

  /** On open: a journaled draft that differs from the disk is offered, never applied. */
  private async lookForDraft(): Promise<void> {
    const rec = await drafts.find(this.path);
    if (rec === null || this.disposed) return;
    if (rec.text === this.baseText) {
      // Nothing to recover: drop that record, whoever wrote it.
      this.clearDraft(this.path, { writer: rec.writer, text: rec.text });
      return;
    }
    // Live in another window of this origin, not lost: that window owns it.
    if (presence.heldElsewhere(this.path)) return;
    if (rec.text === this.current.doc.toString()) return;
    this.recovered = rec;
  }

  restoreDraft(): void {
    const rec = this.recovered;
    if (rec === null || !this.editable) return;
    this.recovered = null;
    const sameBase =
      (rec.baseHash !== "" && this.baseHash !== null && rec.baseHash === this.baseHash) ||
      (rec.baseText !== null && rec.baseText === this.baseText);
    if (sameBase) {
      this.setText(rec.text, "restore", true);
      this.refreshDirty();
      return;
    }
    // The file moved on since the draft was typed.
    if (rec.baseText !== null) {
      const r = merge3(rec.baseText, rec.text, this.baseText);
      if (r.clean) {
        const before = this.current.doc.toString();
        this.setText(r.text, "restore", true);
        this.refreshDirty();
        this.showNotice({ before, after: r.text, depth: undoDepth(this.current) });
        return;
      }
    }
    // Nothing proves the two compatible: restore the draft and hold the disk
    // version as a conflict (compare / keep mine / take disk).
    const disk: DiskVersion = {
      text: this.baseText,
      codec: this.codec,
      hash: this.baseHash,
      mtime: this.savedMtime,
      note: null,
      chunk: { bytes: new Uint8Array(0), size: 0, truncated: false, mtime: this.savedMtime, hash: this.baseHash },
    };
    this.setText(rec.text, "restore", true);
    this.refreshDirty();
    if (this.dirty) this.conflict = { kind: "changed", disk };
  }

  /** Drop the offered draft — the record it came from, whoever wrote it. */
  discardDraft(): void {
    const rec = this.recovered;
    if (rec === null) return;
    this.recovered = null;
    this.clearDraft(this.path, { writer: rec.writer, text: rec.text });
  }

  // --- lifecycle ------------------------------------------------------------

  /** A rename/move of this file or a parent: follow it. */
  rekey(to: string): void {
    const from = this.path;
    if (from === to) return;
    buffers.delete(from);
    buffers.set(to, this);
    this.stopWatch();
    release(from);
    this.path = to;
    this.entry = retain(to);
    this.watch();
    if (this.dirty) {
      setDirty(from, false);
      presence.announce(from, false);
      setDirty(to, true);
      presence.announce(to, true);
      this.clearDraft(from);
      this.scheduleJournal();
    }
  }

  /** Forget a buffer nobody shows that has nothing unsaved (a "changed"
   *  conflict always holds edits, so `dirty` covers it). */
  maybeDispose(): void {
    if (this.disposed || this.refs > 0 || this.dirty) return;
    if (this.saveTask !== null || this.checkTask !== null) return;
    this.dispose();
  }

  private dispose(): void {
    this.disposed = true;
    if (buffers.get(this.path) === this) buffers.delete(this.path);
    for (const t of [this.journalTimer, this.autosaveTimer, this.noticeTimer]) {
      if (t !== null) clearTimeout(t);
    }
    this.stopWatch();
    release(this.path);
    if (this.dirty) {
      setDirty(this.path, false);
      presence.announce(this.path, false);
    }
  }
}

// --- the store ---------------------------------------------------------------

const buffers = new Map<string, Buffer>();

/**
 * Claim the buffer for `path` for a mounting view: the existing one (its
 * edits, history and cursor intact), or a new one seeded from `first` (the
 * host's already-fetched first chunk). Pair with `releaseBuffer`.
 */
export function openBuffer(path: string, first: FileChunk): Buffer {
  let b = buffers.get(path);
  if (b === undefined || b.disposed) {
    b = new Buffer(path, first);
    buffers.set(path, b);
  }
  b.refs += 1;
  return b;
}

/** A view let go; a clean buffer is forgotten with its last view. */
export function releaseBuffer(b: Buffer): void {
  b.refs = Math.max(0, b.refs - 1);
  b.maybeDispose();
}

/** The live buffer for `path`, if any (App's close flow, tests). */
export function bufferFor(path: string): Buffer | undefined {
  return buffers.get(path);
}

function under(path: string, root: string): string | null {
  if (path === root) return "";
  return path.startsWith(`${root}/`) ? path.slice(root.length) : null;
}

let lastMutationSeq = 0;
lastFsMutation.subscribe((m) => {
  if (m === null || m.seq === lastMutationSeq) return;
  lastMutationSeq = m.seq;
  if (m.kind === "rename") {
    for (const b of [...buffers.values()]) {
      const rest = under(b.path, m.from);
      if (rest === null) continue;
      const to = m.to + rest;
      const existing = buffers.get(to);
      if (existing !== undefined && existing !== b) continue; // never merge two buffers
      b.rekey(to);
    }
  } else if (m.kind === "delete") {
    // A confirmed in-app delete: the user chose to lose this file.
    for (const b of [...buffers.values()]) {
      if (under(b.path, m.path) !== null) b.discard();
    }
  }
});

setEditingHost({
  save: (path) => buffers.get(path)?.save() ?? Promise.resolve(true),
  discard: (path) => buffers.get(path)?.discard(),
});

// Hidden or closing: journal every dirty buffer NOW rather than after the
// debounce (the tab may never come back). A store-module listener, like
// workspace/compute.ts — views may all be unmounted while buffers are dirty.
if (typeof document !== "undefined" && typeof window !== "undefined") {
  const flush = (): void => {
    for (const b of buffers.values()) void b.journal(true);
  };
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") flush();
    presence.visibilityChanged();
  });
  window.addEventListener("pagehide", () => {
    flush();
    presence.leave();
  });
}

// --- other windows of this origin ---------------------------------------------

/** A window not heard from for this long is gone — it crashed or was killed
 *  without its "bye" — and its paths stop counting as held elsewhere. */
export const PRESENCE_TTL_MS = 30_000;
/** A window holding unsaved edits re-announces them this often. */
export const PRESENCE_HEARTBEAT_MS = 10_000;
/** Hidden, the browser throttles timers (Chrome: to once a minute), so a
 *  hidden window beats at that pace and what it announced lives longer. */
export const PRESENCE_HIDDEN_HEARTBEAT_MS = 60_000;
export const PRESENCE_HIDDEN_TTL_MS = 3 * 60_000;

export type PresenceMsg =
  | { t: "dirty"; win: string; path: string; dirty: boolean }
  | { t: "hello"; win: string }
  | { t: "state"; win: string; paths: string[]; hidden?: boolean }
  | { t: "bye"; win: string };

/** The slice of BroadcastChannel presence uses (tests pass a fake). */
export interface PresenceBus {
  postMessage(msg: PresenceMsg): void;
  onmessage: ((e: MessageEvent<PresenceMsg>) => void) | null;
}

interface Peer {
  paths: Set<string>;
  /** When this window last heard from it (its own clock). */
  seen: number;
  hidden: boolean;
}

/**
 * Same-origin windows announce which paths they hold unsaved, so a second
 * window on the same file shows "unsaved edits in another window" instead of
 * a silent fork — and does not offer that window's live draft as a recovery.
 * Cross-origin windows (another tunnel port) cannot see this; the
 * content-hash precondition and the merge cover them.
 *
 * A window that dies without its "bye" (a crash, a killed process) must not
 * hide its draft from recovery for ever, so a window re-announces its dirty
 * paths every PRESENCE_HEARTBEAT_MS while it holds any, and peers drop one
 * silent past PRESENCE_TTL_MS. The heartbeat is a deliberate exemption from
 * visibility gating — a hidden window still owns its edits — but it runs
 * only while this window holds unsaved edits, slows to the browser's own
 * hidden-timer pace, and says it is hidden so peers wait longer for it. The
 * peer-side expiry timer runs only while this window is visible and some
 * peer is known; a hidden window re-checks on return and on every read.
 */
export class Presence {
  /** Paths some OTHER live window holds dirty. */
  elsewhere = $state<ReadonlySet<string>>(new Set());
  private readonly win = Math.random().toString(36).slice(2) + Date.now().toString(36);
  private readonly peers = new Map<string, Peer>();
  private readonly channel: PresenceBus | null;
  private readonly mine: () => string[];
  private readonly hidden: () => boolean;
  private heartbeat: ReturnType<typeof setTimeout> | null = null;
  private expiry: ReturnType<typeof setTimeout> | null = null;

  constructor(channel: PresenceBus | null, mine: () => string[], hidden: () => boolean) {
    this.channel = channel;
    this.mine = mine;
    this.hidden = hidden;
    if (channel === null) return;
    channel.onmessage = (e: MessageEvent<PresenceMsg>) => this.receive(e.data);
    this.post({ t: "hello", win: this.win });
  }

  private post(msg: PresenceMsg): void {
    try {
      this.channel?.postMessage(msg);
    } catch {
      // a closed channel (page teardown) has nobody left to tell
    }
  }

  /** Tell peers every path this window holds unsaved (none: nothing to say). */
  private postState(): void {
    const paths = this.mine();
    if (paths.length > 0) this.post({ t: "state", win: this.win, paths, hidden: this.hidden() });
  }

  /** Keep re-announcing while this window holds unsaved edits. */
  private beat(): void {
    if (this.channel === null || this.heartbeat !== null || this.mine().length === 0) return;
    this.heartbeat = setTimeout(
      () => {
        this.heartbeat = null;
        this.postState();
        this.beat();
      },
      this.hidden() ? PRESENCE_HIDDEN_HEARTBEAT_MS : PRESENCE_HEARTBEAT_MS,
    );
  }

  private receive(msg: PresenceMsg): void {
    if (msg === null || typeof msg !== "object" || msg.win === this.win) return;
    if (msg.t === "hello") {
      this.postState();
      return;
    }
    if (msg.t === "bye") {
      this.peers.delete(msg.win);
    } else {
      const peer = this.peers.get(msg.win) ?? { paths: new Set<string>(), seen: 0, hidden: false };
      peer.seen = Date.now();
      if (msg.t === "state") {
        peer.paths = new Set(msg.paths);
        peer.hidden = msg.hidden === true;
      } else if (msg.dirty) peer.paths.add(msg.path);
      else peer.paths.delete(msg.path);
      if (peer.paths.size > 0) this.peers.set(msg.win, peer);
      else this.peers.delete(msg.win);
    }
    this.refresh();
  }

  /** Recompute `elsewhere`, dropping peers silent past their TTL; while any
   *  remain and this window is visible, wake at the next expiry. */
  private refresh(): void {
    const now = Date.now();
    const all = new Set<string>();
    let next = Infinity;
    for (const [win, peer] of this.peers) {
      const until = peer.seen + (peer.hidden ? PRESENCE_HIDDEN_TTL_MS : PRESENCE_TTL_MS);
      if (until <= now) {
        this.peers.delete(win);
        continue;
      }
      next = Math.min(next, until);
      for (const path of peer.paths) all.add(path);
    }
    if (all.size !== this.elsewhere.size || [...all].some((p) => !this.elsewhere.has(p))) {
      this.elsewhere = all;
    }
    if (this.expiry !== null) clearTimeout(this.expiry);
    this.expiry = null;
    if (next !== Infinity && !this.hidden()) {
      this.expiry = setTimeout(() => {
        this.expiry = null;
        this.refresh();
      }, next - now);
    }
  }

  /** Whether a live window of this origin holds `path` unsaved right now. */
  heldElsewhere(path: string): boolean {
    this.refresh();
    return this.elsewhere.has(path);
  }

  announce(path: string, dirty: boolean): void {
    this.post({ t: "dirty", win: this.win, path, dirty });
    this.beat();
  }

  /** Shown or hidden: tell peers (the TTL they give us changes), re-pace the
   *  heartbeat, and catch up on (or stop) peer expiry. */
  visibilityChanged(): void {
    this.postState();
    if (this.heartbeat !== null) clearTimeout(this.heartbeat);
    this.heartbeat = null;
    this.beat();
    this.refresh();
  }

  leave(): void {
    for (const t of [this.heartbeat, this.expiry]) if (t !== null) clearTimeout(t);
    this.heartbeat = this.expiry = null;
    this.post({ t: "bye", win: this.win });
  }
}

export const presence = new Presence(
  typeof window !== "undefined" && typeof BroadcastChannel !== "undefined"
    ? new BroadcastChannel("chimaera.buffers")
    : null,
  () => [...buffers.values()].filter((b) => b.dirty).map((b) => b.path),
  () => typeof document !== "undefined" && document.visibilityState === "hidden",
);
