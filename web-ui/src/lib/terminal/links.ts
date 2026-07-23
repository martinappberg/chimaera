/**
 * Clickable paths in terminals — the context bridge's return direction.
 *
 * An xterm link provider scans rendered lines (wrapped rows joined) for
 * path-like candidates: absolute, ~/, ./ and ../, workspace-relative like
 * results/qc/report.html, bare filenames with an extension, all with an
 * optional :line suffix. Candidates are batch-validated against the daemon
 * (POST /fs/validate — relative candidates resolve against the session's
 * live cwd, then the workspace root, and a bare `name.ext` additionally via
 * the daemon's unique-basename workspace fallback) so ONLY real files and
 * dirs ever underline; results are cached with a short TTL and prefetched per
 * viewport change, so hovering is instant. Click opens the file in an
 * adjacent pane (Cmd/Ctrl+click = new split); dirs reveal in the file tree.
 * Works identically in every session — agents and shells.
 */

import type { ILink, ILinkProvider, Terminal } from "@xterm/xterm";
import { fsValidate, VALIDATE_MAX, type ValidatedPath } from "../previews/files";

/** What a confirmed path candidate resolved to. */
export type PathKind = "file" | "dir";

/** Per-session context the provider resolves relative candidates with. */
export interface LinkContext {
  /** The session's live working directory (cwd_current, else spawn cwd). */
  cwd: string | null;
  /** The active workspace root (second base for workspace-relative paths). */
  root: string | null;
  /**
   * The session's workspace id — sent with /fs/validate so the daemon's
   * bare-basename fallback can resolve a lone `name.ext` mentioned about a
   * file living in a subdirectory. Null degrades to base-only resolution.
   */
  workspaceId: string | null;
}

/** App-level wiring for the pooled terminals' link providers. */
export interface LinkHost {
  context(sessionId: string): LinkContext;
  /** A confirmed link was activated. `newSplit` = Cmd/Ctrl held. */
  open(sessionId: string, path: string, kind: PathKind, newSplit: boolean): void;
}

// --- candidate extraction (pure) ---------------------------------------------

/** One path-like candidate found in a line of terminal text. */
export interface Candidate {
  /** The candidate as written (what gets validated / resolved). */
  raw: string;
  /** 0-based start index into the scanned string. */
  start: number;
  /** Length in the scanned string INCLUDING any :line suffix. */
  length: number;
  /** 1-based line number from a `:42` suffix, if present. */
  line: number | null;
}

/** Longest candidate worth validating. */
const CANDIDATE_MAX_LEN = 512;

/** Path-ish token: the conservative charset real workspace paths use. */
const TOKEN_RE = /[A-Za-z0-9._+@%#$~/-]+/g;
/** Bare filename qualifier: an extension that starts with a letter. */
const BARE_EXT_RE = /\.[A-Za-z][A-Za-z0-9]{0,7}$/;

/**
 * True when `t` looks like a path worth validating: absolute, ~/, ./, ../,
 * relative-with-slash, or a bare filename with an extension. URL tails
 * ("//host/…") and flag-like tokens never qualify — and everything that does
 * still only links if the daemon confirms it exists.
 *
 * `bare` additionally admits a single-segment name with no slash and no
 * extension — a directory like `crates` or an extensionless file like
 * `justfile`. That is offered ONLY on hover (`extractCandidates(text, true)`),
 * never in the whole-screen prefetch, so agent/terminal prose is never
 * mass-validated on the daemon (it lives on shared login nodes). Whether such
 * a name actually becomes a link is then decided per line by [`bareLinkable`].
 */
function qualifies(t: string, bare: boolean): boolean {
  if (t.length < 2 || t.length > CANDIDATE_MAX_LEN) return false;
  if (!/[A-Za-z0-9]/.test(t)) return false;
  if (t.startsWith("//") || t.startsWith("-")) return false;
  if (t.startsWith("/") || t.startsWith("~/") || t.startsWith("./") || t.startsWith("../")) {
    return true;
  }
  if (t.includes("/")) return true;
  if (BARE_EXT_RE.test(t)) return true;
  // A bare name resolved against the session cwd. Requires a letter, so
  // version numbers (1.2.3, 4.8) stay out; the daemon still decides existence.
  return bare && /[A-Za-z]/.test(t);
}

/**
 * Scan `text` for path-like candidates (line indexes into that string). With
 * `bare`, also emit single-segment names (bare directories / extensionless
 * files) — reserved for the on-hover path, never the prefetch.
 */
export function extractCandidates(text: string, bare = false): Candidate[] {
  const out: Candidate[] = [];
  TOKEN_RE.lastIndex = 0;
  for (let m = TOKEN_RE.exec(text); m !== null; m = TOKEN_RE.exec(text)) {
    let raw = m[0];
    let start = m.index;
    // Sentence punctuation is never part of a candidate: trim trailing
    // dots/commas ("see main.rs.") and stray leading dots (ellipses). But keep
    // a single leading dot that begins a dotfile/dotfolder — `.claude`, `.env` —
    // (a `.` immediately followed by a letter); `./` and `../` keep both dots.
    while (raw.endsWith(".") || raw.endsWith(",")) raw = raw.slice(0, -1);
    while (
      raw.startsWith(".") &&
      !raw.startsWith("./") &&
      !raw.startsWith("../") &&
      !/^\.[A-Za-z]/.test(raw)
    ) {
      raw = raw.slice(1);
      start += 1;
    }
    if (!qualifies(raw, bare)) continue;
    // Optional :line suffix directly after the path (underline covers it).
    const after = /^:(\d{1,7})/.exec(text.slice(start + raw.length));
    out.push({
      raw,
      start,
      length: raw.length + (after !== null ? after[0].length : 0),
      line: after !== null ? Number.parseInt(after[1], 10) : null,
    });
  }
  return out;
}

/** The permissions column that opens every `ls -l` entry line. */
const LS_LONG_PERMS_RE = /^[bcdlps-][rwxsStT-]{9}[@+.]?$/;

/** Whitespace words of a line, trimmed of sentence punctuation like candidates. */
function lineWords(text: string): string[] {
  const out: string[] = [];
  for (const w of text.trim().split(/\s+/)) {
    let x = w;
    while (x.endsWith(".") || x.endsWith(",")) x = x.slice(0, -1);
    if (x.length > 0) out.push(x);
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
function bareLinkable(text: string, resolves: (word: string) => boolean): ReadonlySet<string> {
  const words = lineWords(text);
  if (words.length === 0) return new Set();
  if (words.every(resolves)) return new Set(words);
  if (LS_LONG_PERMS_RE.test(words[0])) {
    const name = words[words.length - 1];
    if (resolves(name)) return new Set([name]);
  }
  return new Set();
}

// --- validation cache ---------------------------------------------------------

/** How long a validation verdict stays fresh (files appear and vanish). */
const CACHE_TTL_MS = 15_000;
const CACHE_CAP = 5000;

interface CacheEntry {
  hit: ValidatedPath | null;
  at: number;
}

/** candidate resolved per base+workspace: see [`cacheKey`]. */
const cache = new Map<string, CacheEntry>();
const inflight = new Map<string, Promise<void>>();

function cacheKey(base: string, ws: string | null, candidate: string): string {
  // Absolute and ~ candidates resolve the same under any base or workspace.
  // Relative verdicts key on the workspace too: the daemon's bare-basename
  // fallback makes the answer depend on it, not just on the base.
  const abs = candidate.startsWith("/") || candidate.startsWith("~");
  return abs ? `\u0000${candidate}` : `${base}\u0000${ws ?? ""}\u0000${candidate}`;
}

function cacheGet(base: string, ws: string | null, candidate: string): CacheEntry | undefined {
  const e = cache.get(cacheKey(base, ws, candidate));
  if (e !== undefined && Date.now() - e.at > CACHE_TTL_MS) {
    cache.delete(cacheKey(base, ws, candidate));
    return undefined;
  }
  return e;
}

function cachePut(
  base: string,
  ws: string | null,
  candidate: string,
  hit: ValidatedPath | null,
): void {
  if (cache.size >= CACHE_CAP) {
    // Drop the stalest half; simple and rare.
    const entries = [...cache.entries()].sort((a, b) => a[1].at - b[1].at);
    for (const [k] of entries.slice(0, CACHE_CAP / 2)) cache.delete(k);
  }
  cache.set(cacheKey(base, ws, candidate), { hit, at: Date.now() });
}

/** Test/HMR hook. */
export function clearLinkCache(): void {
  cache.clear();
  inflight.clear();
}

/**
 * Ensure every candidate has a fresh verdict under `base` (plus the
 * workspace's bare-basename fallback when `ws` is set): one batched
 * /fs/validate call per miss set, deduped against in-flight requests.
 * Network failures resolve to nothing cached — retried on the next pass.
 */
async function ensureValidated(
  base: string,
  ws: string | null,
  candidates: string[],
): Promise<void> {
  const missing = [...new Set(candidates)].filter(
    (c) => cacheGet(base, ws, c) === undefined && !inflight.has(cacheKey(base, ws, c)),
  );
  const waits: Promise<void>[] = [];
  for (const c of candidates) {
    const w = inflight.get(cacheKey(base, ws, c));
    if (w !== undefined) waits.push(w);
  }
  if (missing.length > 0) {
    // The server caps candidates per request; chunk to stay within it.
    for (let i = 0; i < missing.length; i += VALIDATE_MAX) {
      const chunk = missing.slice(i, i + VALIDATE_MAX);
      const p = fsValidate(chunk, base, ws)
        .then((valid) => {
          for (const c of chunk) cachePut(base, ws, c, valid[c] ?? null);
        })
        .catch(() => {
          // daemon unreachable / older daemon: leave uncached, retry later
        })
        .finally(() => {
          for (const c of chunk) inflight.delete(cacheKey(base, ws, c));
        });
      for (const c of chunk) inflight.set(cacheKey(base, ws, c), p);
      waits.push(p);
    }
  }
  await Promise.all(waits);
}

/**
 * The confirmed hit for `raw` under this context: the session cwd wins,
 * the workspace root is the fallback base for workspace-relative paths.
 */
function lookup(raw: string, ctx: LinkContext): ValidatedPath | null {
  const bases = basesFor(raw, ctx);
  for (const b of bases) {
    const e = cacheGet(b, ctx.workspaceId, raw);
    if (e?.hit != null) return e.hit;
  }
  return null;
}

/** Resolution bases to try for a candidate, in priority order. */
function basesFor(raw: string, ctx: LinkContext): string[] {
  if (raw.startsWith("/") || raw.startsWith("~")) {
    // Base is irrelevant but the API requires one.
    return [ctx.cwd ?? ctx.root ?? "/"];
  }
  if (raw.startsWith("./") || raw.startsWith("../")) {
    return ctx.cwd !== null ? [ctx.cwd] : [];
  }
  const bases: string[] = [];
  if (ctx.cwd !== null) bases.push(ctx.cwd);
  if (ctx.root !== null && ctx.root !== ctx.cwd) bases.push(ctx.root);
  return bases;
}

async function validateAll(candidates: Candidate[], ctx: LinkContext): Promise<void> {
  const byBase = new Map<string, string[]>();
  for (const c of candidates) {
    for (const b of basesFor(c.raw, ctx)) {
      const list = byBase.get(b);
      if (list === undefined) byBase.set(b, [c.raw]);
      else list.push(c.raw);
    }
  }
  await Promise.all(
    [...byBase].map(([base, raws]) => ensureValidated(base, ctx.workspaceId, raws)),
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

/**
 * Join the wrapped-line group containing 0-based buffer row `row` into one
 * string, with an exact string-index → (row, cell) mapping (wide chars and
 * combined graphemes shift string indexes; the map absorbs that).
 * Shared with the URL link provider (`urlLinks.ts`).
 */
export function groupText(
  term: Terminal,
  row: number,
): { start: number; end: number; g: GroupText } | null {
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
    // Hover path: one line group at a time, so bare directory names are worth
    // resolving here (the prefetch below never does this over whole screens).
    const candidates = extractCandidates(grp.g.text, true);
    if (candidates.length === 0) {
      callback(undefined);
      return;
    }
    const ctx = this.host.context(this.sessionId);
    void validateAll(candidates, ctx).then(() => {
      // Bare names link only on a listing / `ls -l` line, never in prose.
      const bareOk = bareLinkable(grp.g.text, (w) => lookup(w, ctx) !== null);
      const links: ILink[] = [];
      for (const c of candidates) {
        if (!qualifies(c.raw, false) && !bareOk.has(c.raw)) continue;
        const hit = lookup(c.raw, ctx);
        if (hit === null) continue;
        const endIdx = c.start + c.length - 1;
        if (endIdx >= grp.g.cellOf.length) continue;
        links.push({
          text: grp.g.text.slice(c.start, c.start + c.length),
          // Inclusive 1-based cell range (Linkifier hit-test semantics).
          range: {
            start: { x: grp.g.cellOf[c.start] + 1, y: grp.g.rowOf[c.start] + 1 },
            end: { x: grp.g.cellOf[endIdx] + 1, y: grp.g.rowOf[endIdx] + 1 },
          },
          activate: (event: MouseEvent) => {
            this.host.open(
              this.sessionId,
              hit.path,
              hit.kind,
              event.metaKey || event.ctrlKey,
            );
          },
        });
      }
      callback(links.length > 0 ? links : undefined);
    });
  }
}

const PREFETCH_DEBOUNCE_MS = 250;

/**
 * Wire path links into a pooled terminal: the link provider plus a debounced
 * viewport prefetch (fired on render, i.e. output/scroll/resize) that batch-
 * validates every candidate on screen so hover never waits on the network.
 * Returns a dispose function.
 */
export function registerPathLinks(
  term: Terminal,
  sessionId: string,
  host: LinkHost,
): () => void {
  const provider = term.registerLinkProvider(new PathLinkProvider(term, sessionId, host));

  let timer: ReturnType<typeof setTimeout> | null = null;
  const prefetch = () => {
    timer = null;
    const buf = term.buffer.active;
    const seen = new Set<number>();
    const all: Candidate[] = [];
    for (let r = 0; r < term.rows; r++) {
      const row = buf.viewportY + r;
      const grp = groupText(term, row);
      if (grp === null || seen.has(grp.start)) continue;
      seen.add(grp.start);
      all.push(...extractCandidates(grp.g.text));
    }
    if (all.length > 0) void validateAll(all, host.context(sessionId));
  };
  const schedule = () => {
    if (timer !== null) clearTimeout(timer);
    timer = setTimeout(prefetch, PREFETCH_DEBOUNCE_MS);
  };
  const render = term.onRender(schedule);
  schedule();

  return () => {
    if (timer !== null) clearTimeout(timer);
    render.dispose();
    provider.dispose();
  };
}

// --- dev-only self-checks -----------------------------------------------------
if (import.meta.env.DEV) {
  const ok = (cond: boolean, msg: string) => console.assert(cond, `links.ts self-check: ${msg}`);
  const raws = (s: string) => extractCandidates(s).map((c) => c.raw);

  ok(raws("see results/qc/report.html now").join() === "results/qc/report.html", "relative w/ slash");
  ok(raws("cat /etc/hosts").join() === "/etc/hosts", "absolute");
  ok(raws("ls ~/data and ./main.rs and ../up.txt").join() === "~/data,./main.rs,../up.txt", "~ ./ ../ forms");
  ok(raws("plain words never qualify").length === 0, "bare words don't qualify");
  ok(raws("open haiku.txt please").join() === "haiku.txt", "bare filename with extension");
  ok(raws("versions 1.2.3 and 4.8 skip").length === 0, "version numbers don't qualify");
  ok(raws("https://support.claude.com/en/a-b").length === 0, "URLs never qualify");
  ok(raws("see main.rs.").join() === "main.rs", "trailing sentence dot trims");
  ok(raws("ls -la .claude .env here").join() === ".claude,.env", "dotfolders keep their leading dot");
  ok(raws("cat .config/nvim/init.lua").join() === ".config/nvim/init.lua", "dotfolder path keeps the dot");
  ok(raws("wait... then go").length === 0, "leading ellipsis is not a candidate");
  ok(raws("--color=always -la").length === 0, "flags don't qualify");
  const withLine = extractCandidates("err at src/lib.rs:42 here")[0];
  ok(withLine.raw === "src/lib.rs" && withLine.line === 42, "line suffix parses");
  ok(withLine.length === "src/lib.rs:42".length, "underline covers the :line suffix");
  ok(raws("dir results/ listed").join() === "results/", "trailing slash survives");

  // Bare mode (hover only): single-segment names — a directory like `crates`,
  // an extensionless file like `justfile` — become candidates. The prefetch
  // never uses it, so whole screens of prose are never mass-validated.
  const bareRaws = (s: string) => extractCandidates(s, true).map((c) => c.raw);
  ok(bareRaws("cd crates").join() === "cd,crates", "bare names qualify on hover");
  ok(bareRaws("run justfile").join() === "run,justfile", "extensionless files qualify on hover");
  ok(bareRaws("bump to 1.2.3 or 4.8").join() === "bump,to,or", "bare mode still skips version numbers");
  ok(bareRaws("-la --color").length === 0, "bare mode still skips flags");
  // The contrast that keeps the daemon cheap: prose is candidate-rich on hover
  // (one line), and candidate-free in the whole-screen prefetch.
  ok(bareRaws("plain words here").length === 3, "prose words are candidates on hover");
  ok(raws("plain words here").length === 0, "…but never in the prefetch");

  // Being a candidate is not enough: a bare name only LINKS on a line shape
  // prose never has. `has(...)` stands in for "the daemon confirmed this path".
  const has =
    (...names: string[]) =>
    (w: string) =>
      names.includes(w);
  const bare = (s: string, r: (w: string) => boolean) => [...bareLinkable(s, r)].join();
  ok(
    bare("Cargo.lock  crates  target", has("Cargo.lock", "crates", "target")) ===
      "Cargo.lock,crates,target",
    "listing line: every word resolves, so bare names link",
  );
  ok(bare("update the docs now", has("docs")) === "", "prose never yields bare links");
  ok(bare("cd crates", has("crates")) === "", "a command word breaks the listing shape");
  ok(
    bare("drwxr-xr-x 5 me staff 160 Jul 7 18:04 crates", has("crates")) === "crates",
    "ls -l entry: the trailing name links",
  );
  ok(
    bare("drwxr-xr-x 5 me staff 160 Jul 7 18:04 gone", has("crates")) === "",
    "ls -l entry: an unresolved name does not link",
  );
  ok(bare("me@host chimaera % ls", has("chimaera")) === "", "prompt line yields no bare links");
  ok(bare("crates", has("crates")) === "crates", "a lone resolving name links (ls -1)");
}
