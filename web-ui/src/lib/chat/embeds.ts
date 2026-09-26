/**
 * Files a chat shows as embed cards: agent prose `![alt](figs/plot.png)`
 * and the paths a turn's gallery weighs. Targets resolve the way the chat's
 * path links do — against the session's live directory, then where it
 * started, then the workspace root (`shared/fileRef.ts` `resolveBases`) —
 * but strictly (`fs/resolve_targets`): an embed names one file, so no
 * basename guess ever picks a different one. Every card that asks within a
 * few milliseconds shares one request per base ladder, and an answer is
 * reused briefly (a replayed transcript mounts dozens of cards at once).
 */

import { resolveBases, type LinkContext } from "../shared/fileRef";
import { resolveTargets, type TargetResult } from "../shared/embed/embed";

/** Coalesce every card mounted in one render into one request. */
const BATCH_DELAY_MS = 24;
/** How long an answer stands before a new ask re-checks: a hit refreshes
 *  itself on disk changes; a miss is re-asked by its card on screen. */
const HIT_TTL_MS = 30_000;
const MISS_TTL_MS = 5_000;
const CACHE_CAP = 1000;

interface Pending {
  target: string;
  bases: string[];
  waiters: ((r: TargetResult | null) => void)[];
}

export class EmbedResolver {
  readonly #context: () => LinkContext;
  readonly #cache = new Map<string, { r: TargetResult; at: number }>();
  #queue = new Map<string, Pending>();
  #timer: ReturnType<typeof setTimeout> | null = null;
  #disposed = false;

  constructor(context: () => LinkContext) {
    this.#context = context;
  }

  #key(ctx: LinkContext, target: string, bases: string[]): string {
    return [ctx.workspaceId ?? "", ...bases, "", target].join("\u0000");
  }

  /**
   * The answer for one target as written (fragment allowed; the daemon
   * ignores it). Null when it could not be asked (no directory known for a
   * relative path, the daemon unreachable): unknown, not missing.
   */
  resolve(target: string): Promise<TargetResult | null> {
    if (this.#disposed) return Promise.resolve(null);
    const ctx = this.#context();
    const bases = resolveBases(ctx, target.split("#")[0] ?? target);
    if (bases.length === 0) return Promise.resolve(null);
    const key = this.#key(ctx, target, bases);
    const hit = this.#cache.get(key);
    if (hit !== undefined) {
      const ttl = "missing" in hit.r ? MISS_TTL_MS : HIT_TTL_MS;
      if (Date.now() - hit.at < ttl) return Promise.resolve(hit.r);
    }
    return new Promise((resolve) => {
      const q = this.#queue.get(key);
      if (q !== undefined) q.waiters.push(resolve);
      else this.#queue.set(key, { target, bases, waiters: [resolve] });
      if (this.#timer === null) this.#timer = setTimeout(() => void this.#flush(), BATCH_DELAY_MS);
    });
  }

  async #flush(): Promise<void> {
    this.#timer = null;
    const queue = this.#queue;
    this.#queue = new Map();
    const workspaceId = this.#context().workspaceId;
    // One request per base ladder.
    const groups = new Map<string, { bases: string[]; entries: [string, Pending][] }>();
    for (const entry of queue) {
      const ladder = entry[1].bases.join("\u0000");
      const g = groups.get(ladder);
      if (g === undefined) groups.set(ladder, { bases: entry[1].bases, entries: [entry] });
      else g.entries.push(entry);
    }
    await Promise.all(
      [...groups.values()].map(async ({ bases, entries }) => {
        let results: Record<string, TargetResult> | null;
        try {
          results = await resolveTargets(
            entries.map(([, p]) => p.target),
            bases[0],
            { bases: bases.slice(1), workspaceId },
          );
        } catch {
          results = null;
        }
        const at = Date.now();
        for (const [key, p] of entries) {
          const r = results?.[p.target] ?? null;
          if (r !== null && !this.#disposed) this.#store(key, r, at);
          for (const w of p.waiters) w(r);
        }
      }),
    );
  }

  #store(key: string, r: TargetResult, at: number): void {
    this.#cache.delete(key);
    if (this.#cache.size >= CACHE_CAP) {
      const oldest = this.#cache.keys().next().value;
      if (oldest !== undefined) this.#cache.delete(oldest);
    }
    this.#cache.set(key, { r, at });
  }

  dispose(): void {
    this.#disposed = true;
    if (this.#timer !== null) clearTimeout(this.#timer);
    this.#timer = null;
    for (const p of this.#queue.values()) for (const w of p.waiters) w(null);
    this.#queue.clear();
    this.#cache.clear();
  }
}
