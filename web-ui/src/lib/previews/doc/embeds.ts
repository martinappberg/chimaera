/**
 * A document's image-shaped references — `![alt](target#frag)` and
 * `![[name]]`, inline or as a block of their own — resolved together: the
 * document's whole set goes to the daemon in ONE `fs/resolve_targets` round
 * trip (`DocEmbeds.sync`), again whenever the set changes, and every view of
 * the document (the reading article, live mode's blocks) draws from the
 * answers. An answer carries the file's version and a `/raw` ticket the
 * daemon keeps for that version, so a block drawn again reuses its URL (no
 * flash) and an overwritten file gets a new one.
 *
 * Targets resolve as document links do: beside the document, then the
 * workspace root (a root-relative `/x` too). An `![[name]]` that misses
 * there is looked up by name, as Obsidian finds it.
 */
import type { Tree } from "@lezer/common";
import { inlineOf, type DocText, type InlineOptions } from "../mdTable";
import { isMath } from "../mdMath";
import { dirname, fsValidate, safeDecodeUri } from "../files";
import { isMissing, resolveTargets, splitTarget, type TargetResult } from "../../shared/embed/embed";
import type { LinkContext } from "../docLinks";
import { embedSpecOf, type EmbedRef } from "./render";

/** Syntax that holds no image reference: never descended into. */
const OPAQUE = new Set([
  "FencedCode",
  "CodeBlock",
  "HTMLBlock",
  "CommentBlock",
  "ProcessingInstructionBlock",
  "LinkReference",
  "InlineCode",
  "HTMLTag",
  "Comment",
  "ProcessingInstruction",
  "Autolink",
  "URL",
]);

/** Every image-shaped reference in the document from `from` on (past a
 *  frontmatter block), in order, each once. Reads the syntax tree only —
 *  no rendering — so a long document costs one walk. */
export function embedTargets(tree: Tree, doc: DocText, opts: InlineOptions, from = 0): EmbedRef[] {
  const out = new Map<string, EmbedRef>();
  tree.iterate({
    from,
    enter: (n) => {
      if (OPAQUE.has(n.name) || isMath(n.type)) return false;
      if (n.name !== "Image" && n.name !== "Wikilink") return;
      const parent = n.node.parent;
      if (parent !== null)
        for (const i of inlineOf(parent, n.from, n.to, doc, opts)) {
          const spec = embedSpecOf(i);
          if (spec !== null) out.set(refKey(spec), { target: spec.target, byName: spec.byName });
        }
      return false;
    },
  });
  return [...out.values()];
}

/** One answer per reference: an `![[name]]` and a path may read alike and
 *  still resolve differently. */
export function refKey(ref: EmbedRef): string {
  return `${ref.byName ? "n" : "p"}\u0000${ref.target}`;
}

function sameAnswer(a: TargetResult, b: TargetResult): boolean {
  if (isMissing(a) || isMissing(b)) return isMissing(a) === isMissing(b);
  return a.path === b.path && a.kind === b.kind && a.version === b.version && a.ticket === b.ticket;
}

/** Asks within this long go out as one request (a render mounts every
 *  embed it draws in one task). */
const COALESCE_MS = 16;
/** An answer older than this is asked again when an embed draws from it or
 *  comes into view: it picks up a new version, and re-minting keeps the
 *  ticket alive (the daemon keeps one 10 minutes past its last mint). */
const REFRESH_MS = 60_000;
/** Past this an answer's ticket may be gone: not drawn from, asked again. */
const TICKET_MS = 8 * 60_000;
const ANSWERS_MAX = 1000;

interface Ask {
  ref: EmbedRef;
  promise: Promise<TargetResult | null>;
  resolve: (r: TargetResult | null) => void;
}

export type EmbedListener = (key: string, answer: TargetResult) => void;

export class DocEmbeds {
  private readonly answers = new Map<string, { r: TargetResult; at: number }>();
  private queued = new Map<string, Ask>();
  private readonly inflight = new Map<string, Ask>();
  private timer: ReturnType<typeof setTimeout> | null = null;
  private readonly listeners = new Set<EmbedListener>();
  private synced: EmbedRef[] = [];
  private syncedKey: string | null = null;
  private disposed = false;

  constructor(
    /** The document's absolute path: targets resolve beside it. */
    readonly docPath: string,
    /** Read when asking: the workspace root and id the targets fall back to. */
    private readonly links: () => LinkContext,
  ) {}

  /** The answer to draw `ref` with now, if one is fresh enough; an aging
   *  one is asked again meanwhile (a changed answer reaches subscribers). */
  answer(ref: EmbedRef): TargetResult | undefined {
    const a = this.answers.get(refKey(ref));
    if (a === undefined) return undefined;
    const age = Date.now() - a.at;
    if (age >= TICKET_MS) return undefined;
    if (age >= REFRESH_MS) void this.ask(ref);
    return a.r;
  }

  /** An embed came into view: ask again when its answer is aging. */
  touch(ref: EmbedRef): void {
    const a = this.answers.get(refKey(ref));
    if (a === undefined || Date.now() - a.at >= REFRESH_MS) void this.ask(ref);
  }

  /**
   * Resolve `ref`, joined with every other ask in the same frame and with a
   * request already on the wire for it. Null: unknown (the daemon did not
   * answer), never a miss.
   */
  ask(ref: EmbedRef): Promise<TargetResult | null> {
    const key = refKey(ref);
    const pending = this.queued.get(key) ?? this.inflight.get(key);
    if (pending !== undefined) return pending.promise;
    if (this.disposed) return Promise.resolve(null);
    let resolve: (r: TargetResult | null) => void = () => {};
    const promise = new Promise<TargetResult | null>((r) => (resolve = r));
    this.queued.set(key, { ref, promise, resolve });
    this.timer ??= setTimeout(() => void this.flush(), COALESCE_MS);
    return promise;
  }

  /** The document's references as they stand: when the set differs from
   *  the last one, every one of them is asked again in one request. */
  sync(refs: readonly EmbedRef[]): void {
    const byKey = new Map(refs.map((r) => [refKey(r), r]));
    const key = [...byKey.keys()].sort().join("\u0001");
    if (key === this.syncedKey) return;
    this.syncedKey = key;
    this.synced = [...byKey.values()];
    for (const r of this.synced) void this.ask(r);
  }

  /** The document changed on disk (an agent's rewrite regenerates its
   *  figures too): ask for the whole set again. */
  refresh(): void {
    for (const r of this.synced) void this.ask(r);
  }

  /** Changed answers (a first answer, a new version, a file gone). */
  subscribe(fn: EmbedListener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  dispose(): void {
    this.disposed = true;
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    for (const a of this.queued.values()) a.resolve(null);
    this.queued.clear();
    this.listeners.clear();
  }

  private async flush(): Promise<void> {
    this.timer = null;
    const batch = [...this.queued.values()];
    this.queued = new Map();
    for (const a of batch) this.inflight.set(refKey(a.ref), a);
    let results = new Map<string, TargetResult>();
    try {
      results = await this.resolve(batch.map((a) => a.ref));
    } catch {
      // unreachable: every ask stays unknown
    }
    const at = Date.now();
    for (const a of batch) {
      const key = refKey(a.ref);
      if (this.inflight.get(key) === a) this.inflight.delete(key);
      const r = results.get(key) ?? null;
      if (r !== null && !this.disposed) this.store(key, r, at);
      a.resolve(r);
    }
  }

  private store(key: string, r: TargetResult, at: number): void {
    const prev = this.answers.get(key);
    this.answers.delete(key);
    this.answers.set(key, { r, at });
    if (this.answers.size > ANSWERS_MAX) {
      const oldest = this.answers.keys().next().value;
      if (oldest !== undefined) this.answers.delete(oldest);
    }
    if (prev === undefined || !sameAnswer(prev.r, r)) for (const fn of this.listeners) fn(key, r);
  }

  private async resolve(refs: readonly EmbedRef[]): Promise<Map<string, TargetResult>> {
    const out = new Map<string, TargetResult>();
    if (refs.length === 0 || this.disposed) return out;
    const ctx = this.links();
    const base = dirname(this.docPath);
    const strict = await resolveTargets(
      refs.map((r) => r.target),
      base,
      { bases: ctx.wsRoot !== null ? [ctx.wsRoot] : [], workspaceId: ctx.workspaceId },
    );
    const named: EmbedRef[] = [];
    for (const r of refs) {
      const a = strict[r.target];
      if (a === undefined) continue;
      if (r.byName && isMissing(a)) named.push(r);
      else out.set(refKey(r), a);
    }
    if (named.length > 0) for (const [k, a] of await this.byName(named, base, ctx.workspaceId)) out.set(k, a);
    return out;
  }

  /** `![[name]]`s missing beside the document: the daemon's by-name lookup
   *  (a unique match in the workspace), then those files' answers. */
  private async byName(refs: readonly EmbedRef[], base: string, ws: string | null): Promise<Map<string, TargetResult>> {
    const out = new Map<string, TargetResult>();
    const nameOf = (r: EmbedRef): string => safeDecodeUri(splitTarget(r.target).path);
    const miss = (): Map<string, TargetResult> => {
      for (const r of refs) out.set(refKey(r), { missing: true });
      return out;
    };
    let valid: Awaited<ReturnType<typeof fsValidate>>["valid"];
    try {
      valid = (await fsValidate(refs.map(nameOf), base, ws)).valid;
    } catch {
      return out; // unknown, not missing
    }
    const found = new Map<EmbedRef, string>();
    for (const r of refs) {
      const hit = valid[nameOf(r)];
      if (hit !== undefined && hit.kind === "file") found.set(r, hit.path);
    }
    if (found.size === 0) return miss();
    let answers: Record<string, TargetResult>;
    try {
      answers = await resolveTargets([...new Set(found.values())], "/");
    } catch {
      return out;
    }
    for (const r of refs) {
      const path = found.get(r);
      out.set(refKey(r), (path !== undefined ? answers[path] : undefined) ?? { missing: true });
    }
    return out;
  }
}
