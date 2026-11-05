/**
 * Deck edits that outlive their view ("never lose an edit"): one `DeckEdits`
 * per .pptx path, holding a `DeckEditsCore` (the operation history) plus
 * what views render — dirty, saving, the last error, conflicts.
 *
 * - A view `attach`es each version of the file it loads (bytes + hash): the
 *   first creates the history, a later one rebases it (our own save is
 *   recognized by its hash and changes nothing).
 * - The store mirrors dirtiness into `shared/editing`'s `dirtyFiles` (tab
 *   dot, beforeunload, the native unsaved-quit guard) and answers that
 *   module's save/discard for deck paths, so closing a window with an edited
 *   deck offers Save like any edited file.
 * - Edits are journaled a second after they settle through `drafts.ts`
 *   under `<path>#pptx-edits`, as JSON operations with the hash of the file
 *   they apply to; a later attach replays a journaled history it finds.
 * - A save sends only the dirty parts (`fsPatchPackage`) with the hash
 *   precondition; a 409 never overwrites — it waits for the view's reload,
 *   whose rebase decides.
 *
 * `DeckEditsCore` is a plain field, never $state (the history holds whole
 * part texts); `version` is bumped on every change so views re-read.
 */

import { addEditingHost, setDirty } from "../shared/editing";
import { FileConflictError, PackageUnsupportedError, fsPatchPackage } from "./files";
import { noteWrite } from "./fileStore.svelte";
import { DeckEditsCore, describeOp, type DeckOp } from "./pptxEdits";
import { PptxXmlError } from "./pptxXml";
import { encodePart, packageReader, toBase64 } from "./pptxPackage";
import * as drafts from "./drafts";
import { ApiError } from "../net/api";

/** Journal this long after the last edit. */
const JOURNAL_DELAY_MS = 1_000;
/** A save that hangs (a dead tunnel) gives up after this. */
const SAVE_TIMEOUT_MS = 30_000;
/** The quiet "saved" / "merged" note fades after this. */
const NOTE_MS = 4_000;

const JOURNAL_SUFFIX = "#pptx-edits";

interface JournalBody {
  v: 1;
  ops: DeckOp[];
}

export class DeckEdits {
  readonly path: string;
  /** Bumped on every change to the history; views re-read part texts. */
  version = $state(0);
  dirty = $state(false);
  canUndo = $state(false);
  canRedo = $state(false);
  saving = $state(false);
  /** The last save or edit failure, shown in the bar. */
  error = $state<string | null>(null);
  /** Edits a change on disk left undecided (descriptions). */
  conflicts = $state<string[]>([]);
  /** A quiet note ("saved", "merged with a change on disk"). */
  note = $state<string | null>(null);
  /** The journal could not record the edits (shown: never silently "safe"). */
  unjournaled = $state(false);
  /** Why this deck can't be edited, when it can't. */
  blocked = $state<string | null>(null);
  /** The history exists (the first version was attached). `core` itself is
   *  a plain field, so this is what views react to. */
  ready = $state(false);

  private core: DeckEditsCore | null = null;
  private views = 0;
  private journalTimer: ReturnType<typeof setTimeout> | null = null;
  private noteTimer: ReturnType<typeof setTimeout> | null = null;
  private attachChain: Promise<void> = Promise.resolve();
  /** The journal record a restore came from (another page wrote it), so
   *  a clean save clears it too. */
  private restoredText: string | null = null;

  constructor(path: string) {
    this.path = path;
  }


  /** Text of `part` with every edit applied, when it has edits or is loaded. */
  partText(part: string): string | null {
    const c = this.core;
    return c !== null && c.has(part) ? c.text(part) : null;
  }

  /** What the file on disk holds for `part` (what a fresh load draws). */
  diskText(part: string): string | null {
    return this.core?.disk(part) ?? null;
  }

  /** Parts the history touched (the view re-renders their slides). */
  touchedParts(): string[] {
    const c = this.core;
    if (c === null) return [];
    return [...new Set(c.history.map((op) => op.part))];
  }

  /** Load `part` so edits to it apply at once (call before a gesture). */
  async ensure(part: string): Promise<string | null> {
    const c = this.core;
    if (c === null) return null;
    try {
      return await c.ensure(part);
    } catch (e) {
      this.error = e instanceof Error ? e.message : "this part can't be edited";
      return null;
    }
  }

  /**
   * The view loaded a version of the file: `bytes` hashing to `hash` (null
   * when this page can't hash, which leaves the deck read-only). Serialized,
   * so two quick disk versions rebase in order.
   */
  attach(bytes: ArrayBuffer, hash: string | null): Promise<void> {
    const run = this.attachChain.then(() => this.attachNow(bytes, hash));
    this.attachChain = run.catch(() => {});
    return run;
  }

  private async attachNow(bytes: ArrayBuffer, hash: string | null): Promise<void> {
    if (hash === null) {
      this.blocked = "editing needs a secure page (this window can't hash the deck)";
      return;
    }
    let read;
    try {
      read = await packageReader(bytes);
    } catch (e) {
      this.blocked = e instanceof Error ? e.message : "this deck can't be edited";
      return;
    }
    this.blocked = null;
    if (this.core === null) {
      this.core = new DeckEditsCore(read, hash);
      await this.restoreJournal();
      this.ready = true;
      this.sync();
      return;
    }
    const wasDirty = this.core.dirty;
    const { conflicts } = await this.core.rebase(read, hash);
    this.sync();
    if (wasDirty && conflicts.length === 0 && this.core.dirty) this.flashNote("merged with a change on disk");
  }

  private async restoreJournal(): Promise<void> {
    const c = this.core;
    if (c === null) return;
    const rec = await drafts.find(this.path + JOURNAL_SUFFIX).catch(() => null);
    if (rec === null) return;
    let body: JournalBody;
    try {
      body = JSON.parse(rec.text) as JournalBody;
    } catch {
      return;
    }
    if (body.v !== 1 || !Array.isArray(body.ops) || body.ops.length === 0) return;
    this.restoredText = rec.text;
    await c.restore(body.ops);
    this.flashNote("restored unsaved edits");
  }

  /** Apply an edit (`gesture` folds a drag or a typing burst into one undo
   *  step). False when it doesn't apply; the error says why. */
  edit(op: DeckOp, gesture: string | null = null): boolean {
    const c = this.core;
    if (c === null || this.blocked !== null) return false;
    if (!c.has(op.part)) {
      this.error = "this slide is still loading";
      return false;
    }
    try {
      c.push(op, gesture);
    } catch (e) {
      if (!(e instanceof PptxXmlError)) throw e;
      this.error = `that edit can't be written to this slide (${e.message})`;
      return false;
    }
    this.error = null;
    this.sync();
    return true;
  }

  undo(): DeckOp | null {
    const op = this.core?.undo() ?? null;
    if (op !== null) this.sync();
    return op;
  }

  redo(): DeckOp | null {
    const op = this.core?.redo() ?? null;
    if (op !== null) this.sync();
    return op;
  }

  /** Keep or drop the edits a change on disk left undecided. */
  resolve(choice: "theirs" | "mine"): void {
    const c = this.core;
    if (c === null) return;
    const lost = c.resolve(choice);
    this.sync();
    if (lost > 0) this.error = `${lost} of your edits had nothing left to apply to`;
  }

  /** Save the dirty parts. True once the file on disk holds every edit. */
  async save(): Promise<boolean> {
    const c = this.core;
    if (c === null) return !this.dirty;
    if (!c.dirty) return true;
    if (this.saving) return false;
    if (c.conflicts.length > 0) {
      this.error = "decide what to do with the edits a change on disk left first";
      return false;
    }
    const parts = c.pendingSave();
    const hash = c.diskHash;
    this.saving = true;
    this.error = null;
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), SAVE_TIMEOUT_MS);
    try {
      const res = await fsPatchPackage(
        this.path,
        parts.map((p) => ({ name: p.name, data: toBase64(encodePart(p.text)) })),
        hash,
        ctl.signal,
      );
      if (res.hash === null) throw new Error("the daemon didn't report what it wrote");
      c.markSaved(parts, res.hash);
      noteWrite(this.path, res.mtime);
      this.sync();
      this.flashNote("saved");
      return !c.dirty;
    } catch (e) {
      if (e instanceof FileConflictError) {
        this.error = "the deck changed on disk — your edits are kept; reloading to merge them";
      } else if (e instanceof PackageUnsupportedError) {
        this.error = "this daemon is too old to save deck edits; update it to save";
      } else if (e instanceof ApiError) {
        this.error = e.message;
      } else if (ctl.signal.aborted) {
        this.error = "the save timed out — your edits are kept";
      } else {
        this.error = e instanceof Error ? `save failed: ${e.message}` : "save failed";
      }
      return false;
    } finally {
      clearTimeout(timer);
      this.saving = false;
    }
  }

  /** Throw every edit (and the journal) away. */
  discard(): void {
    this.core?.discard();
    this.sync();
    const restored = this.restoredText;
    this.restoredText = null;
    void drafts
      .clear(this.path + JOURNAL_SUFFIX, { writer: drafts.WRITER, ...(restored !== null ? { text: restored } : {}) })
      .catch(() => {});
  }

  retainView(): void {
    this.views++;
  }

  releaseView(): void {
    this.views = Math.max(0, this.views - 1);
    this.maybeForget();
  }

  private maybeForget(): void {
    if (this.views === 0 && !this.dirty && this.conflicts.length === 0) {
      registry.delete(this.path);
      if (this.journalTimer !== null) clearTimeout(this.journalTimer);
      if (this.noteTimer !== null) clearTimeout(this.noteTimer);
    }
  }

  private sync(): void {
    const c = this.core;
    this.dirty = c?.dirty ?? false;
    this.canUndo = c?.canUndo ?? false;
    this.canRedo = c?.canRedo ?? false;
    this.conflicts = (c?.conflicts ?? []).map(describeOp);
    this.version += 1;
    setDirty(this.path, this.dirty || this.conflicts.length > 0);
    this.scheduleJournal();
    this.maybeForget();
  }

  private scheduleJournal(): void {
    if (this.journalTimer !== null) clearTimeout(this.journalTimer);
    this.journalTimer = setTimeout(() => void this.journalNow(), JOURNAL_DELAY_MS);
  }

  private async journalNow(): Promise<void> {
    this.journalTimer = null;
    const c = this.core;
    if (c === null) return;
    const key = this.path + JOURNAL_SUFFIX;
    if (!c.dirty && c.conflicts.length === 0) {
      this.unjournaled = false;
      const restored = this.restoredText;
      this.restoredText = null;
      await drafts.clear(key, { writer: drafts.WRITER, ...(restored !== null ? { text: restored } : {}) }).catch(() => {});
      return;
    }
    const body: JournalBody = { v: 1, ops: [...c.history, ...c.conflicts] };
    const r = await drafts
      .journal({
        path: key,
        writer: drafts.WRITER,
        baseHash: c.diskHash,
        baseText: null,
        text: JSON.stringify(body),
        updatedMs: Date.now(),
      })
      .catch(() => null);
    this.unjournaled = r === null || !drafts.journaled(r);
  }

  private flashNote(text: string): void {
    this.note = text;
    if (this.noteTimer !== null) clearTimeout(this.noteTimer);
    this.noteTimer = setTimeout(() => {
      this.noteTimer = null;
      if (this.note === text) this.note = null;
    }, NOTE_MS);
  }
}

const registry = new Map<string, DeckEdits>();

/** The edits for `path` (created on first use). */
export function deckEdits(path: string): DeckEdits {
  let d = registry.get(path);
  if (d === undefined) {
    d = new DeckEdits(path);
    registry.set(path, d);
  }
  return d;
}

addEditingHost((path) => registry.get(path)?.dirty === true || (registry.get(path)?.conflicts.length ?? 0) > 0, {
  save: (path) => registry.get(path)?.save() ?? Promise.resolve(true),
  discard: (path) => registry.get(path)?.discard(),
});
