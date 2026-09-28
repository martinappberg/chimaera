/**
 * Clickable paths in terminals — the context bridge's return direction.
 *
 * An xterm link provider scans rendered lines (soft-wrapped rows joined) for
 * file references with `shared/fileRef.ts`, the parser chat uses too:
 * absolute, ~/, ./ and ../, workspace-relative, bare filenames with an
 * extension, `@mentions`, `a/`/`b/` diff sides, `…/` abbreviations, Unicode
 * names, with an optional `:12` / `:12:3` / `#L12-L20` / `(12,3)` line
 * suffix. A path a TUI hard-wrapped onto the next row is re-joined. Candidates
 * are batch-validated against the daemon (POST /fs/validate with the
 * session's base ladder — live cwd, spawn cwd, workspace root — plus the
 * workspace index fallbacks) so ONLY real files and dirs ever underline;
 * verdicts are cached with a short TTL and prefetched per viewport change, so
 * hovering is instant. Click opens the file in the active pane at the
 * referenced line (Cmd/Ctrl+click = new split); dirs open in the Finder; a
 * name several files answer to asks which in the shared context menu. Works
 * identically in every session — agents and shells.
 */

import type { ILink, ILinkProvider, Terminal } from "@xterm/xterm";
import { fsValidate, VALIDATE_MAX, type ValidatedPath } from "../previews/files";
import { contextMenu } from "../shared/contextMenu.svelte";
import {
  extractFileRefs,
  findFileRef,
  resolveBases,
  revealOf,
  type FileRef,
  type LinkContext,
} from "../shared/fileRef";
import type { PathKind } from "../shared/openPath";
import { workspaceRelative } from "../shared/reference";
import type { Reveal } from "../shared/reveal";

export type { LinkContext, PathKind };

/** App-level wiring for the pooled terminals' link providers. */
export interface LinkHost {
  context(sessionId: string): LinkContext;
  /** A confirmed link was activated. `split` = Cmd/Ctrl held. */
  open(sessionId: string, path: string, kind: PathKind, opts: { split: boolean; reveal?: Reveal }): void;
}

// --- candidate extraction (pure) ---------------------------------------------

/** One path-like candidate found in a line of terminal text. */
export interface Candidate {
  /** The clean path candidate (what gets validated / resolved). */
  raw: string;
  /** 0-based start index into the scanned string. */
  start: number;
  /** Length in the scanned string INCLUDING any line suffix. */
  length: number;
  /** 1-based line number from a `:42` / `#L42` suffix, if present. */
  line: number | null;
  ref: FileRef;
}

/**
 * Scan `text` for path-like candidates (indexes into that string). With
 * `bare`, also emit single-segment names (bare directories / extensionless
 * files like `crates`, `justfile`) — reserved for the on-hover path, never
 * the prefetch, so agent/terminal prose is never mass-validated on the
 * daemon (it lives on shared login nodes). Whether such a name actually
 * becomes a link is then decided per line by [`bareLinkable`].
 */
export function extractCandidates(text: string, bare = false): Candidate[] {
  return extractFileRefs(text, { bare }).map((f) => ({
    raw: f.ref.path,
    start: f.start,
    length: f.end - f.start,
    line: f.ref.line ?? null,
    ref: f.ref,
  }));
}

/** The permissions column that opens every `ls -l` entry line. */
const LS_LONG_PERMS_RE = /^[bcdlps-][rwxsStT-]{9}[@+.]?$/;

/** Whitespace words of a line, as the candidates they would be. */
function lineWords(text: string): string[] {
  const out: string[] = [];
  for (const w of text.trim().split(/\s+/)) {
    if (w.length === 0) continue;
    out.push(findFileRef(w, { bare: true })?.ref.path ?? w);
  }
  return out;
}

/**
 * Which bare single-segment names on a line may become links.
 *
 * A bare name (`crates`, `justfile`) is indistinguishable from an English word,
 * so linking every one that happens to name a real entry would underline
 * `docs`/`site`/`target` mid-sentence — worse on a case-insensitive filesystem,
 * where prose "license" resolves to `LICENSE`. Instead only two line shapes are
 * trusted, neither of which prose ever has:
 *
 *   - a **listing line**: every word on it resolves to a real path (plain `ls`)
 *   - an **`ls -l` entry**: the line opens with a permissions column, so its
 *     last word is the name
 *
 * Any other line contributes no bare links. Slashed and extensioned paths on
 * the line still link exactly as before, wherever they appear.
 */
export function bareLinkable(
  text: string,
  resolves: (word: string) => boolean,
): ReadonlySet<string> {
  const words = lineWords(text);
  if (words.length === 0) return new Set();
  if (words.every(resolves)) return new Set(words);
  if (LS_LONG_PERMS_RE.test(words[0])) {
    const name = words[words.length - 1];
    if (resolves(name)) return new Set([name]);
  }
  return new Set();
}

/** One row's part of a token a TUI hard-wrapped: [start, end) in that row. */
export interface WrapPiece {
  row: number;
  start: number;
  end: number;
}

/** Most rows one hard-wrapped path is followed across. */
const WRAP_MAX_ROWS = 4;
/** A hard wrap breaks at the TUI's box width, so a wrapped piece ends near
 *  its row's right edge (terminal rows are padded to the full width); a
 *  token that ends earlier was not wrapped. Keeps ordinary line pairs out of
 *  the daemon's batches. */
const WRAP_EDGE_SLACK = 20;

/**
 * Tokens a TUI hard-wrapped across rows. Ink (Claude Code, Codex) breaks a
 * long unbroken token at its box width and indents the continuation, so the
 * pieces sit on separate rows that xterm does not mark as soft-wrapped. For
 * each row that ends in a token, glue on the first token of the next row
 * (after its indentation) — and of the row after that while each
 * continuation fills its row. Every glued prefix of two or more pieces is a
 * candidate; the daemon decides which one is real.
 */
export function wrappedTokens(rows: string[]): { text: string; pieces: WrapPiece[] }[] {
  const out: { text: string; pieces: WrapPiece[] }[] = [];
  for (let i = 0; i + 1 < rows.length; i++) {
    const last = /(\S+)\s*$/.exec(rows[i]);
    if (last === null) continue;
    const lastEnd = last.index + last[1].length;
    if (rows[i].length - lastEnd > WRAP_EDGE_SLACK) continue;
    const pieces: WrapPiece[] = [{ row: i, start: last.index, end: lastEnd }];
    let text = last[1];
    for (let j = i + 1; j < rows.length && pieces.length < WRAP_MAX_ROWS; j++) {
      const lead = /^(\s*)(\S+)/.exec(rows[j]);
      if (lead === null) break;
      const start = lead[1].length;
      const end = start + lead[2].length;
      pieces.push({ row: j, start, end });
      text += lead[2];
      out.push({ text, pieces: [...pieces] });
      // Only a continuation that fills its row continues onto the next.
      if (rows[j].slice(end).trim() !== "" || rows[j].length - end > WRAP_EDGE_SLACK) break;
    }
  }
  return out;
}

/** A reference re-joined across hard-wrapped rows, with where each end sits. */
export interface WrappedRef {
  ref: FileRef;
  /** Row + offset of the first and last character the link covers. */
  first: { row: number; index: number };
  last: { row: number; index: number };
}

/** Parse `wrappedTokens` into references that genuinely cross a row break. */
export function wrappedRefs(rows: string[]): WrappedRef[] {
  const out: WrappedRef[] = [];
  for (const w of wrappedTokens(rows)) {
    const f = findFileRef(w.text);
    if (f === null) continue;
    // Map joined-string offsets back onto the pieces.
    const locate = (k: number): { row: number; index: number } | null => {
      let base = 0;
      for (const p of w.pieces) {
        const len = p.end - p.start;
        if (k < base + len) return { row: p.row, index: p.start + (k - base) };
        base += len;
      }
      return null;
    };
    const first = locate(f.start);
    const last = locate(f.end - 1);
    if (first === null || last === null || first.row === last.row) continue;
    // The reference must end in the LAST piece, or a shorter prefix already
    // covers it.
    if (last.row !== w.pieces[w.pieces.length - 1].row) continue;
    out.push({ ref: f.ref, first, last });
  }
  return out;
}

// --- validation cache ---------------------------------------------------------

/** How long a validation verdict stays fresh (files appear and vanish). */
const CACHE_TTL_MS = 15_000;
const CACHE_CAP = 5000;

/** A hit, the matches of an ambiguous name, or a miss. */
type Verdict = ValidatedPath | ValidatedPath[] | null;

interface CacheEntry {
  v: Verdict;
  at: number;
}

/** candidate resolved per base ladder + workspace: see [`cacheKey`]. */
const cache = new Map<string, CacheEntry>();
const inflight = new Map<string, Promise<void>>();

function cacheKey(bases: string[], ws: string | null, candidate: string): string {
  // Absolute and ~ candidates resolve the same under any base or workspace.
  // Relative verdicts key on the workspace too: the daemon's index
  // fallbacks make the answer depend on it, not just on the bases.
  const abs = candidate.startsWith("/") || candidate.startsWith("~");
  return abs ? `\u0000${candidate}` : `${bases.join("\u0001")}\u0000${ws ?? ""}\u0000${candidate}`;
}

function cacheGet(key: string): CacheEntry | undefined {
  const e = cache.get(key);
  if (e !== undefined && Date.now() - e.at > CACHE_TTL_MS) {
    cache.delete(key);
    return undefined;
  }
  return e;
}

function cachePut(key: string, v: Verdict): void {
  if (cache.size >= CACHE_CAP) {
    // Drop the stalest half; simple and rare.
    const entries = [...cache.entries()].sort((a, b) => a[1].at - b[1].at);
    for (const [k] of entries.slice(0, CACHE_CAP / 2)) cache.delete(k);
  }
  cache.set(key, { v, at: Date.now() });
}

/** Test/HMR hook. */
export function clearLinkCache(): void {
  cache.clear();
  inflight.clear();
}

/**
 * Ensure every candidate has a fresh verdict under this base ladder: one
 * batched /fs/validate call per miss set, deduped against in-flight
 * requests. Network failures and unanswered candidates cache nothing —
 * retried on the next pass.
 */
async function ensureValidated(bases: string[], ws: string | null, candidates: string[]): Promise<void> {
  const waits: Promise<void>[] = [];
  const missing: string[] = [];
  for (const c of new Set(candidates)) {
    const key = cacheKey(bases, ws, c);
    const w = inflight.get(key);
    if (w !== undefined) waits.push(w);
    else if (cacheGet(key) === undefined) missing.push(c);
  }
  // The server caps candidates per request; chunk to stay within it.
  for (let i = 0; i < missing.length; i += VALIDATE_MAX) {
    const chunk = missing.slice(i, i + VALIDATE_MAX);
    const p = fsValidate(chunk, bases[0], ws, bases.slice(1))
      .then((res) => {
        const unchecked = new Set(res.unchecked);
        for (const c of chunk) {
          if (unchecked.has(c)) continue;
          const many = res.ambiguous[c];
          const v: Verdict =
            res.valid[c] ?? (many === undefined ? null : many.length === 1 ? many[0] : many);
          cachePut(cacheKey(bases, ws, c), v);
        }
      })
      .catch(() => {
        // daemon unreachable: leave uncached, retry later
      })
      .finally(() => {
        for (const c of chunk) inflight.delete(cacheKey(bases, ws, c));
      });
    for (const c of chunk) inflight.set(cacheKey(bases, ws, c), p);
    waits.push(p);
  }
  await Promise.all(waits);
}

/** The cached verdict for `raw` under this context (null: miss or unknown). */
function lookup(raw: string, ctx: LinkContext): Verdict {
  const bases = resolveBases(ctx, raw);
  if (bases.length === 0) return null;
  return cacheGet(cacheKey(bases, ctx.workspaceId, raw))?.v ?? null;
}

async function validateAll(raws: string[], ctx: LinkContext): Promise<void> {
  const byBases = new Map<string, { bases: string[]; raws: string[] }>();
  for (const raw of raws) {
    const bases = resolveBases(ctx, raw);
    if (bases.length === 0) continue;
    const key = bases.join("\u0001");
    const g = byBases.get(key);
    if (g === undefined) byBases.set(key, { bases, raws: [raw] });
    else g.raws.push(raw);
  }
  await Promise.all(
    [...byBases.values()].map((g) => ensureValidated(g.bases, ctx.workspaceId, g.raws)),
  );
}

// --- buffer text mapping --------------------------------------------------------

export interface GroupText {
  text: string;
  /** cell x (0-based) for each string index. */
  cellOf: number[];
  /** buffer row (0-based) for each string index. */
  rowOf: number[];
}

/** A soft-wrapped run of buffer rows, joined. */
export interface GroupRun {
  start: number;
  end: number;
  g: GroupText;
}

/**
 * Join the wrapped-line group containing 0-based buffer row `row` into one
 * string, with an exact string-index → (row, cell) mapping (wide chars and
 * combined graphemes shift string indexes; the map absorbs that).
 * Shared with the URL link provider (`urlLinks.ts`).
 */
export function groupText(term: Terminal, row: number): GroupRun | null {
  const buf = term.buffer.active;
  if (row < 0 || row >= buf.length) return null;
  let start = row;
  while (start > 0 && buf.getLine(start)?.isWrapped) start -= 1;
  let end = row;
  while (end + 1 < buf.length && buf.getLine(end + 1)?.isWrapped) end += 1;

  const g: GroupText = { text: "", cellOf: [], rowOf: [] };
  for (let y = start; y <= end; y++) {
    const line = buf.getLine(y);
    if (line === undefined) break;
    for (let x = 0; x < line.length; x++) {
      const cell = line.getCell(x);
      if (cell === undefined || cell.getWidth() === 0) continue; // wide-char tail
      const chars = cell.getChars();
      const s = chars.length === 0 ? " " : chars;
      for (let k = 0; k < s.length; k++) {
        g.cellOf.push(x);
        g.rowOf.push(y);
      }
      g.text += s;
    }
  }
  return { start, end, g };
}

/** The groups around `grp` a hard-wrapped path could span: up to
 *  WRAP_MAX_ROWS - 1 on either side, in buffer order. */
function neighbourGroups(term: Terminal, grp: GroupRun): { groups: GroupRun[]; self: number } {
  const before: GroupRun[] = [];
  for (let s = grp.start; before.length < WRAP_MAX_ROWS - 1 && s > 0; ) {
    const g = groupText(term, s - 1);
    if (g === null) break;
    before.unshift(g);
    s = g.start;
  }
  const after: GroupRun[] = [];
  for (let e = grp.end; after.length < WRAP_MAX_ROWS - 1; ) {
    const g = groupText(term, e + 1);
    if (g === null) break;
    after.push(g);
    e = g.end;
  }
  return { groups: [...before, grp, ...after], self: before.length };
}

/** Inclusive 1-based cell position (Linkifier hit-test semantics). */
function cellAt(g: GroupText, index: number): { x: number; y: number } {
  return { x: g.cellOf[index] + 1, y: g.rowOf[index] + 1 };
}

// --- the provider ---------------------------------------------------------------

/**
 * One provider per pooled terminal. provideLinks is called for the hovered
 * buffer line; the viewport prefetch (registerPathLinks) keeps the cache warm
 * so links usually materialize synchronously.
 */
class PathLinkProvider implements ILinkProvider {
  constructor(
    private readonly term: Terminal,
    private readonly sessionId: string,
    private readonly host: LinkHost,
  ) {}

  provideLinks(bufferLineNumber: number, callback: (links: ILink[] | undefined) => void): void {
    const grp = groupText(this.term, bufferLineNumber - 1);
    if (grp === null) {
      callback(undefined);
      return;
    }
    const text = grp.g.text;
    // Hover path: one line group at a time, so bare directory names are worth
    // resolving here (the prefetch below never does this over whole screens).
    const candidates = extractCandidates(text, true);
    const strict = new Set(extractCandidates(text).map((c) => c.start));
    // Paths a TUI hard-wrapped across this row and its neighbours.
    const { groups, self } = neighbourGroups(this.term, grp);
    const joined = wrappedRefs(groups.map((x) => x.g.text)).filter(
      (w) => w.first.row <= self && w.last.row >= self,
    );
    if (candidates.length === 0 && joined.length === 0) {
      callback(undefined);
      return;
    }
    const ctx = this.host.context(this.sessionId);
    const raws = [...candidates.map((c) => c.raw), ...joined.map((w) => w.ref.path)];
    void validateAll(raws, ctx).then(() => {
      const links: ILink[] = [];
      // A re-joined path wins over the fragments of it on this row.
      const claimed: [number, number][] = [];
      for (const w of joined) {
        const v = lookup(w.ref.path, ctx);
        if (v === null) continue;
        const a = groups[w.first.row].g;
        const b = groups[w.last.row].g;
        links.push(
          this.link(
            w.ref.path,
            { start: cellAt(a, w.first.index), end: cellAt(b, w.last.index) },
            v,
            w.ref,
            ctx,
          ),
        );
        claimed.push([
          w.first.row === self ? w.first.index : 0,
          w.last.row === self ? w.last.index + 1 : text.length,
        ]);
      }
      // Bare names link only on a listing / `ls -l` line, never in prose.
      const bareOk = bareLinkable(text, (word) => lookup(word, ctx) !== null);
      for (const c of candidates) {
        if (!strict.has(c.start) && !bareOk.has(c.raw)) continue;
        const endIdx = c.start + c.length - 1;
        if (endIdx >= grp.g.cellOf.length) continue;
        if (claimed.some(([from, to]) => c.start < to && endIdx >= from)) continue;
        const v = lookup(c.raw, ctx);
        if (v === null) continue;
        links.push(
          this.link(
            text.slice(c.start, c.start + c.length),
            { start: cellAt(grp.g, c.start), end: cellAt(grp.g, endIdx) },
            v,
            c.ref,
            ctx,
          ),
        );
      }
      callback(links.length > 0 ? links : undefined);
    });
  }

  private link(
    text: string,
    range: ILink["range"],
    v: ValidatedPath | ValidatedPath[],
    ref: FileRef,
    ctx: LinkContext,
  ): ILink {
    return {
      text,
      range,
      activate: (event: MouseEvent) => {
        const split = event.metaKey || event.ctrlKey;
        const reveal = revealOf(ref);
        const open = (hit: ValidatedPath) =>
          this.host.open(this.sessionId, hit.path, hit.kind, {
            split,
            reveal: hit.kind === "file" ? reveal : undefined,
          });
        if (!Array.isArray(v)) {
          open(v);
          return;
        }
        // Several files answer to this name: ask which.
        contextMenu.openAt(
          event,
          v.map((hit) => ({
            label: ctx.root !== null ? workspaceRelative(hit.path, ctx.root) : hit.path,
            onSelect: () => open(hit),
          })),
        );
      },
    };
  }
}

/** A constantly animating TUI renders every frame; a debounce would never
 *  fire under it, so the prefetch runs at most this often instead. */
const PREFETCH_INTERVAL_MS = 300;

/**
 * Wire path links into a pooled terminal: the link provider plus a throttled
 * viewport prefetch (fired on render, i.e. output/scroll/resize) that batch-
 * validates every candidate on screen so hover never waits on the network.
 * Returns a dispose function.
 */
export function registerPathLinks(term: Terminal, sessionId: string, host: LinkHost): () => void {
  const provider = term.registerLinkProvider(new PathLinkProvider(term, sessionId, host));

  let timer: ReturnType<typeof setTimeout> | null = null;
  const prefetch = () => {
    timer = null;
    const buf = term.buffer.active;
    const groups: GroupRun[] = [];
    const last = buf.viewportY + term.rows - 1;
    for (let row = buf.viewportY; row <= last; ) {
      const g = groupText(term, row);
      if (g === null) break;
      groups.push(g);
      row = g.end + 1;
    }
    const raws = groups.flatMap((g) => extractCandidates(g.g.text).map((c) => c.raw));
    for (const w of wrappedRefs(groups.map((g) => g.g.text))) raws.push(w.ref.path);
    if (raws.length > 0) void validateAll(raws, host.context(sessionId));
  };
  const schedule = () => {
    if (timer === null) timer = setTimeout(prefetch, PREFETCH_INTERVAL_MS);
  };
  const render = term.onRender(schedule);
  schedule();

  return () => {
    if (timer !== null) clearTimeout(timer);
    render.dispose();
    provider.dispose();
  };
}
