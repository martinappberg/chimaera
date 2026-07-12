/**
 * Client for the daemon's file service (M3 wave 1):
 *   GET  /fs/list?path=&hidden=      directory listing (dirs first, sorted)
 *   GET  /fs/file?path=&offset=&limit=   raw bytes + X-File-Size/X-Truncated
 *   GET  /fs/markdown?path=          server-rendered, sanitized GFM HTML
 *   GET  /fs/table?path=&offset_rows=&limit_rows=   paged CSV/TSV
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
}

export interface FileChunk {
  bytes: Uint8Array;
  /** Total file size on disk (X-File-Size). */
  size: number;
  /** True when the response stopped short of EOF (X-Truncated). */
  truncated: boolean;
  /**
   * Opaque modification token (X-Mtime; nanoseconds-since-epoch as a string).
   * Kept as a string — the value exceeds 2^53, so parsing it as a number would
   * lose precision and break the PUT conflict check. Echoed back as
   * `expect_mtime`. Null on an older daemon that omits the header.
   */
  mtime: string | null;
}

export interface TablePage {
  columns: string[];
  rows: string[][];
  offset: number;
  truncated: boolean;
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

export async function fsFile(path: string, offset = 0, limit = FILE_CHUNK): Promise<FileChunk> {
  const q = new URLSearchParams({ path, offset: String(offset), limit: String(limit) });
  const res = await api(`/fs/file?${q.toString()}`);
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
  return { bytes, size: Number.isFinite(size) ? size : bytes.length, truncated, mtime };
}

/** A concurrent-modification conflict raised by PUT /fs/file (HTTP 409). */
export class FileConflictError extends Error {
  constructor(message = "file changed on disk") {
    super(message);
    this.name = "FileConflictError";
  }
}

/**
 * Write `bytes` to `path` via PUT /fs/file. When `expectMtime` is given the
 * daemon refuses (409 → FileConflictError) if the file changed on disk since
 * that mtime — the caller offers reload/overwrite. Other failures surface as
 * ApiError (400 dir/missing-parent, 413 over the 1MB cap). Resolves to the
 * new mtime (X-Mtime on the 204) so the editor can keep tracking edits.
 */
export async function fsWrite(
  path: string,
  bytes: Uint8Array,
  expectMtime: string | null = null,
): Promise<string | null> {
  const q = new URLSearchParams({ path });
  if (expectMtime !== null) q.set("expect_mtime", expectMtime);
  // Copy into a fresh ArrayBuffer-backed view so the body is a plain
  // BodyInit (Uint8Array over SharedArrayBuffer is not).
  const body = bytes.slice();
  const res = await api(`/fs/file?${q.toString()}`, {
    method: "PUT",
    headers: { "Content-Type": "application/octet-stream" },
    body,
  });
  if (res.status === 409) throw new FileConflictError();
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
  return res.headers.get("X-Mtime");
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

/** Server cap on candidates per /fs/validate request. */
export const VALIDATE_MAX = 50;

/** Hard ceiling on candidates validated per call, across all batches — bounds
 *  the daemon round-trips a single message can trigger (VALIDATE_CAP /
 *  VALIDATE_MAX requests, currently 4). */
export const VALIDATE_CAP = 200;

/**
 * Batch existence check behind the terminal link provider, per the
 * /fs/validate contract: candidates resolve absolutely or against the
 * absolute `base`, `~` expands, and only hits come back (keyed by the
 * candidate as sent). Misses are simply absent — never errors.
 *
 * `workspaceId` (additive — older daemons ignore it) enables the daemon's
 * bare-basename fallback: a slash-less `name.ext` candidate that misses the
 * base also resolves when exactly ONE file in that workspace's index bears
 * the name ("FIGURE_PLAN.md" mentioned bare, living at paper/FIGURE_PLAN.md).
 *
 * The server caps each request at VALIDATE_MAX; callers cache the FULL sent
 * list as resolved, so anything past that cap would otherwise stick as a
 * permanent miss. Loop in VALIDATE_MAX-sized batches (bounded by VALIDATE_CAP)
 * so every candidate is actually validated. Batches run sequentially to keep
 * the daemon's concurrent load low.
 */
export async function fsValidate(
  candidates: string[],
  base: string,
  workspaceId: string | null = null,
): Promise<Record<string, ValidatedPath>> {
  const capped = candidates.slice(0, VALIDATE_CAP);
  const out: Record<string, ValidatedPath> = {};
  for (let i = 0; i < capped.length; i += VALIDATE_MAX) {
    const batch = capped.slice(i, i + VALIDATE_MAX);
    const body = await json<{ valid: Record<string, ValidatedPath> }>(
      await api("/fs/validate", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          candidates: batch,
          base,
          ...(workspaceId !== null ? { workspace_id: workspaceId } : {}),
        }),
      }),
    );
    Object.assign(out, body.valid);
  }
  return out;
}

export async function fsMarkdown(path: string): Promise<string> {
  const q = new URLSearchParams({ path });
  const body = await json<{ html: string }>(await api(`/fs/markdown?${q.toString()}`));
  return body.html;
}

export async function fsTable(path: string, offsetRows = 0, limitRows = 200): Promise<TablePage> {
  const q = new URLSearchParams({
    path,
    offset_rows: String(offsetRows),
    limit_rows: String(limitRows),
  });
  return json(await api(`/fs/table?${q.toString()}`));
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
  | "pdf"
  | "binary"
  | "text";

const IMAGE_EXTS = new Set(["png", "jpg", "jpeg", "gif", "webp", "svg"]);

/** Whether a path renders as an image (chat cards inline-preview these). */
export function isImagePath(path: string): boolean {
  return IMAGE_EXTS.has(extension(path));
}
const MARKDOWN_EXTS = new Set(["md", "markdown"]);
const HTML_EXTS = new Set(["html", "htm"]);
const TABLE_EXTS = new Set(["csv", "tsv"]);
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
  "mp3", "mp4", "m4a", "wav", "flac", "ogg", "mov", "avi", "mkv", "webm",
  "woff", "woff2", "ttf", "otf", "eot",
  "ico", "bmp", "tif", "tiff", "heic", "psd",
  "sqlite", "db", "parquet", "feather", "h5", "hdf5",
  "xlsx", "xls", "docx", "doc", "pptx", "ppt",
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
  if (ext === "pdf") return "pdf";
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
