/**
 * Path links in chat text (agent prose, code spans, markdown link targets,
 * the user's own messages, tool cards). `shared/fileRef.ts` proposes the
 * candidates; the daemon (`fsValidate`) decides what actually becomes
 * clickable. This module owns the chat half between them: which candidates
 * a rendered code span or link offers, and the per-chat resolver that
 * batches, caches and expires the daemon's answers.
 */

import { contextMenu, type ContextMenuEntry } from "../shared/contextMenu.svelte";
import {
  extractFileRefs,
  parseFileRef,
  resolveBases,
  type FileRef,
  type FoundRef,
  type LinkContext,
} from "../shared/fileRef";
import type { OpenPathOptions, PathKind } from "../shared/openPath";
import { workspaceRelative } from "../shared/reference";
import type { Reveal } from "../shared/reveal";

/** A resolved /fs/validate hit. */
export interface PathHit {
  path: string;
  kind: PathKind;
}

/** The daemon's verdict on one candidate. */
export type Resolution =
  | { state: "hit"; hit: PathHit }
  | { state: "ambiguous"; matches: PathHit[] }
  | { state: "miss" };

/** One /fs/validate answer, the shape `fsValidate` returns. */
export interface ValidateAnswer {
  valid: Record<string, PathHit>;
  ambiguous?: Record<string, PathHit[]>;
  /** Never answered: unknown, not misses. */
  unchecked?: string[];
}

/** How the chat surfaces open a resolved path (ChatView routes it through
 *  `shared/openPath.ts`, so reveals and Cmd/Ctrl-click splits work). */
export type OpenPathFn = (path: string, kind: PathKind, opts?: OpenPathOptions) => void;

// --- the resolution context App registers ---------------------------------------

type ContextLookup = (sessionId: string) => LinkContext;
let contextLookup: ContextLookup | null = null;

/** App-level wiring: the same per-session context the terminal link
 *  provider uses (live cwd, spawn cwd, workspace root, workspace id). Null
 *  unregisters. */
export function setChatLinkContext(fn: ContextLookup | null): void {
  contextLookup = fn;
}

/** The registered context for a session, or null (tests, a detached view). */
export function chatLinkContext(sessionId: string): LinkContext | null {
  return contextLookup?.(sessionId) ?? null;
}

// --- candidates a rendered element offers ------------------------------------------

/**
 * An inline code span's references: the whole span when it is one
 * (`src/x.rs:12`, `Screenshot (1).png`) and, when it holds several words
 * (`cat results/x.csv`), each reference inside it — offered in case the
 * whole misses.
 */
export function codeSpanRefs(text: string): { whole: FileRef | null; parts: FoundRef[] } {
  const whole = parseFileRef(text, { delimited: true });
  const parts = /\s/.test(text.trim()) ? extractFileRefs(text) : [];
  return { whole, parts };
}

/** A markdown link target naming a local path (`src/a.rs#L10`,
 *  `demo%20assets/x.csv`); null for web links and in-page anchors. */
export function hrefRef(href: string): FileRef | null {
  if (href === "" || href.startsWith("#")) return null;
  return parseFileRef(href, { delimited: true });
}

/** Group candidates by the directories they resolve against, so one
 *  request carries each group's `base` + `bases` ladder. */
export function groupByBases(
  candidates: string[],
  ctx: LinkContext,
): { bases: string[]; candidates: string[] }[] {
  const groups = new Map<string, { bases: string[]; candidates: string[] }>();
  for (const c of candidates) {
    const bases = resolveBases(ctx, c);
    if (bases.length === 0) continue; // nowhere to resolve it: a miss
    const key = bases.join("\u0000");
    const g = groups.get(key);
    if (g === undefined) groups.set(key, { bases, candidates: [c] });
    else g.candidates.push(c);
  }
  return [...groups.values()];
}

// --- the per-chat resolver -------------------------------------------------------------

/** A miss stands this long; the next stamp pass after it asks again, so a
 *  path mentioned before the agent creates it links once it exists. */
export const MISS_TTL_MS = 15_000;
/** Coalesce every renderer's candidates (a replayed transcript mounts
 *  dozens of messages at once) into one request. */
const BATCH_DELAY_MS = 24;
/** Per flush — fsValidate's own ceiling (VALIDATE_CAP). */
const BATCH_CAP = 200;
/** Verdicts kept per chat. */
const CACHE_CAP = 4000;

interface Waiter {
  promise: Promise<boolean>;
  resolve: (linkable: boolean) => void;
}

export interface PathResolverOptions {
  /** Labels ambiguous matches relative to this (the workspace root). */
  root?: () => string | null;
  now?: () => number;
}

/**
 * Batches, caches and expires /fs/validate answers for one chat. Renderers
 * `peek` synchronously while stamping and `resolve` what they could not
 * peek; the promise says whether anything became linkable, so a renderer
 * re-stamps only then. A transient failure is never cached: the candidate
 * stays unknown and the next pass (or a click) asks again.
 */
export class PathResolver {
  readonly #validate: (candidates: string[]) => Promise<ValidateAnswer>;
  readonly #root: () => string | null;
  readonly #now: () => number;
  readonly #cache = new Map<string, { res: Resolution; at: number }>();
  readonly #waiting = new Map<string, Waiter>();
  #queue: string[] = [];
  #timer: ReturnType<typeof setTimeout> | null = null;
  #immediate = false;
  #flushing = false;
  #disposed = false;
  readonly #listeners = new Set<() => void>();

  constructor(
    validate: (candidates: string[]) => Promise<ValidateAnswer>,
    opts: PathResolverOptions = {},
  ) {
    this.#validate = validate;
    this.#root = opts.root ?? (() => null);
    this.#now = opts.now ?? Date.now;
  }

  /** The standing verdict, or undefined when unknown, in flight, or an
   *  expired miss. */
  peek(candidate: string): Resolution | undefined {
    const e = this.#cache.get(candidate);
    if (e === undefined) return undefined;
    if (e.res.state === "miss" && this.#now() - e.at >= MISS_TTL_MS) return undefined;
    return e.res;
  }

  /**
   * Ask about every candidate without a standing verdict (queued ones join
   * their pending batch). Resolves true when at least one of them came back
   * linkable (a hit or ambiguous), false otherwise — including on failure.
   */
  resolve(candidates: Iterable<string>, opts: { immediate?: boolean } = {}): Promise<boolean> {
    if (this.#disposed) return Promise.resolve(false);
    const waits: Promise<boolean>[] = [];
    for (const c of candidates) {
      const w = this.#waiting.get(c);
      if (w !== undefined) {
        waits.push(w.promise);
        continue;
      }
      if (this.peek(c) !== undefined) continue;
      let resolve: (linkable: boolean) => void = () => {};
      const promise = new Promise<boolean>((r) => (resolve = r));
      this.#waiting.set(c, { promise, resolve });
      this.#queue.push(c);
      waits.push(promise);
    }
    if (waits.length === 0) return Promise.resolve(false);
    this.#schedule(opts.immediate === true);
    return Promise.all(waits).then((r) => r.some(Boolean));
  }

  /** A click: ask again now, even when the standing answer is a miss. */
  async resolveNow(candidate: string): Promise<Resolution | undefined> {
    if (this.#cache.get(candidate)?.res.state === "miss") this.#cache.delete(candidate);
    await this.resolve([candidate], { immediate: true });
    return this.peek(candidate);
  }

  /** Drop every miss (a turn just ended, so files the agent mentioned may
   *  exist now) and tell the renderers, which re-stamp what they missed. */
  expireMisses(): void {
    let dropped = false;
    for (const [k, v] of this.#cache) {
      if (v.res.state !== "miss") continue;
      this.#cache.delete(k);
      dropped = true;
    }
    if (dropped) for (const fn of this.#listeners) fn();
  }

  /** Called after `expireMisses` dropped any. Returns the unsubscribe. */
  onExpire(fn: () => void): () => void {
    this.#listeners.add(fn);
    return () => this.#listeners.delete(fn);
  }

  /** A match's label: workspace-relative when under the root. */
  label(path: string): string {
    const root = this.#root();
    return root !== null ? workspaceRelative(path, root) : path;
  }

  dispose(): void {
    this.#disposed = true;
    if (this.#timer !== null) clearTimeout(this.#timer);
    this.#timer = null;
    for (const w of this.#waiting.values()) w.resolve(false);
    this.#waiting.clear();
    this.#queue = [];
    this.#listeners.clear();
  }

  #schedule(immediate: boolean): void {
    if (this.#flushing) {
      // The running flush drains the queue when its request lands.
      if (immediate) this.#immediate = true;
      return;
    }
    if (this.#timer !== null) {
      if (!immediate) return;
      clearTimeout(this.#timer);
    }
    this.#timer = setTimeout(
      () => {
        this.#timer = null;
        void this.#flush();
      },
      immediate ? 0 : BATCH_DELAY_MS,
    );
  }

  async #flush(): Promise<void> {
    this.#flushing = true;
    try {
      while (this.#queue.length > 0 && !this.#disposed) {
        const batch = this.#queue.splice(0, BATCH_CAP);
        let answer: ValidateAnswer | null;
        try {
          answer = await this.#validate(batch);
        } catch {
          answer = null;
        }
        if (this.#disposed) return;
        if (answer === null) {
          // Transient (daemon unreachable, a failed request): nothing is
          // cached, every waiter hears "nothing new", the next pass retries.
          this.#settle(batch, false);
          this.#settle(this.#queue.splice(0), false);
          return;
        }
        const unchecked = new Set(answer.unchecked ?? []);
        const at = this.#now();
        for (const c of batch) {
          if (unchecked.has(c)) {
            this.#settle([c], false);
            continue;
          }
          const res = verdict(answer, c);
          this.#store(c, res, at);
          this.#settle([c], res.state !== "miss");
        }
      }
    } finally {
      this.#flushing = false;
      if (this.#immediate) {
        this.#immediate = false;
        if (this.#queue.length > 0) this.#schedule(true);
      }
    }
  }

  #settle(candidates: string[], linkable: boolean): void {
    for (const c of candidates) {
      const w = this.#waiting.get(c);
      this.#waiting.delete(c);
      w?.resolve(linkable);
    }
  }

  #store(candidate: string, res: Resolution, at: number): void {
    this.#cache.delete(candidate);
    if (this.#cache.size >= CACHE_CAP) {
      // Oldest quarter out (Map order is insertion order, refreshed above).
      let drop = CACHE_CAP / 4;
      for (const k of this.#cache.keys()) {
        if (drop-- <= 0) break;
        this.#cache.delete(k);
      }
    }
    this.#cache.set(candidate, { res, at });
  }
}

function verdict(answer: ValidateAnswer, candidate: string): Resolution {
  const hit = answer.valid[candidate];
  if (hit !== undefined) return { state: "hit", hit };
  const matches = answer.ambiguous?.[candidate] ?? [];
  if (matches.length === 1) return { state: "hit", hit: matches[0] };
  if (matches.length > 1) return { state: "ambiguous", matches };
  return { state: "miss" };
}

// --- opening ------------------------------------------------------------------------------

/** The context-menu rows for an ambiguous reference: one per match. */
export function matchMenuEntries(
  matches: PathHit[],
  label: (path: string) => string,
  open: (hit: PathHit) => void,
): ContextMenuEntry[] {
  return matches.map((m) => ({
    label: `${label(m.path)}${m.kind === "dir" && !m.path.endsWith("/") ? "/" : ""}`,
    onSelect: () => open(m),
  }));
}

/** Where a menu for a gesture on `anchor` opens: the pointer, or under the
 *  element for a keyboard activation (whose click carries no position). */
export function menuPoint(e: MouseEvent | KeyboardEvent, anchor: Element): { x: number; y: number } {
  if (e instanceof MouseEvent && (e.clientX !== 0 || e.clientY !== 0)) {
    return { x: e.clientX, y: e.clientY };
  }
  const r = anchor.getBoundingClientRect();
  return { x: r.left, y: r.bottom };
}

/** Open a resolution: a hit directly (a file at `reveal`), an ambiguous
 *  one by asking at `at` which match. */
export function openResolution(
  res: Resolution,
  open: OpenPathFn,
  opts: { split?: boolean; reveal?: Reveal; at: { x: number; y: number }; label: (p: string) => string },
): boolean {
  const go = (hit: PathHit) =>
    open(hit.path, hit.kind, {
      split: opts.split,
      reveal: hit.kind === "file" ? opts.reveal : undefined,
    });
  if (res.state === "hit") {
    go(res.hit);
    return true;
  }
  if (res.state === "ambiguous") {
    contextMenu.openAtPoint(opts.at.x, opts.at.y, matchMenuEntries(res.matches, opts.label, go));
    return true;
  }
  return false;
}

/**
 * A click on something that names a path but was never stamped (a tool
 * card location, a local link that missed): resolve it now against the
 * session's directories and open it. An absolute path the daemon could not
 * confirm still opens as a file, so the viewer can say what is wrong.
 */
export async function resolveAndOpen(
  resolver: PathResolver | undefined,
  candidate: string,
  open: OpenPathFn,
  opts: { split?: boolean; reveal?: Reveal; at: { x: number; y: number } },
): Promise<boolean> {
  const res = resolver !== undefined ? await resolver.resolveNow(candidate) : undefined;
  if (res !== undefined) {
    const label = (p: string) => resolver?.label(p) ?? p;
    if (openResolution(res, open, { ...opts, label })) return true;
  }
  if (candidate.startsWith("/")) {
    open(candidate, "file", { split: opts.split, reveal: opts.reveal });
    return true;
  }
  return false;
}
