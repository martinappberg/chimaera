/**
 * A preview's lifetime in one place: the file-store claim, the loads that
 * must end with the view, the disk-change reload loop, pane visibility, and
 * the object URLs the view owns.
 *
 * Pane keeps up to eight views per pane mounted (layout/Pane.svelte), so a
 * view spends most of its life parked: `opacity: 0` + `inert`, its `visible`
 * prop false. Two rules follow, and these helpers carry them:
 *   - a preview's loads end with the view: every fetch carries the signal of
 *     its load generation, aborted when a newer load starts or the view goes;
 *   - a disk change that lands while the view is parked is not rendered into
 *     the parked layer. With `whileHidden: "defer"` the new version is read
 *     and parsed off screen, and its result is applied in the reveal's own
 *     update (a pre effect on the `visible` flip), before that reveal paints.
 *
 * The cores are plain classes (`ViewSignal`, `FileClaim`, `DiskVersionLoop`,
 * `RevealQueue`) so node Vitest drives them without a DOM or a component;
 * the exported `use…`/`on…` functions wire them to runes and must be called
 * during component initialisation.
 */

import { untrack } from "svelte";
import { fromStore } from "svelte/store";
import { pageVisible } from "../shared/visibility";
import { retain, release, type FileEntry } from "./fileStore.svelte";
import { watchPaneVisible } from "./paneVisible";
import { OwnedUrls } from "./blobUrls";

// --- cores ------------------------------------------------------------------------

/** One AbortController per load generation. `renew` aborts the previous
 *  generation's loads; `dispose` aborts the current one and refuses more. */
export class ViewSignal {
  private ctl: AbortController | null = null;
  private gen = 0;
  private dead = false;

  /** The current generation (bumped by `renew` and `dispose`). */
  get generation(): number {
    return this.gen;
  }

  get disposed(): boolean {
    return this.dead;
  }

  /** Abort the current generation and start the next; its signal. */
  renew(): AbortSignal {
    this.ctl?.abort();
    this.gen += 1;
    const ctl = new AbortController();
    if (this.dead) ctl.abort();
    this.ctl = ctl;
    return ctl.signal;
  }

  /** The current generation's signal (starting one if none has). */
  get signal(): AbortSignal {
    return this.ctl?.signal ?? this.renew();
  }

  /** Whether `gen` is still the live generation of a live view. */
  isCurrent(gen: number): boolean {
    return !this.dead && gen === this.gen;
  }

  dispose(): void {
    this.dead = true;
    this.gen += 1;
    this.ctl?.abort();
  }
}

/** One claim on a store entry at a time: a new path releases the old first,
 *  so the refs balance however often the path moves. */
export class FileClaim<E> {
  private held: string | null = null;
  constructor(
    private readonly take: (path: string) => E,
    private readonly drop: (path: string) => void,
  ) {}

  claim(path: string): E {
    this.free();
    const e = this.take(path);
    this.held = path;
    return e;
  }

  free(): void {
    const p = this.held;
    if (p === null) return;
    this.held = null;
    this.drop(p);
  }
}

/** The stale-entry guard (FileView): on a path switch the claim effect runs
 *  after deriveds re-evaluate, so for a moment the entry still belongs to the
 *  previous path. Read through this, a mismatched entry is no entry. */
export function entryFor<E extends { path: string }>(entry: E | null, path: string): E | null {
  return entry !== null && entry.path === path ? entry : null;
}

/** Work that waits for the view to be shown: `run` now when it is, else the
 *  latest `run` waits (replacing an earlier one) for `setShown(true)`. */
export class RevealQueue {
  private pending: (() => void) | null = null;
  private shown = true;

  run(fn: () => void): void {
    if (this.shown) fn();
    else this.pending = fn;
  }

  setShown(shown: boolean): void {
    this.shown = shown;
    if (!shown || this.pending === null) return;
    const fn = this.pending;
    this.pending = null;
    fn();
  }

  /** Forget the waiting work (superseded, or the view went). */
  clear(): void {
    this.pending = null;
  }
}

type Outcome<T> = { ok: true; value: T } | { ok: false; error: unknown };

export interface DiskVersionOptions<T> {
  /** Read (and parse) the file; honour `signal`. */
  load: (signal: AbortSignal) => Promise<T>;
  /** Show a result. `still()` is false once a newer load started or the
   *  view went (for follow-up work the result starts, like PPTX notes). */
  apply: (result: T, still: () => boolean) => void;
  /** Show a failure (stale failures, aborts included, never arrive). */
  fail: (error: unknown) => void;
  /** Free a result that will never be shown (superseded, or the view went). */
  discard?: (result: T) => void;
  /** "run": apply when the load lands, parked or not. "defer": a change
   *  that lands while parked is held and applied when the view is shown.
   *  The first load always applies when it lands. */
  whileHidden: "defer" | "run";
}

/**
 * The mtime → reload loop every whole-file preview hand-rolled. Feed it the
 * entry's version token; it loads on the first token it sees (the open),
 * skips the token's first arrival after that open (learning the version is
 * not a change), reloads on every later change, drops results of superseded
 * generations, and with "defer" holds a parked view's result for the reveal.
 */
export class DiskVersionLoop<T> {
  private seen: string | null | undefined = undefined;
  private readonly sig = new ViewSignal();
  private visible = true;
  private held: { outcome: Outcome<T>; gen: number } | null = null;

  constructor(private readonly opts: DiskVersionOptions<T>) {}

  /** The entry's current version token (null: not known yet). */
  token(m: string | null): void {
    const seen = this.seen;
    if (seen !== undefined && (m === null || m === seen || seen === null)) {
      if (m !== null) this.seen = m;
      return;
    }
    this.seen = m;
    this.start(seen === undefined);
  }

  setVisible(visible: boolean): void {
    this.visible = visible;
    if (!visible || this.held === null) return;
    const held = this.held;
    this.held = null;
    this.deliver(held.outcome, held.gen);
  }

  dispose(): void {
    this.sig.dispose();
    this.drop();
  }

  private start(first: boolean): void {
    const signal = this.sig.renew();
    const gen = this.sig.generation;
    const hold = !first && this.opts.whileHidden === "defer";
    let pending: Promise<T>;
    try {
      pending = this.opts.load(signal);
    } catch (error) {
      pending = Promise.reject(error);
    }
    pending.then(
      (value) => this.land({ ok: true, value }, gen, hold),
      (error: unknown) => this.land({ ok: false, error }, gen, hold),
    );
  }

  private land(outcome: Outcome<T>, gen: number, hold: boolean): void {
    if (!this.sig.isCurrent(gen)) {
      if (outcome.ok) this.opts.discard?.(outcome.value);
      return;
    }
    if (hold && !this.visible) {
      this.drop();
      this.held = { outcome, gen };
      return;
    }
    this.deliver(outcome, gen);
  }

  private deliver(outcome: Outcome<T>, gen: number): void {
    if (outcome.ok) this.opts.apply(outcome.value, () => this.sig.isCurrent(gen));
    else this.opts.fail(outcome.error);
  }

  private drop(): void {
    const held = this.held;
    this.held = null;
    if (held?.outcome.ok === true) this.opts.discard?.(held.outcome.value);
  }
}

// --- rune wiring (call during component init) ---------------------------------------

/** A load signal bound to this view: `renew()` per load, aborted on unmount. */
export function viewSignal(): ViewSignal {
  const sig = new ViewSignal();
  $effect(() => () => sig.dispose());
  return sig;
}

/**
 * Claim the store entry for `getPath()` while the view lives (retain on
 * mount and on a path change, release the previous one first), running
 * `ensure` once per claim. `current` is null while the claim still holds
 * another path.
 */
export function useFileEntry(
  getPath: () => string,
  ensure?: (entry: FileEntry) => void,
): { readonly current: FileEntry | null } {
  const claim = new FileClaim<FileEntry>(retain, release);
  let entry = $state<FileEntry | null>(null);
  $effect(() => {
    const p = getPath();
    const e = untrack(() => claim.claim(p));
    entry = e;
    if (ensure !== undefined) untrack(() => ensure(e));
    return () => claim.free();
  });
  return {
    get current() {
      return entryFor(entry, getPath());
    },
  };
}

export interface PaneVisibility {
  /** This view's pane layer is the one on screen. */
  readonly pane: boolean;
  /** …and the window is visible: what recurring work gates on. */
  readonly shown: boolean;
  /** Resolves once the pane is on screen (at once if it is), or when the
   *  view goes — check your own disposal after awaiting. */
  whenVisible(): Promise<void>;
}

/**
 * The one source of "is this view on screen": the `visible` prop when the
 * host passes one (Pane does, through FileView), else the enclosing
 * `.layer`'s `inert` flag watched from `getNode()` (embeds, hover previews:
 * no layer means visible).
 */
export function paneVisibility(
  getVisible: () => boolean | undefined,
  getNode: () => Element | null = () => null,
): PaneVisibility {
  const page = fromStore(pageVisible);
  let layer = $state(true);
  $effect(() => {
    if (getVisible() !== undefined) return;
    const node = getNode();
    if (node === null) return;
    return watchPaneVisible(node, (v) => (layer = v));
  });
  const pane = (): boolean => getVisible() ?? layer;
  const waiters: (() => void)[] = [];
  const wake = (): void => {
    for (const w of waiters.splice(0)) w();
  };
  $effect(() => {
    if (pane()) wake();
  });
  $effect(() => () => wake());
  return {
    get pane() {
      return pane();
    },
    get shown() {
      return pane() && page.current;
    },
    whenVisible() {
      return untrack(pane) ? Promise.resolve() : new Promise<void>((done) => waiters.push(done));
    },
  };
}

/**
 * Reload a preview on every on-disk version of its entry (see
 * `DiskVersionLoop`). `visible` is read in a pre effect, so a deferred
 * result applies in the same update that reveals the view.
 */
export function onDiskVersion<T>(
  getEntry: () => FileEntry | null,
  visible: () => boolean,
  opts: DiskVersionOptions<T>,
): DiskVersionLoop<T> {
  const loop = new DiskVersionLoop(opts);
  $effect.pre(() => {
    const v = visible();
    untrack(() => loop.setVisible(v));
  });
  $effect(() => {
    const m = getEntry()?.mtime ?? null;
    untrack(() => loop.token(m));
  });
  $effect(() => () => loop.dispose());
  return loop;
}

/** A `RevealQueue` fed by `visible` in a pre effect (applied before the
 *  reveal paints) and cleared on unmount. */
export function whenRevealed(visible: () => boolean): RevealQueue {
  const queue = new RevealQueue();
  $effect.pre(() => {
    const v = visible();
    untrack(() => queue.setShown(v));
  });
  $effect(() => () => queue.clear());
  return queue;
}

/** The object URLs this view owns, all revoked when it unmounts. */
export function ownedObjectUrls(): OwnedUrls {
  const urls = new OwnedUrls();
  $effect(() => () => urls.revokeAll());
  return urls;
}
