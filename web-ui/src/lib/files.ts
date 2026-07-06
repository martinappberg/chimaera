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

import { api, ApiError } from "./api";

export interface FsEntry {
  name: string;
  path: string;
  kind: "dir" | "file";
  size: number;
  mtime: number;
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
  return { bytes, size: Number.isFinite(size) ? size : bytes.length, truncated };
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

/** Name of the containing directory ("/" for root-level paths). */
export function parentName(path: string): string {
  const i = path.lastIndexOf("/");
  if (i <= 0) return "/";
  return basename(path.slice(0, i));
}

export function extension(path: string): string {
  const name = basename(path).toLowerCase();
  const i = name.lastIndexOf(".");
  return i > 0 ? name.slice(i + 1) : "";
}

export type FileViewKind = "image" | "markdown" | "html" | "table" | "binary" | "text";

const IMAGE_EXTS = new Set(["png", "jpg", "jpeg", "gif", "webp", "svg"]);
const MARKDOWN_EXTS = new Set(["md", "markdown"]);
const HTML_EXTS = new Set(["html", "htm"]);
const TABLE_EXTS = new Set(["csv", "tsv"]);
/**
 * Extensions we know are binary up front — straight to the info card, no
 * fetch. Everything not listed anywhere goes down the text path, which
 * still sniffs the first bytes and falls back to the card (that catches
 * .gz, .bam, extensionless binaries, and the long tail).
 */
const BINARY_EXTS = new Set([
  "pdf", "zip", "tar", "7z", "rar", "xz", "zst", "bz2",
  "exe", "dll", "so", "dylib", "o", "a", "class", "jar", "pyc", "wasm",
  "iso", "dmg",
  "mp3", "mp4", "m4a", "wav", "flac", "ogg", "mov", "avi", "mkv", "webm",
  "woff", "woff2", "ttf", "otf", "eot",
  "ico", "bmp", "tif", "tiff", "heic", "psd",
  "sqlite", "db", "parquet", "feather", "h5", "hdf5",
  "xlsx", "xls", "docx", "doc", "pptx", "ppt",
  "bam", "bai", "cram", "crai", "bcf", "csi", "tbi", "bigwig", "bw", "bigbed", "bb",
]);

/** How FileView renders `path`, decided purely from the extension. */
export function viewKindFor(path: string): FileViewKind {
  const ext = extension(path);
  if (IMAGE_EXTS.has(ext)) return "image";
  if (MARKDOWN_EXTS.has(ext)) return "markdown";
  if (HTML_EXTS.has(ext)) return "html";
  if (TABLE_EXTS.has(ext)) return "table";
  if (BINARY_EXTS.has(ext)) return "binary";
  return "text";
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
