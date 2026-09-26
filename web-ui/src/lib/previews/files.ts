/**
 * Client for the daemon's file service (M3 wave 1):
 *   GET  /fs/list?path=&hidden=      directory listing (dirs first, sorted)
 *   GET  /fs/file?path=&offset=&limit=   raw bytes + X-File-Size/X-Truncated
 *                                    (+ X-Content-Hash on a complete raw read)
 *   PUT  /fs/file?path=&expect_hash= content-hash-guarded write
 *   /fs/drafts, /fs/draft            the unsaved-edit journal mirror
 *   GET  /fs/markdown?path=          server-rendered, sanitized GFM HTML
 *   GET  /fs/table?path=&offset_rows=&limit_rows=   paged CSV/TSV/VCF/BED/GFF/SAM
 *   POST /fs/ticket {path}           short-lived unauthenticated /raw/ URL
 * plus the pure helpers that decide how a path is displayed (extension →
 * view kind, basename/parent, human sizes). Bearer auth rides on api().
 */

import { api, ApiError } from "../net/api";
import { EXT_GLYPH, GLYPHS, NAME_GLYPH, type Glyph } from "../shared/icons";

export interface FsEntry {
  name: string;
  path: string;
  kind: "dir" | "file";
  size: number;
  mtime: number;
  /** This entry is a symlink. Absent on older daemons → treat as not a link.
   *  `kind` still reflects the resolved target (a symlink-to-dir is "dir"). */
  symlink?: boolean;
  /** Raw link text (readlink), for the "→ target" hover. Present on symlinks. */
  target?: string;
  /** A symlink whose target does not resolve. `kind` is "file" (wire union),
   *  but the UI shows it distinctly and refuses to open it. */
  broken?: boolean;
}

export interface FsListing {
  path: string;
  parent: string | null;
  entries: FsEntry[];
  /** The daemon stopped at its host-safety listing ceiling. Older daemons
   *  omit this field, which is equivalent to a complete listing. */
  truncated?: boolean;
}

export interface FileChunk {
  bytes: Uint8Array;
  /** Total file size on disk (X-File-Size). */
  size: number;
  /** True when the response stopped short of EOF (X-Truncated). */
  truncated: boolean;
  /**
   * Opaque file-version token (`X-Mtime`; currently a decimal metadata hash).
   * Never parse it: echo it byte-for-byte as `expect_mtime` for the PUT
   * conflict check. Null on an older daemon that omits the header.
   */
  mtime: string | null;
  /**
   * SHA-256 (hex) of the whole file (`X-Content-Hash`). The daemon sends it
   * only when the body IS the complete raw file — offset 0, not truncated, not
   * a decompressed gzip — so a partial read can never pose as a version, and
   * only when no write raced the read (so it always names `mtime`'s version).
   * Null otherwise, and from older daemons (callers fall back to `mtime`).
   */
  hash: string | null;
}

export interface TablePage {
  columns: string[];
  rows: string[][];
  offset: number;
  truncated: boolean;
  /** Exact data-row count, once the daemon has read to the end (fs/table;
   *  absent from older daemons and from fs/xlsx). */
  total_rows?: number | null;
  /** A byte-rate row estimate while the total is unknown (plain files). */
  est_rows?: number | null;
  /** The daemon's per-request scan budget ran out before `offset`: the page
   *  is empty and asking again resumes from `scanned_to`. */
  scan_limited?: boolean;
  /** The data-row number the daemon's scan stopped at. */
  scanned_to?: number;
}

/** Server cap for one /fs/file read; also the code view's chunk size. */
export const FILE_CHUNK = 262144;

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) {
    let message = `request failed with status ${res.status}`;
    try {
      const body = (await res.json()) as { error?: string };
      if (body.error) message = body.error;
    } catch {
      // non-JSON error body; keep the generic message
    }
    throw new ApiError(res.status, message);
  }
  return (await res.json()) as T;
}

export async function fsList(path: string, hidden = false): Promise<FsListing> {
  const q = new URLSearchParams({ path });
  if (hidden) q.set("hidden", "true");
  return json(await api(`/fs/list?${q.toString()}`));
}

export async function fsFile(
  path: string,
  offset = 0,
  limit = FILE_CHUNK,
  signal?: AbortSignal,
): Promise<FileChunk> {
  const q = new URLSearchParams({ path, offset: String(offset), limit: String(limit) });
  const res = await api(`/fs/file?${q.toString()}`, signal !== undefined ? { signal } : {});
  if (!res.ok) {
    let message = `request failed with status ${res.status}`;
    try {
      const body = (await res.json()) as { error?: string };
      if (body.error) message = body.error;
    } catch {
      // raw endpoint; error bodies may not be JSON
    }
    throw new ApiError(res.status, message);
  }
  const bytes = new Uint8Array(await res.arrayBuffer());
  const size = Number(res.headers.get("X-File-Size") ?? bytes.length);
  const truncated = res.headers.get("X-Truncated") === "true";
  const mtime = res.headers.get("X-Mtime");
  const hash = res.headers.get("X-Content-Hash");
  return { bytes, size: Number.isFinite(size) ? size : bytes.length, truncated, mtime, hash };
}

/** A concurrent-modification conflict raised by PUT /fs/file (HTTP 409). */
export class FileConflictError extends Error {
  /** The current disk version, when the daemon reports it (409 headers). */
  readonly hash: string | null;
  readonly mtime: string | null;
  constructor(
    message = "file changed on disk",
    hash: string | null = null,
    mtime: string | null = null,
  ) {
    super(message);
    this.name = "FileConflictError";
    this.hash = hash;
    this.mtime = mtime;
  }
}

export interface WriteOptions {
  /** Refuse (409) unless the file on disk hashes to this — the preferred,
   *  content-verified precondition. */
  expectHash?: string | null;
  /** Metadata precondition, for a daemon that never sent a content hash. */
  expectMtime?: string | null;
  signal?: AbortSignal;
}

export interface WriteResult {
  mtime: string | null;
  /** SHA-256 of what is now on disk; null from older daemons. */
  hash: string | null;
}

/**
 * Write `bytes` to `path` via PUT /fs/file. With a precondition the daemon
 * refuses (409 → FileConflictError) when the disk moved on. Only one is sent,
 * the hash when known: a daemon new enough to report hashes checks them, and
 * one that already holds exactly these bytes answers success without writing,
 * so a retry after a lost reply is safe. Other failures surface as ApiError
 * (400 dir/missing-parent, 413 over the 1MB cap); a dead link or an abort
 * rejects with the fetch's own error.
 */
export async function fsWrite(
  path: string,
  bytes: Uint8Array,
  opts: WriteOptions = {},
): Promise<WriteResult> {
  const q = new URLSearchParams({ path });
  if (opts.expectHash != null) q.set("expect_hash", opts.expectHash);
  else if (opts.expectMtime != null) q.set("expect_mtime", opts.expectMtime);
  // Copy into a fresh ArrayBuffer-backed view so the body is a plain
  // BodyInit (Uint8Array over SharedArrayBuffer is not).
  const body = bytes.slice();
  const res = await api(`/fs/file?${q.toString()}`, {
    method: "PUT",
    headers: { "Content-Type": "application/octet-stream" },
    body,
    ...(opts.signal !== undefined ? { signal: opts.signal } : {}),
  });
  if (res.status === 409) {
    throw new FileConflictError(
      "file changed on disk",
      res.headers.get("X-Content-Hash"),
      res.headers.get("X-Mtime"),
    );
  }
  if (!res.ok) {
    let message = `save failed with status ${res.status}`;
    try {
      const errBody = (await res.json()) as { error?: string };
      if (errBody.error) message = errBody.error;
    } catch {
      // non-JSON error body; keep the generic message
    }
    throw new ApiError(res.status, message);
  }
  return { mtime: res.headers.get("X-Mtime"), hash: res.headers.get("X-Content-Hash") };
}

// --- the unsaved-edit journal's daemon mirror -------------------------------
// A new tunnel port is a new browser origin with an empty IndexedDB, so dirty
// text is mirrored to the daemon (~/.chimaera/drafts). Older daemons lack the
// routes: every helper degrades to "unsupported"/null rather than an error.

export interface DraftBody {
  path: string;
  base_hash: string;
  text: string;
  /** When the daemon stored it, by the DAEMON's clock. */
  updated_ms: number;
  /** When the writer's text last changed, by the WRITER's clock (what it sent
   *  as `updated_ms`); null from an older writer or daemon. */
  client_updated_ms: number | null;
}

export type DraftPutResult = "ok" | "too-large" | "unsupported";

/** Statuses meaning "this daemon has no drafts route". */
function draftsUnsupported(status: number): boolean {
  return status === 404 || status === 405 || status === 501;
}

/** The JSON body of a draft PUT (drafts.ts weighs it against the browser's
 *  keepalive quota before asking for `keepalive`). */
export function draftPutBody(path: string, baseHash: string, text: string, updatedMs: number): string {
  return JSON.stringify({ path, base_hash: baseHash, text, updated_ms: updatedMs });
}

/** Mirror a draft; `updatedMs` is this client's time for the text (an older
 *  daemon ignores it). `keepalive` lets a small body outlive a closing page. */
export async function fsDraftPut(
  path: string,
  baseHash: string,
  text: string,
  updatedMs: number,
  keepalive = false,
): Promise<DraftPutResult> {
  const res = await api("/fs/drafts", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: draftPutBody(path, baseHash, text, updatedMs),
    keepalive,
    signal: AbortSignal.timeout(15_000),
  });
  if (res.ok) return "ok";
  if (res.status === 413) return "too-large";
  if (draftsUnsupported(res.status)) return "unsupported";
  throw new ApiError(res.status, `draft mirror failed with status ${res.status}`);
}

/** The daemon's draft for `path`, or null (none, or an older daemon). */
export async function fsDraftGet(path: string): Promise<DraftBody | null> {
  const q = new URLSearchParams({ path });
  const res = await api(`/fs/draft?${q.toString()}`, { signal: AbortSignal.timeout(10_000) });
  if (!res.ok) {
    if (draftsUnsupported(res.status) || res.status === 400) return null;
    throw new ApiError(res.status, `draft read failed with status ${res.status}`);
  }
  try {
    const body = (await res.json()) as Partial<DraftBody>;
    if (typeof body.text !== "string" || typeof body.path !== "string") return null;
    return {
      path: body.path,
      base_hash: typeof body.base_hash === "string" ? body.base_hash : "",
      text: body.text,
      updated_ms: typeof body.updated_ms === "number" ? body.updated_ms : 0,
      client_updated_ms: typeof body.client_updated_ms === "number" ? body.client_updated_ms : null,
    };
  } catch {
    return null; // not JSON (an older daemon's fallback), so not a draft
  }
}

export interface DraftSummary {
  path: string;
  base_hash: string;
  updated_ms: number;
  client_updated_ms?: number | null;
  bytes: number;
}

/**
 * Every mirrored draft (summaries only), or null from a daemon without the
 * route. Cheaper to consult on open than fetching one draft, which answers a
 * plain "none" with a 404 the browser logs as a failed resource.
 */
export async function fsDraftList(): Promise<DraftSummary[] | null> {
  const res = await api("/fs/drafts", { signal: AbortSignal.timeout(10_000) });
  if (!res.ok) {
    if (draftsUnsupported(res.status)) return null;
    throw new ApiError(res.status, `draft list failed with status ${res.status}`);
  }
  try {
    const body = (await res.json()) as { drafts?: DraftSummary[] };
    return Array.isArray(body.drafts) ? body.drafts : null;
  } catch {
    return null; // not JSON (an older daemon's fallback)
  }
}

/** Drop the daemon's draft for `path` (a no-op on an older daemon). */
export async function fsDraftDelete(path: string, keepalive = false): Promise<void> {
  const q = new URLSearchParams({ path });
  const res = await api(`/fs/draft?${q.toString()}`, {
    method: "DELETE",
    keepalive,
    signal: AbortSignal.timeout(10_000),
  });
  if (!res.ok && !draftsUnsupported(res.status)) {
    throw new ApiError(res.status, `draft delete failed with status ${res.status}`);
  }
}

export interface QuickOpenEntry {
  /** Absolute path on the daemon's filesystem. */
  path: string;
  /** Workspace-relative path (what the palette matches and shows). */
  rel: string;
  name: string;
  mtime: number;
  /** Absent on older daemons — treat as "file". */
  kind?: "file" | "dir";
}

/**
 * Fuzzy file index for the quick-open palette. The daemon walks the workspace
 * root (ignoring .git/node_modules/target/…), subsequence-matches `q` against
 * the relative path, and returns up to `limit` ranked entries. An empty `q`
 * returns the most-recently-modified files. `dirs` admits directories too
 * (chat @-mentions tag folders; the Cmd+P palette stays files-only).
 */
export async function fsQuickOpen(
  workspaceId: string,
  q: string,
  limit = 50,
  dirs = false,
): Promise<QuickOpenEntry[]> {
  const params = new URLSearchParams({ workspace_id: workspaceId, q, limit: String(limit) });
  if (dirs) params.set("dirs", "true");
  const body = await json<{ entries: QuickOpenEntry[] }>(
    await api(`/fs/quickopen?${params.toString()}`),
  );
  return body.entries;
}

/** One confirmed path from POST /fs/validate. */
export interface ValidatedPath {
  /** Canonical absolute path on the daemon. */
  path: string;
  kind: "file" | "dir";
}

/** A /fs/validate answer, merged across batches. */
export interface ValidateResult {
  /** Candidate (as sent) → its one resolution. */
  valid: Record<string, ValidatedPath>;
  /** Candidate → the (at most five) files it could mean, when the
   *  workspace-index fallback found several. Empty from older daemons. */
  ambiguous: Record<string, ValidatedPath[]>;
  /** Candidates that were never answered: past VALIDATE_CAP, or in a batch
   *  that failed after an earlier one succeeded. Unknown, NOT misses —
   *  callers must not cache them as such. */
  unchecked: string[];
}

/** Server cap on candidates per /fs/validate request. */
export const VALIDATE_MAX = 50;

/** Hard ceiling on candidates validated per call, across all batches — bounds
 *  the daemon round-trips a single message can trigger (VALIDATE_CAP /
 *  VALIDATE_MAX requests, currently 4). */
export const VALIDATE_CAP = 200;

/** Server cap on extra `bases` per request. */
export const VALIDATE_BASES_MAX = 8;

/** Server cap on one candidate's length, in UTF-8 bytes. */
const CANDIDATE_MAX_BYTES = 1024;

/**
 * Batch existence check behind every path link (terminal, chat, tool
 * cards), per the /fs/validate contract. Each candidate must be a clean path
 * (no `:12` suffix, `#L` anchor or wrapper — `shared/fileRef.ts` makes
 * them). The daemon's ladder: absolute / `~` → `base`, then each of `bases`
 * in order → the same without a leading `a/` / `b/` diff prefix → with
 * `workspaceId`, a unique basename or unique path suffix in that
 * workspace's index (`figs/plot.png` → `results/figs/plot.png`), or up to
 * five `ambiguous` matches. A miss is simply absent, never an error.
 * `strict` (document links) keeps only the exact join onto each base — no
 * prefix strip, no index fallback — so a broken `b/spec.md` link stays
 * broken. `workspaceId`, `bases` and `strict` are additive: older daemons
 * ignore them.
 *
 * The server caps each request at VALIDATE_MAX; loop in VALIDATE_MAX-sized
 * batches (bounded by VALIDATE_CAP), sequentially to keep the daemon's
 * concurrent load low. A candidate over the byte cap can never validate
 * and is dropped (a miss). Throws only when nothing was answered.
 */
export async function fsValidate(
  candidates: string[],
  base: string,
  workspaceId: string | null = null,
  bases: string[] = [],
  opts: { strict?: boolean } = {},
): Promise<ValidateResult> {
  const out: ValidateResult = { valid: {}, ambiguous: {}, unchecked: [] };
  const sendable = [...new Set(candidates)].filter(
    (c) => c.length > 0 && new TextEncoder().encode(c).length <= CANDIDATE_MAX_BYTES,
  );
  const capped = sendable.slice(0, VALIDATE_CAP);
  out.unchecked.push(...sendable.slice(VALIDATE_CAP));
  const extra = [...new Set(bases)].filter((b) => b !== base).slice(0, VALIDATE_BASES_MAX);
  for (let i = 0; i < capped.length; i += VALIDATE_MAX) {
    const batch = capped.slice(i, i + VALIDATE_MAX);
    type Body = { valid?: Record<string, ValidatedPath>; ambiguous?: Record<string, ValidatedPath[]> };
    let body: Body;
    try {
      body = await json<Body>(
        await api("/fs/validate", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            candidates: batch,
            base,
            ...(workspaceId !== null ? { workspace_id: workspaceId } : {}),
            ...(extra.length > 0 ? { bases: extra } : {}),
            ...(opts.strict === true ? { strict: true } : {}),
          }),
        }),
      );
    } catch (err) {
      if (i === 0) throw err;
      out.unchecked.push(...capped.slice(i));
      break;
    }
    Object.assign(out.valid, body.valid ?? {});
    for (const [cand, matches] of Object.entries(body.ambiguous ?? {})) {
      if (Array.isArray(matches) && matches.length > 0) out.ambiguous[cand] = matches;
    }
  }
  return out;
}

/** A server-rendered markdown document (GET /fs/markdown). */
export interface MarkdownDoc {
  /** Sanitized comrak HTML; block elements carry `data-sourcepos`. */
  html: string;
  /** Raw YAML text of a leading `---` block (no longer part of `html`), or
   *  null. Older daemons omit the field and render the block into `html`. */
  frontmatter: string | null;
}

export async function fsMarkdown(path: string): Promise<MarkdownDoc> {
  const q = new URLSearchParams({ path });
  const body = await json<{ html: string; frontmatter?: string | null }>(
    await api(`/fs/markdown?${q.toString()}`),
  );
  return {
    html: body.html,
    frontmatter: typeof body.frontmatter === "string" ? body.frontmatter : null,
  };
}

/**
 * How fs/table reads a bioinformatics text format: which lines are comments,
 * whether a header row exists (else the format's standard column names), and
 * that fields are never quoted (a SAM quality or VCF text may open with `"`).
 */
export interface TablePreset {
  /** Line prefixes the daemon skips wherever they appear. */
  comment: string[];
  /** The file carries its own header row (after the comments). */
  header: boolean;
  /** Column names for a header-less file; further fields are `colN`. */
  names?: string[];
  /** Strip a leading `#` from the first header cell (VCF's `#CHROM`). */
  stripHash?: boolean;
}

const BED_NAMES = ["chrom", "chromStart", "chromEnd", "name", "score", "strand"];
const PEAK_NAMES = [...BED_NAMES, "signalValue", "pValue", "qValue"];
const BED_COMMENTS = ["#", "track", "browser"];
const GFF: TablePreset = {
  comment: ["#"],
  header: false,
  names: ["seqid", "source", "type", "start", "end", "score", "strand", "phase", "attributes"],
};

const TABLE_PRESETS: Record<string, TablePreset> = {
  vcf: { comment: ["##"], header: true, stripHash: true },
  bed: {
    comment: BED_COMMENTS,
    header: false,
    names: [...BED_NAMES, "thickStart", "thickEnd", "itemRgb", "blockCount", "blockSizes", "blockStarts"],
  },
  bedgraph: { comment: BED_COMMENTS, header: false, names: ["chrom", "chromStart", "chromEnd", "dataValue"] },
  narrowpeak: { comment: BED_COMMENTS, header: false, names: [...PEAK_NAMES, "peak"] },
  broadpeak: { comment: BED_COMMENTS, header: false, names: PEAK_NAMES },
  gff: GFF,
  gff3: GFF,
  gtf: {
    comment: ["#"],
    header: false,
    names: ["seqname", "source", "feature", "start", "end", "score", "strand", "frame", "attribute"],
  },
  sam: {
    comment: ["@"],
    header: false,
    names: ["QNAME", "FLAG", "RNAME", "POS", "MAPQ", "CIGAR", "RNEXT", "PNEXT", "TLEN", "SEQ", "QUAL"],
  },
};

/** The bioinformatics preset for `path` (gzip wrappers by their inner
 *  extension), or null for plain CSV/TSV. */
export function tablePreset(path: string): TablePreset | null {
  return TABLE_PRESETS[innerExtension(path)] ?? null;
}

/** The fs/table query for one page of `path`, preset options included. */
export function tableQuery(path: string, offsetRows: number, limitRows: number): URLSearchParams {
  const q = new URLSearchParams({
    path,
    offset_rows: String(offsetRows),
    limit_rows: String(limitRows),
  });
  const preset = tablePreset(path);
  if (preset !== null) {
    // Every preset format is tab-separated; the daemon's name sniff knows
    // only .csv/.tsv.
    q.set("delim", "tab");
    q.set("quote", "false");
    q.set("comment", preset.comment.join(","));
    if (!preset.header) {
      q.set("header", "false");
      if (preset.names !== undefined) q.set("names", preset.names.join(","));
    }
  }
  return q;
}

/** A page's column names as the grid shows them (VCF's `#CHROM` → `CHROM`). */
export function presetColumns(path: string, columns: string[]): string[] {
  if (tablePreset(path)?.stripHash !== true || !columns[0]?.startsWith("#")) return columns;
  return [columns[0].slice(1), ...columns.slice(1)];
}

export async function fsTable(path: string, offsetRows = 0, limitRows = 200): Promise<TablePage> {
  const page = await json<TablePage>(
    await api(`/fs/table?${tableQuery(path, offsetRows, limitRows).toString()}`),
  );
  return { ...page, columns: presetColumns(path, page.columns) };
}

/** One page of a spreadsheet sheet: the CSV `TablePage` shape plus the
 *  workbook's sheet names and which one this page is from. */
export interface XlsxPage extends TablePage {
  sheets: string[];
  sheet: string;
}

/** A page of a spreadsheet (xlsx/xls/xlsm/ods). `sheet` null = the first sheet. */
export async function fsXlsx(
  path: string,
  sheet: string | null,
  offsetRows = 0,
  limitRows = 200,
): Promise<XlsxPage> {
  const q = new URLSearchParams({
    path,
    offset_rows: String(offsetRows),
    limit_rows: String(limitRows),
  });
  if (sheet !== null) q.set("sheet", sheet);
  return json(await api(`/fs/xlsx?${q.toString()}`));
}

/**
 * Mint a single-path ticket and return the unauthenticated /raw/ URL for it
 * (iframes and <img> cannot send Authorization headers; the bearer token must
 * never appear in such a URL). Tickets expire server-side after ~10 minutes.
 */
export async function fsRawUrl(path: string): Promise<string> {
  const body = await json<{ ticket: string }>(
    await api("/fs/ticket", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path }),
    }),
  );
  return `/raw/${body.ticket}`;
}

/**
 * Per-path `/raw/` URL memo (write-once artifacts). A ticket is valid ~10 min;
 * re-minting one on every mount — as the chat artifact cards did — spends a
 * round-trip AND changes the `<img>` src, forcing the browser to re-fetch and
 * re-decode an image it already has (the flash). Memoizing the URL per path
 * keeps the src STABLE across re-renders/remounts, so a cached image just shows.
 * Safe for write-once outputs (plots, result tables) whose bytes don't change;
 * live-updating previews mint their own fresh tickets on change (see fileStore).
 */
const ticketUrls = new Map<string, { url: string; at: number }>();
const TICKET_MEMO_MS = 8 * 60 * 1000;

export async function rawTicketUrl(path: string): Promise<string> {
  const hit = ticketUrls.get(path);
  if (hit !== undefined && Date.now() - hit.at < TICKET_MEMO_MS) return hit.url;
  const url = await fsRawUrl(path);
  ticketUrls.set(path, { url, at: Date.now() });
  return url;
}

/**
 * Create an empty file or directory (POST /fs/create), making any missing
 * parents — the inline "new file" input accepts nested a/b/c.txt names.
 * 409 (already exists) surfaces as ApiError with the server's message.
 * Resolves to the canonical created path.
 */
export async function fsCreate(path: string, kind: "file" | "dir"): Promise<string> {
  const body = await json<{ path: string }>(
    await api("/fs/create", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path, kind }),
    }),
  );
  return body.path;
}

/**
 * Rename/move a file or directory (POST /fs/rename). `to` is the full new
 * path; an existing target is a 409 ApiError. Resolves to the canonical new
 * path. Prefer fsRenameOp (workspace/fsEvents) so open surfaces refresh.
 */
export async function fsRename(from: string, to: string): Promise<string> {
  const body = await json<{ path: string }>(
    await api("/fs/rename", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ from, to }),
    }),
  );
  return body.path;
}

/**
 * Copy a file/dir/symlink (POST /fs/copy). `to` is the full destination path.
 * `unique` picks a free "name copy" sibling instead of a 409 on collision.
 * Resolves to the canonical new path. Prefer fsCopyOp (workspace/fsEvents) so
 * open surfaces refresh.
 */
export async function fsCopy(
  from: string,
  to: string,
  onConflict: "fail" | "unique" = "fail",
): Promise<string> {
  const body = await json<{ path: string }>(
    await api("/fs/copy", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ from, to, on_conflict: onConflict }),
    }),
  );
  return body.path;
}

/**
 * Move a file/dir/symlink (POST /fs/move). `to` is the full destination path;
 * an existing target is a 409. Resolves to the canonical new path. Prefer
 * fsMoveOp (workspace/fsEvents) so open surfaces refresh + tabs follow.
 */
export async function fsMove(from: string, to: string): Promise<string> {
  const body = await json<{ path: string }>(
    await api("/fs/move", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ from, to }),
    }),
  );
  return body.path;
}

/**
 * Permanently delete a file or directory (POST /fs/delete; recursive, no
 * trash). The UI fronts this with an explicit confirmation. Prefer
 * fsDeleteOp (workspace/fsEvents) so open surfaces refresh.
 */
export async function fsDelete(path: string): Promise<void> {
  const res = await api("/fs/delete", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ path }),
  });
  if (!res.ok) {
    let message = `delete failed with status ${res.status}`;
    try {
      const body = (await res.json()) as { error?: string };
      if (body.error) message = body.error;
    } catch {
      // non-JSON error body; keep the generic message
    }
    throw new ApiError(res.status, message);
  }
}

/**
 * Download `path` (file or folder) as a browser download: mint a ticket,
 * then navigate a transient anchor at /download/{ticket}. The server's
 * Content-Disposition names the file (folders arrive as <name>.zip); an
 * attachment response never navigates the SPA. Works identically against a
 * remote daemon — the window's origin IS the ssh tunnel.
 */
export async function fsDownload(path: string): Promise<void> {
  const body = await json<{ ticket: string }>(
    await api("/fs/ticket", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path }),
    }),
  );
  const a = document.createElement("a");
  a.href = `/download/${body.ticket}`;
  a.rel = "noopener";
  // The `download` attribute forces a download even for a showable MIME (a
  // .md is text/*): without it the Tauri WKWebView navigates the main webview
  // to the raw file body (the "opens in the native window" bug) because its
  // response policy keys on canShowMIMEType, never on Content-Disposition.
  // Same-origin, so the server's Content-Disposition filename still wins.
  a.download = "";
  document.body.appendChild(a);
  a.click();
  a.remove();
}

/**
 * Cheap existence probe for restore-time pruning. Only a definitive server
 * "no such file" (400/404) counts as dead — an unreachable daemon or an
 * older daemon without the endpoint (405) must never wipe tabs.
 */
export async function fsProbe(path: string): Promise<"ok" | "dead" | "unknown"> {
  try {
    const q = new URLSearchParams({ path, offset: "0", limit: "1" });
    const res = await api(`/fs/file?${q.toString()}`);
    if (res.ok) return "ok";
    return res.status === 400 || res.status === 404 ? "dead" : "unknown";
  } catch {
    return "unknown";
  }
}

// --- pure path/display helpers ----------------------------------------------

export function basename(path: string): string {
  const trimmed = path.endsWith("/") ? path.slice(0, -1) : path;
  const i = trimmed.lastIndexOf("/");
  return i >= 0 ? trimmed.slice(i + 1) : trimmed;
}

/** A directory as a drop-destination label: `src/`, and `/` for the root
 *  (never `//`). One helper so every "upload into …" chip agrees. */
export function dirLabel(path: string): string {
  const name = basename(path);
  return name === "" ? "/" : `${name}/`;
}

/**
 * Middle-ellipsis truncation (polish inventory: paths truncate in the
 * middle — the basename is the informative end).
 */
export function midTruncate(s: string, max: number): string {
  if (s.length <= max || max < 5) return s;
  const tail = Math.floor((max - 1) / 2);
  const head = max - 1 - tail;
  return `${s.slice(0, head)}…${s.slice(s.length - tail)}`;
}

/** Name of the containing directory ("/" for root-level paths). */
export function parentName(path: string): string {
  const i = path.lastIndexOf("/");
  if (i <= 0) return "/";
  return basename(path.slice(0, i));
}

/** Absolute path of the containing directory ("/" at the top). */
export function dirname(path: string): string {
  const trimmed = path.endsWith("/") ? path.slice(0, -1) : path;
  const i = trimmed.lastIndexOf("/");
  return i > 0 ? trimmed.slice(0, i) : "/";
}

/** Join a directory and a leaf into an absolute path. */
export function joinPath(dir: string, leaf: string): string {
  return dir.endsWith("/") ? `${dir}${leaf}` : `${dir}/${leaf}`;
}

/**
 * Resolve a document-relative reference (`figs/plot.png`, `../notes.md`)
 * against the directory of `docPath` into an absolute-path candidate, folding
 * `.`/`..` segments. Only builds the string — the server still canonicalizes
 * and enforces access. Shared by the markdown reading view's image stamping
 * and the live preview's image widgets.
 */
/** `decodeURI` that returns the raw string on malformed input — the server
 *  rejects what it can't canonicalize, so a broken escape isn't fatal here. */
export function safeDecodeUri(s: string): string {
  try {
    return decodeURI(s);
  } catch {
    return s;
  }
}

export function resolveDocPath(docPath: string, rel: string): string {
  const i = docPath.lastIndexOf("/");
  const base = i <= 0 ? "" : docPath.slice(0, i);
  const parts = (rel.startsWith("/") ? rel : `${base}/${rel}`).split("/");
  const out: string[] = [];
  for (const seg of parts) {
    if (seg === "" || seg === ".") continue;
    if (seg === "..") out.pop();
    else out.push(seg);
  }
  return `/${out.join("/")}`;
}

export function extension(path: string): string {
  const name = basename(path).toLowerCase();
  const i = name.lastIndexOf(".");
  return i > 0 ? name.slice(i + 1) : "";
}

/** Gzip wrappers the server decompresses transparently (fs/table, fs/file). */
const GZIP_EXTS = new Set(["gz", "bgz"]);

/** True when the path is a server-decompressed gzip member. */
export function isGzipped(path: string): boolean {
  return GZIP_EXTS.has(extension(path));
}

/**
 * The "effective" extension used for view-kind and icon decisions: for a
 * gzip wrapper (foo.tsv.gz → tsv) the inner extension is sniffed, matching
 * the server's own inner-name sniff. A bare `foo.gz` stays "gz".
 */
export function innerExtension(path: string): string {
  const ext = extension(path);
  if (!GZIP_EXTS.has(ext)) return ext;
  const stem = basename(path).toLowerCase().slice(0, -(ext.length + 1));
  const i = stem.lastIndexOf(".");
  return i > 0 ? stem.slice(i + 1) : ext;
}

export type FileViewKind =
  | "image"
  | "markdown"
  | "html"
  | "table"
  | "xlsx"
  | "pdf"
  | "video"
  | "audio"
  | "binary"
  | "text";

/** Formats every supported webview decodes natively (WebKit included). */
const IMAGE_EXTS = new Set(["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico", "avif"]);
/** Played by the native <video>/<audio> element over ranged /raw reads. The
 *  container is no promise of a codec (an HEVC .mov, a Vorbis .ogg on
 *  WebKit): MediaView owns the "can't play this here" fallback. */
const VIDEO_EXTS = new Set(["mp4", "webm", "m4v", "ogv", "mov"]);
const AUDIO_EXTS = new Set(["mp3", "wav", "m4a", "flac", "ogg", "oga", "opus", "aac"]);

/** Whether a path renders as an image (chat cards inline-preview these). */
export function isImagePath(path: string): boolean {
  return IMAGE_EXTS.has(extension(path));
}
const MARKDOWN_EXTS = new Set(["md", "markdown"]);
const HTML_EXTS = new Set(["html", "htm"]);
/** Paged by fs/table: delimited text, including the bioinformatics formats
 *  `tablePreset` knows how to read. */
const TABLE_EXTS = new Set([
  "csv", "tsv",
  "vcf", "bed", "bedgraph", "narrowpeak", "broadpeak", "gff", "gff3", "gtf", "sam",
]);
/** Spreadsheets parsed server-side (calamine) into the same paged table grid. */
const SPREADSHEET_EXTS = new Set(["xlsx", "xls", "xlsm", "ods"]);
/**
 * Extensions we know are binary up front — straight to the info card, no
 * fetch. Everything not listed anywhere goes down the text path, which
 * still sniffs the first bytes and falls back to the card (that catches
 * .bam, extensionless binaries, and the long tail). Gzip wrappers are NOT
 * listed here: their inner extension is sniffed first (foo.tsv.gz → table).
 */
const BINARY_EXTS = new Set([
  "zip", "tar", "7z", "rar", "xz", "zst", "bz2",
  "exe", "dll", "so", "dylib", "o", "a", "class", "jar", "pyc", "wasm",
  "iso", "dmg",
  "avi", "mkv",
  "woff", "woff2", "ttf", "otf", "eot",
  "tif", "tiff", "heic", "psd",
  "sqlite", "db", "parquet", "feather", "h5", "hdf5",
  "docx", "doc", "pptx", "ppt",
  "bam", "bai", "cram", "crai", "bcf", "csi", "tbi", "bigwig", "bw", "bigbed", "bb",
]);

/**
 * How FileView renders `path`, decided from the extension. Gzip wrappers
 * resolve by their inner extension (foo.tsv.gz renders as a table, foo.gz of
 * an unknown inner type falls through to the text/sniff path) — the server's
 * fs/table and fs/file decompress transparently.
 */
export function viewKindFor(path: string): FileViewKind {
  const ext = innerExtension(path);
  // Only fs/table and fs/file decompress gzip; the /raw/ (image/pdf/html) and
  // fs/markdown paths do not. A gzipped tabular file previews as a table;
  // every other gzip goes down the text path (fs/file decompresses, then the
  // NUL sniff falls back to the binary card for gzipped binaries).
  if (isGzipped(path)) return TABLE_EXTS.has(ext) ? "table" : "text";
  if (IMAGE_EXTS.has(ext)) return "image";
  if (MARKDOWN_EXTS.has(ext)) return "markdown";
  if (HTML_EXTS.has(ext)) return "html";
  if (TABLE_EXTS.has(ext)) return "table";
  if (SPREADSHEET_EXTS.has(ext)) return "xlsx";
  if (ext === "pdf") return "pdf";
  if (VIDEO_EXTS.has(ext)) return "video";
  if (AUDIO_EXTS.has(ext)) return "audio";
  if (BINARY_EXTS.has(ext)) return "binary";
  return "text";
}

/** View kinds the chat renders inline under tool cards (images, tabular
 *  data, PDFs — the "job output" formats worth seeing without a click). */
const INLINE_PREVIEW_KINDS = new Set<FileViewKind>(["image", "table", "pdf"]);

/** True when the chat can inline-preview this path's kind. */
export function canInlinePreview(path: string): boolean {
  return INLINE_PREVIEW_KINDS.has(viewKindFor(path));
}

/** Largest file the daemon accepts for an in-place edit (PUT /fs/file). */
export const EDIT_MAX_BYTES = 1024 * 1024;

/**
 * The vendored file-type glyph for a path (tree, tabs, pane bars, quick-open),
 * resolved by exact filename first (Dockerfile, LICENSE, .gitignore, lockfiles)
 * then by extension. Gzip wrappers resolve by their inner extension, matching
 * the server's inner-name sniff (foo.tsv.gz → the table glyph). Unknown types
 * fall back to a quiet generic-file glyph.
 */
export function iconFor(path: string): Glyph | null {
  const name = basename(path).toLowerCase();
  const byName = NAME_GLYPH[name];
  if (byName !== undefined) return GLYPHS[byName] ?? null;
  const ext = innerExtension(path);
  const byExt = EXT_GLYPH[ext];
  return byExt !== undefined ? (GLYPHS[byExt] ?? null) : null;
}

/** True when the first bytes look like binary data (NUL sniff, first 8KB). */
export function looksBinary(bytes: Uint8Array): boolean {
  const n = Math.min(bytes.length, 8192);
  for (let i = 0; i < n; i++) {
    if (bytes[i] === 0) return true;
  }
  return false;
}

export function humanSize(n: number): string {
  if (!Number.isFinite(n) || n < 0) return "—";
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n;
  let u = -1;
  do {
    v /= 1024;
    u += 1;
  } while (v >= 1024 && u < units.length - 1);
  return `${v >= 100 ? Math.round(v) : v.toFixed(1)} ${units[u]}`;
}

/** Compact local timestamp for mtimes (epoch seconds). */
export function formatMtime(mtime: number): string {
  if (!Number.isFinite(mtime) || mtime <= 0) return "—";
  const d = new Date(mtime * 1000);
  const pad = (x: number) => String(x).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/**
 * Tab titles for the open file tabs: basename, disambiguated by the parent
 * directory when two open files share a basename ("api.ts · lib").
 */
export function fileTabTitles(paths: readonly string[]): Map<string, string> {
  const byBase = new Map<string, string[]>();
  for (const p of paths) {
    const base = basename(p);
    const list = byBase.get(base);
    if (list === undefined) byBase.set(base, [p]);
    else list.push(p);
  }
  const titles = new Map<string, string>();
  for (const [base, ps] of byBase) {
    if (ps.length === 1) {
      titles.set(ps[0], base);
    } else {
      for (const p of ps) titles.set(p, `${base} · ${parentName(p)}`);
    }
  }
  return titles;
}
