/**
 * A deck's in-viewer edits as a stack of operations over its parts.
 *
 * An edit is an operation ("shape 4 on slide 3 goes to x,y,cx,cy", "its text
 * becomes …", "the slides go in this order"), never new bytes: the patched
 * part is always origin text + the operations replayed through `pptxXml.ts`.
 * That one choice makes the rest simple —
 *
 * - **Undo / redo** pop and push operations and replay the touched part.
 * - **Dirty** is exact: a part is dirty when its replayed text differs from
 *   what is on disk, so undoing back to the saved state is clean again.
 * - **Our own save** changes only the touched parts, so the origin texts we
 *   cached stay true and the history survives the save.
 * - **An outside change** (an agent's python-pptx rewrite, PowerPoint) is a
 *   rebase: replay each operation over the new file. One whose target no
 *   longer reads the way it did when it was made (`from`), or whose shape is
 *   gone, is held as a conflict for the person to decide — never applied
 *   blindly, never dropped silently.
 *
 * Plain class with no Svelte: `pptxEditStore.svelte.ts` wraps it for views.
 */

import {
  PptxXmlError,
  deleteShape,
  readText,
  readXfrm,
  reorderSlides,
  setText,
  setXfrm,
  slideOrder,
  type Xfrm,
} from "./pptxXml";

export type DeckOp =
  | { kind: "xfrm"; part: string; shape: string; from: Xfrm | null; to: Xfrm }
  | { kind: "text"; part: string; shape: string; from: string[]; to: string[] }
  | { kind: "delete"; part: string; shape: string }
  | { kind: "order"; part: string; from: string[]; to: string[] };

/** Reads one part of a package as text (byte-faithful; see `pptxPackage.ts`). */
export type PartReader = (name: string) => Promise<string>;

function sameXfrm(a: Xfrm | null, b: Xfrm | null): boolean {
  return a === b || (a !== null && b !== null && a.x === b.x && a.y === b.y && a.cx === b.cx && a.cy === b.cy);
}

function sameList(a: readonly string[] | null, b: readonly string[] | null): boolean {
  return a !== null && b !== null && a.length === b.length && a.every((v, k) => v === b[k]);
}

/** `op` applied to `xml`. Throws `PptxXmlError` when it cannot apply. */
export function applyOp(xml: string, op: DeckOp): string {
  switch (op.kind) {
    case "xfrm":
      return setXfrm(xml, op.shape, op.to);
    case "text":
      return setText(xml, op.shape, op.to);
    case "delete":
      return deleteShape(xml, op.shape);
    case "order":
      return reorderSlides(xml, op.to);
  }
}

/** Does `xml` still read the way `op` saw it when it was made? */
export function opFits(xml: string, op: DeckOp): boolean {
  try {
    switch (op.kind) {
      case "xfrm":
        return shapeExists(xml, op.shape) && sameXfrm(readXfrm(xml, op.shape), op.from);
      case "text":
        return sameList(readText(xml, op.shape), op.from);
      case "delete":
        return true;
      case "order":
        return sameList(slideOrder(xml), op.from);
    }
  } catch {
    return false;
  }
}

function shapeExists(xml: string, id: string): boolean {
  // A shape exists when any patcher can find it; deleteShape is the cheapest probe.
  try {
    deleteShape(xml, id);
    return true;
  } catch {
    return false;
  }
}

/** A short description of an operation, for the conflict banner. */
export function describeOp(op: DeckOp): string {
  switch (op.kind) {
    case "xfrm":
      return "a move or resize";
    case "text":
      return "a text edit";
    case "delete":
      return "a deleted shape";
    case "order":
      return "the slide order";
  }
}

export interface RebaseResult {
  /** Operations that no longer fit the new file, held for a decision. */
  conflicts: DeckOp[];
}

export class DeckEditsCore {
  /** Part texts as the edit history started from (cached on first use). */
  private origin = new Map<string, string>();
  /** Part texts this window saved since (what disk holds for those parts). */
  private saved = new Map<string, string>();
  /** origin + ops, per touched part. */
  private current = new Map<string, string>();
  private ops: DeckOp[] = [];
  private redoStack: DeckOp[] = [];
  /** Held by a rebase until `resolve`. */
  private held: DeckOp[] = [];
  /** Gesture id of the last operation, for coalescing a drag or typing. */
  private lastGesture: string | null = null;

  constructor(
    private read: PartReader,
    /** SHA-256 (hex) of the file on disk as this window last knew it. */
    public diskHash: string,
  ) {}

  /** Load `part` so operations on it can be pushed synchronously. */
  async ensure(part: string): Promise<string> {
    if (!this.origin.has(part)) {
      const text = await this.read(part);
      // Another ensure may have landed while this one read: the first wins.
      if (!this.origin.has(part)) this.origin.set(part, text);
    }
    return this.text(part);
  }

  has(part: string): boolean {
    return this.origin.has(part);
  }

  /** The part's text with every edit applied. */
  text(part: string): string {
    const t = this.current.get(part) ?? this.saved.get(part) ?? this.origin.get(part);
    if (t === undefined) throw new Error(`part ${part} not loaded`);
    return t;
  }

  /** What the file on disk holds for `part` (as far as this window knows). */
  disk(part: string): string | undefined {
    return this.saved.get(part) ?? this.origin.get(part);
  }

  /** Parts whose edited text differs from disk. */
  dirtyParts(): string[] {
    return [...this.current.keys()].filter((p) => this.current.get(p) !== this.disk(p));
  }

  get dirty(): boolean {
    return this.dirtyParts().length > 0;
  }

  get canUndo(): boolean {
    return this.ops.length > 0;
  }

  get canRedo(): boolean {
    return this.redoStack.length > 0;
  }

  get conflicts(): readonly DeckOp[] {
    return this.held;
  }

  /** Every operation still in the history (journal and tests). */
  get history(): readonly DeckOp[] {
    return this.ops;
  }

  /**
   * Apply `op`. With a `gesture` id equal to the last push's, it folds into
   * that operation (one drag, one typing burst = one undo step), keeping the
   * first one's `from`. Throws `PptxXmlError` when the edit can't apply; the
   * history is unchanged then.
   */
  push(op: DeckOp, gesture: string | null = null): void {
    const before = this.text(op.part);
    const after = applyOp(before, op);
    const last = this.ops[this.ops.length - 1];
    if (
      gesture !== null &&
      gesture === this.lastGesture &&
      last !== undefined &&
      last.kind === op.kind &&
      last.part === op.part &&
      ("shape" in last ? last.shape === (op as { shape: string }).shape : true)
    ) {
      this.ops[this.ops.length - 1] = { ...op, from: (last as { from: unknown }).from } as DeckOp;
    } else this.ops.push(op);
    this.lastGesture = gesture;
    this.redoStack = [];
    this.current.set(op.part, after);
  }

  /** Recompute `part` from its base and the history. */
  private replay(part: string): void {
    let t = this.origin.get(part);
    if (t === undefined) return;
    for (const op of this.ops) if (op.part === part) t = applyOp(t, op);
    this.current.set(part, t);
  }

  undo(): DeckOp | null {
    const op = this.ops.pop();
    if (op === undefined) return null;
    this.redoStack.push(op);
    this.lastGesture = null;
    this.replay(op.part);
    return op;
  }

  redo(): DeckOp | null {
    const op = this.redoStack.pop();
    if (op === undefined) return null;
    this.ops.push(op);
    this.lastGesture = null;
    this.current.set(op.part, applyOp(this.text(op.part), op));
    return op;
  }

  /** What a save sends: each dirty part's full edited text. */
  pendingSave(): { name: string; text: string }[] {
    return this.dirtyParts().map((name) => ({ name, text: this.text(name) }));
  }

  /** A save of `parts` landed; the file on disk now hashes to `hash`. */
  markSaved(parts: readonly { name: string; text: string }[], hash: string): void {
    for (const p of parts) this.saved.set(p.name, p.text);
    this.diskHash = hash;
    this.lastGesture = null;
  }

  /**
   * The file on disk is now `hash`, readable through `read`. Our own save
   * (same hash) changes nothing. An outside change becomes the new origin:
   * every operation that still fits is replayed over it; the rest are held
   * (`conflicts`) for `resolve`.
   */
  async rebase(read: PartReader, hash: string): Promise<RebaseResult> {
    this.read = read;
    if (hash === this.diskHash) return { conflicts: [...this.held] };
    const parts = [...new Set([...this.origin.keys(), ...this.held.map((op) => op.part)])];
    const next = new Map<string, string>();
    for (const p of parts) {
      try {
        next.set(p, await read(p));
      } catch {
        // The part is gone (a slide deleted): its operations can't fit.
      }
    }
    this.origin = next;
    this.saved.clear();
    this.current.clear();
    this.diskHash = hash;
    this.lastGesture = null;
    this.redoStack = [];
    const all = [...this.ops, ...this.held];
    this.ops = [];
    this.held = [];
    for (const op of all) this.replant(op);
    return { conflicts: [...this.held] };
  }

  /** Put a journaled history back over the loaded file (same rules as a
   *  rebase: what doesn't fit is held). */
  async restore(ops: readonly DeckOp[]): Promise<void> {
    for (const op of ops) {
      try {
        await this.ensure(op.part);
      } catch {
        this.held.push(op);
        continue;
      }
      this.replant(op);
    }
  }

  /** Replay `op` over the current text if it fits; an op the file already
   *  reflects (the same change made elsewhere, or a save whose journal
   *  outlived it) is dropped as done; anything else is held. */
  private replant(op: DeckOp): void {
    const base = this.origin.has(op.part) ? this.text(op.part) : null;
    if (base !== null) {
      try {
        const after = applyOp(base, op);
        if (after === base) return;
        if (opFits(base, op)) {
          this.current.set(op.part, after);
          this.ops.push(op);
          return;
        }
      } catch (e) {
        if (!(e instanceof PptxXmlError)) throw e;
      }
    }
    this.held.push(op);
  }

  /**
   * Decide the held operations: "theirs" drops them (the file's version
   * stands), "mine" applies each over the new file where its shape still
   * exists. Returns how many of mine could not apply (their shape is gone).
   */
  resolve(choice: "theirs" | "mine"): number {
    const held = this.held;
    this.held = [];
    if (choice === "theirs") return 0;
    let lost = 0;
    for (const op of held) {
      if (!this.origin.has(op.part)) {
        lost++;
        continue;
      }
      try {
        this.current.set(op.part, applyOp(this.text(op.part), op));
        this.ops.push(op);
      } catch (e) {
        if (!(e instanceof PptxXmlError)) throw e;
        lost++;
      }
    }
    return lost;
  }

  /** Throw every edit away. */
  discard(): void {
    this.ops = [];
    this.redoStack = [];
    this.held = [];
    this.current.clear();
    this.lastGesture = null;
  }
}
