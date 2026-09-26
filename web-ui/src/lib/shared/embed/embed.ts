/**
 * Embed cards: the `POST /fs/resolve_targets` client, and the pure pieces
 * every card shares — which body a file gets, the `/raw` URLs a card loads,
 * the size hint in an alt text, and the crop a region fragment draws.
 *
 * One round trip per document: a renderer (the markdown views, chat prose)
 * hands every target it holds to `resolveTargets` at once and passes each
 * card its answer as `info`. A card mounted with only an absolute path
 * resolves itself through `resolveFile`, which coalesces every card that
 * asks within a frame into one request. Answers carry the file's version
 * and a `/raw` ticket the daemon keeps stable for that version, so a card
 * re-rendered (or remounted by a tab switch) reuses the browser's cached
 * bytes, and an overwritten file gets a new URL.
 *
 * `mountEmbed` (./mount.svelte.ts) is the entry point for renderers that
 * own raw DOM rather than Svelte markup.
 */

import { api, ApiError } from "../../net/api";
import { basename, viewKindFor } from "../../previews/files";
import type { Locator } from "../reveal";
import type { Region } from "./fragment";

/** One resolved target (the daemon's answer, verbatim). */
export interface TargetInfo {
  /** Canonical absolute path. */
  path: string;
  kind: "file" | "dir";
  size: number;
  /** The file's version token (fs/file's `X-Mtime`). */
  version: string;
  /** Last modification, epoch ms (daemon clock). */
  mtime_ms: number | null;
  /** Guessed from the extension; advisory. */
  mime: string;
  /** Image size from the header bytes, when the format says. */
  width?: number;
  height?: number;
  /** `/raw` ticket, for kinds loaded through one (images, PDF, HTML, media). */
  ticket?: string;
}

export interface TargetMissing {
  missing: true;
}

export type TargetResult = TargetInfo | TargetMissing;

export function isMissing(r: TargetResult): r is TargetMissing {
  return "missing" in r;
}

/** Server cap on targets per request. */
export const RESOLVE_MAX = 200;
/** Server cap on extra `bases`. */
const RESOLVE_BASES_MAX = 8;

export interface ResolveOptions {
  /** Directories tried after `base`, in order. */
  bases?: string[];
  /** Enables the root-relative `/x` fallback onto the workspace root. */
  workspaceId?: string | null;
  signal?: AbortSignal;
}

/**
 * Resolve link/embed targets as a document writes them (`figs/plot.png`,
 * `data.csv#row=2-9`, `/docs/x.md`) against `base` — strictly, exactly
 * like document links. Answers every target the daemon reached; a target
 * absent from the result is unknown (the daemon's time budget ran out, or
 * a later batch failed), never a miss. Batches of RESOLVE_MAX run one after
 * another. Throws only when nothing was answered.
 */
export async function resolveTargets(
  targets: readonly string[],
  base: string,
  opts: ResolveOptions = {},
): Promise<Record<string, TargetResult>> {
  const out: Record<string, TargetResult> = {};
  const unique = [...new Set(targets)].filter((t) => t.length > 0);
  const bases = [...new Set(opts.bases ?? [])].filter((b) => b !== base).slice(0, RESOLVE_BASES_MAX);
  for (let i = 0; i < unique.length; i += RESOLVE_MAX) {
    const batch = unique.slice(i, i + RESOLVE_MAX);
    let body: { results?: Record<string, TargetResult> };
    try {
      const res = await api("/fs/resolve_targets", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          base,
          targets: batch,
          ...(bases.length > 0 ? { bases } : {}),
          ...(opts.workspaceId != null ? { workspace_id: opts.workspaceId } : {}),
        }),
        ...(opts.signal !== undefined ? { signal: opts.signal } : {}),
      });
      if (!res.ok) {
        let message = `resolve failed with status ${res.status}`;
        try {
          const err = (await res.json()) as { error?: string };
          if (err.error) message = err.error;
        } catch {
          // non-JSON error body
        }
        throw new ApiError(res.status, message);
      }
      body = (await res.json()) as { results?: Record<string, TargetResult> };
    } catch (err) {
      if (i === 0) throw err;
      break;
    }
    Object.assign(out, body.results ?? {});
  }
  return out;
}

// --- one card at a time, one request per frame --------------------------------

type Waiter = (r: TargetResult | null) => void;
const waiting = new Map<string, Waiter[]>();
/** Paths whose request is on the wire: later askers join it. */
const inflight = new Set<string>();
let flushTimer: ReturnType<typeof setTimeout> | null = null;
/** Long enough to gather every card a render mounts (a replayed chat
 *  mounts dozens at once), short enough not to be seen. */
const COALESCE_MS = 16;

/**
 * A filesystem path as a `resolve_targets` target. The daemon reads a
 * target the way a document writes a link — it cuts at `#` and `?`, decodes
 * `%XX`, trims whitespace, unwraps `<…>` and refuses a `//host` — so every
 * character it would interpret is percent-escaped, and the path names the
 * same file after its decode: `/scratch/run#2/plot.png`, `/data/50%/x.png`.
 */
export function pathTarget(path: string): string {
  return path.replace(/[%#?<>\s]/gu, (c) => encodeURIComponent(c)).replace(/^\/\//, "/%2F");
}

async function flush(): Promise<void> {
  flushTimer = null;
  const batch = [...waiting.keys()].filter((p) => !inflight.has(p)).slice(0, RESOLVE_MAX);
  if (batch.length === 0) return;
  for (const path of batch) inflight.add(path);
  let results: Record<string, TargetResult> | null;
  try {
    results = await resolveTargets(batch.map(pathTarget), "/");
  } catch {
    results = null;
  }
  for (const path of batch) {
    inflight.delete(path);
    const ws = waiting.get(path);
    waiting.delete(path);
    for (const w of ws ?? []) w(results?.[pathTarget(path)] ?? null);
  }
  if (waiting.size > 0 && flushTimer === null) flushTimer = setTimeout(() => void flush(), 0);
}

/**
 * Resolve one ABSOLUTE filesystem path (a tool location, a card's refresh
 * after a disk change) — a real path, never a link target: `#`, `?` and `%`
 * are part of the name (`pathTarget`). Coalesced with every other card
 * asking in the same frame. Null when the daemon could not answer
 * (unreachable): unknown, not missing.
 */
export function resolveFile(path: string): Promise<TargetResult | null> {
  return new Promise((resolve) => {
    const ws = waiting.get(path);
    if (ws !== undefined) ws.push(resolve);
    else waiting.set(path, [resolve]);
    if (flushTimer === null) flushTimer = setTimeout(() => void flush(), COALESCE_MS);
  });
}

// --- what a card draws ------------------------------------------------------------

export type EmbedKind =
  | "image"
  | "pdf"
  | "code"
  | "table"
  | "xlsx"
  | "html"
  | "video"
  | "audio"
  | "notebook"
  | "markdown"
  | "file"
  | "dir";

/** The body a resolved file gets. Markdown decides between a text excerpt
 *  and a Marp slide once it has read the source. */
export function embedKind(info: Pick<TargetInfo, "path" | "kind">): EmbedKind {
  if (info.kind === "dir") return "dir";
  switch (viewKindFor(info.path)) {
    case "image":
      return "image";
    case "pdf":
      return "pdf";
    case "html":
      return "html";
    case "table":
      return "table";
    case "xlsx":
      return "xlsx";
    case "video":
      return "video";
    case "audio":
      return "audio";
    case "notebook":
      return "notebook";
    case "markdown":
      return "markdown";
    case "text":
    case "log":
    case "mermaid":
      return "code";
    default:
      return "file";
  }
}

/** The `/raw` URL of a ticketed file; an HTML page is addressed by its own
 *  name under the ticket, so its relative assets resolve through the
 *  daemon's folder-confined `/raw/{ticket}/{path}` route. */
export function rawUrl(info: TargetInfo): string | null {
  if (info.ticket === undefined) return null;
  if (embedKind(info) === "html") return `/raw/${info.ticket}/${encodeURIComponent(basename(info.path))}`;
  return `/raw/${info.ticket}`;
}

/** A media element's source with its `#t=` moment (browsers seek natively). */
export function mediaUrl(url: string, time: Locator["time"]): string {
  if (time === undefined) return url;
  return `${url}#t=${time.start}${time.end !== undefined ? `,${time.end}` : ""}`;
}

/**
 * Obsidian's size hint in alt text: `plot|400` (width) or `plot|400x300`.
 * The caption keeps everything before the last `|`.
 */
export function parseSizeHint(alt: string): { alt: string; width: number | null; height: number | null } {
  const m = /^(.*)\|\s*(\d{1,5})(?:\s*x\s*(\d{1,5}))?\s*$/.exec(alt);
  if (m === null) return { alt, width: null, height: null };
  const width = Number(m[2]);
  const height = m[3] !== undefined ? Number(m[3]) : null;
  return { alt: m[1].trim(), width: width > 0 ? width : null, height: height !== null && height > 0 ? height : null };
}

/** Split a target into its path and fragment (`a.png#xywh=…`). */
export function splitTarget(target: string): { path: string; fragment: string | null } {
  const i = target.indexOf("#");
  if (i < 0) return { path: target, fragment: null };
  return { path: target.slice(0, i), fragment: i < target.length - 1 ? target.slice(i + 1) : null };
}

/**
 * How a region of a `natural`-sized picture is drawn cropped: the frame's
 * aspect ratio, and where the full picture sits inside it (percentages of
 * the frame, so it scales with the card). Null when the region is empty
 * or falls outside the picture.
 */
export function cropLayout(
  natural: { w: number; h: number },
  region: Region,
): { aspect: number; width: number; height: number; left: number; top: number } | null {
  if (natural.w <= 0 || natural.h <= 0) return null;
  const px =
    region.percent === true
      ? {
          x: (region.x / 100) * natural.w,
          y: (region.y / 100) * natural.h,
          w: (region.w / 100) * natural.w,
          h: (region.h / 100) * natural.h,
        }
      : region;
  const x = Math.min(Math.max(0, px.x), natural.w);
  const y = Math.min(Math.max(0, px.y), natural.h);
  const w = Math.min(px.w, natural.w - x);
  const h = Math.min(px.h, natural.h - y);
  if (w <= 0 || h <= 0) return null;
  return {
    aspect: w / h,
    width: (natural.w / w) * 100,
    height: (natural.h / h) * 100,
    left: (-x / w) * 100,
    top: (-y / h) * 100,
  };
}

/**
 * Report whether `el`'s content overflows its (clipped) box — an excerpt
 * that needs a "more" — now and whenever it or its children resize (an
 * image in it finishes loading). Returns the disconnect.
 */
export function watchOverflow(el: HTMLElement, on: (overflowing: boolean) => void): () => void {
  const check = () => on(el.scrollHeight > el.clientHeight + 2);
  if (typeof ResizeObserver === "undefined") {
    check();
    return () => {};
  }
  const ro = new ResizeObserver(check);
  ro.observe(el);
  for (const child of el.children) ro.observe(child);
  check();
  return () => ro.disconnect();
}

/**
 * The widest a picture's frame is drawn: its natural width, the author's
 * size hint, and whatever width keeps it under `maxHeight` — the frame then
 * reserves exactly its final box (width + aspect-ratio) before a byte loads.
 */
export function frameWidth(
  natural: { w: number; h: number },
  maxHeight: number,
  hint: number | null = null,
): number {
  const byHeight = (maxHeight * natural.w) / natural.h;
  return Math.max(1, Math.round(Math.min(natural.w, byHeight, hint ?? Infinity)));
}
