/**
 * Publishing a markdown document — the pure pieces behind the toolbar's
 * publish menu (`PublishButton.svelte`; the page work is `publishRun.ts`):
 *
 * - which local files a document names (`docTargets`): every link, image,
 *   wikilink, reference definition and raw-HTML `src`/`href`, with where
 *   its destination sits in the source, read off the one syntax tree;
 * - how a bundle is laid out (`planBundle`): the document plus every file
 *   it names inside its workspace, at the places its links name them, so
 *   relative links hold unchanged on GitHub, in Obsidian or unzipped — only
 *   an absolute path (`/docs/x.md`, `/home/…/plot.png`) is rewritten into a
 *   relative one, in the bundle's copy;
 * - which pictures an HTML export inlines under its byte cap (`planInline`);
 * - the exported page's shell (`titleBlock`, `htmlPage`): a title block
 *   from the frontmatter, a policy that lets nothing load and no script run.
 */
import type { SyntaxNode } from "@lezer/common";
import { inlineOf, destinationText, type DocText } from "../mdTable";
import { isMath } from "../mdMath";
import { classifyHref, parseFrontmatter } from "../mdDoc";
import { basename, dirname, resolveDocPath } from "../files";
import { bodyText, docText } from "./model";
import { docParser } from "./parser";
import { escapeHref, escapeText, hrefFor, wikilinkHref } from "./render";

// --- the files a document names -----------------------------------------------------

export type TargetKind = "link" | "image" | "wikilink" | "html";

export interface DocTarget {
  /** The destination as an href, escaped the way the renderer writes one. */
  href: string;
  /** Where the destination's text sits in the source (original offsets,
   *  CRLF and all): what a rewrite replaces. Null for a wikilink, which
   *  names a note, not a path, and is never rewritten. */
  span: { from: number; to: number } | null;
  kind: TargetKind;
  /** A wikilink: resolved by name when it isn't beside the document. */
  byName: boolean;
}

/** Syntax that holds no destination worth following. */
const OPAQUE = new Set([
  "FencedCode",
  "CodeBlock",
  "InlineCode",
  "CommentBlock",
  "Comment",
  "ProcessingInstructionBlock",
  "ProcessingInstruction",
  "Autolink",
]);

/** `src=` and `href=` in raw HTML, quoted either way or bare. */
const HTML_ATTR = /\b(?:src|href)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+))/gi;

/** Offsets in the parser's text (CRLF folded to LF) back to the source's. */
function sourceOffsets(source: string): (pos: number) => number {
  if (!source.includes("\r\n")) return (pos) => pos;
  // The LF-text position of each folded `\r\n`'s `\n`, in order.
  const folds: number[] = [];
  let removed = 0;
  for (let i = source.indexOf("\r\n"); i >= 0; i = source.indexOf("\r\n", i + 2)) {
    folds.push(i - removed);
    removed++;
  }
  return (pos) => {
    let lo = 0;
    let hi = folds.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (folds[mid] < pos) lo = mid + 1;
      else hi = mid;
    }
    return pos + lo;
  };
}

/**
 * Every destination the document names, in order: links, images and
 * reference definitions (`[x]: path`) with the source span of their URL,
 * wikilinks as the href the renderer gives them, and raw HTML's `src` and
 * `href` values. Frontmatter, code and comments name nothing. Web, mail
 * and in-page targets are included — `localTarget` tells them apart.
 */
export function docTargets(source: string): DocTarget[] {
  const { text } = bodyText(source);
  const doc: DocText = docText(text);
  const toSource = sourceOffsets(source);
  const out: DocTarget[] = [];
  const url = (n: SyntaxNode, kind: TargetKind): void => {
    const u = n.getChild("URL");
    if (u === null) return;
    const href = hrefFor(destinationText(text.slice(u.from, u.to)));
    if (href === null || href === "") return;
    out.push({ href, span: { from: toSource(u.from), to: toSource(u.to) }, kind, byName: false });
  };
  docParser.parse(text).iterate({
    enter: (n) => {
      if (OPAQUE.has(n.name) || isMath(n.type)) return false;
      switch (n.name) {
        case "Link":
        case "LinkReference":
          url(n.node, "link");
          return n.name === "Link"; // a link's text can hold an image
        case "Image":
          url(n.node, "image");
          return false;
        case "Wikilink": {
          const parent = n.node.parent;
          if (parent === null) return false;
          for (const i of inlineOf(parent, n.from, n.to, doc)) {
            if (i.kind !== "wikilink" || i.target === "" || /^[a-z][a-z0-9+.-]*:/i.test(i.target)) continue;
            // As the renderer names it (`note` → `note.md`), the fragment
            // as written for an embed (a spot, not a heading).
            const href = i.embed
              ? `${wikilinkHref(i.target, null)}${i.heading === null ? "" : `#${escapeHref(i.heading)}`}`
              : wikilinkHref(i.target, i.heading);
            out.push({ href, span: null, kind: "wikilink", byName: true });
          }
          return false;
        }
        case "HTMLBlock":
        case "HTMLTag": {
          const raw = text.slice(n.from, n.to);
          for (const m of raw.matchAll(HTML_ATTR)) {
            const value = m[1] ?? m[2] ?? m[3] ?? "";
            if (value.trim() === "") continue;
            const at = n.from + (m.index ?? 0) + m[0].length - value.length - (m[3] === undefined ? 1 : 0);
            out.push({ href: value.trim(), span: { from: toSource(at), to: toSource(at + value.length) }, kind: "html", byName: false });
          }
          return false;
        }
      }
      return undefined;
    },
  });
  return out;
}

/** A target that names a file on this host: its decoded path (relative to
 *  the document, or absolute) and fragment. Null for web, mail and
 *  in-page targets and anything with another scheme. */
export function localTarget(href: string): { path: string; fragment: string | null } | null {
  const t = classifyHref(href);
  return t.kind === "path" ? { path: t.path, fragment: t.fragment } : null;
}

// --- paths ------------------------------------------------------------------------

/** Whether absolute `path` is `dir` or inside it. */
export function isInside(path: string, dir: string): boolean {
  if (dir === "/") return path.startsWith("/");
  const d = dir.replace(/\/+$/, "");
  return path === d || path.startsWith(`${d}/`);
}

function segments(path: string): string[] {
  return path.split("/").filter((s) => s !== "");
}

/** The relative path from directory `fromDir` to `to` (both absolute). */
export function relPath(fromDir: string, to: string): string {
  const a = segments(fromDir);
  const b = segments(to);
  let i = 0;
  while (i < a.length && i < b.length && a[i] === b[i]) i++;
  const up = a.length - i;
  const rest = b.slice(i);
  const parts = [...Array<string>(up).fill(".."), ...rest];
  return parts.length === 0 ? "." : parts.join("/");
}

/** The deepest directory holding every one of `dirs` (absolute). */
export function commonDir(dirs: readonly string[]): string {
  if (dirs.length === 0) return "/";
  let common = segments(dirs[0]);
  for (const d of dirs.slice(1)) {
    const s = segments(d);
    let i = 0;
    while (i < common.length && i < s.length && common[i] === s[i]) i++;
    common = common.slice(0, i);
  }
  return `/${common.join("/")}`;
}

/** A file name without its last extension, safe as a download or folder
 *  name. */
export function stemOf(path: string): string {
  const name = basename(path);
  const dot = name.lastIndexOf(".");
  const stem = (dot > 0 ? name.slice(0, dot) : name).replace(/[\\/:*?"<>|\u0000-\u001f]+/g, "-").trim();
  return stem === "" ? "document" : stem;
}

// --- the bundle ---------------------------------------------------------------------

/** A local target the daemon found. */
export interface FoundTarget {
  target: DocTarget;
  /** The path as the document writes it (decoded). */
  written: string;
  fragment: string | null;
  /** The file it names, canonical (a symlink resolved). */
  real: string;
  kind: "file" | "dir";
  size: number;
}

export type SkipReason = "outside" | "folder" | "missing" | "cap";

export interface BundlePlan {
  /** The folder on disk the bundle mirrors. */
  root: string;
  /** The document's own entry. */
  doc: string;
  /** Every other file: its entry, and the file to read into it. */
  files: { entry: string; real: string; size: number }[];
  /** Edits to the document's copy: absolute destinations made relative. */
  rewrites: Edit[];
  skipped: { href: string; why: SkipReason }[];
  bytes: number;
}

export interface Edit {
  from: number;
  to: number;
  insert: string;
}

export interface BundleCaps {
  files: number;
  bytes: number;
}

/** A bundle is built in the browser, whole in memory: bounded to what a
 *  tab holds comfortably. */
export const BUNDLE_CAPS: BundleCaps = { files: 500, bytes: 100 * 1024 * 1024 };

/**
 * Lay out a bundle: the document and every file it names that lies inside
 * `boundary` (its workspace), each at the place its link names — beside
 * the document for a relative link (the file's bytes may come from where a
 * symlink points), where it really is for an absolute path (rewritten to a
 * relative one in the copy) or a wikilink (Obsidian finds those by name).
 * Entries sit under one folder named after the document, mirroring the
 * deepest directory that holds them all, so every relative link resolves
 * inside the unzipped folder. Folders, files outside the boundary and
 * files past the caps are left out and reported; so are targets the daemon
 * found nothing at (`missing`, from the caller).
 */
export function planBundle(
  docPath: string,
  boundary: string,
  found: readonly FoundTarget[],
  missing: readonly DocTarget[] = [],
  caps: BundleCaps = BUNDLE_CAPS,
): BundlePlan {
  const docDir = dirname(docPath);
  const skipped: BundlePlan["skipped"] = missing.map((t) => ({ href: t.href, why: "missing" as const }));
  const placed = new Map<string, { real: string; size: number }>();
  const rewrites: Edit[] = [];
  let bytes = 0;
  for (const f of found) {
    const absolute = f.written.startsWith("/");
    const named = f.target.byName || absolute ? f.real : resolveDocPath(docPath, f.written);
    if (named === docPath || f.real === docPath) continue;
    if (!isInside(named, boundary)) {
      skipped.push({ href: f.target.href, why: "outside" });
      continue;
    }
    if (f.kind === "dir") {
      skipped.push({ href: f.target.href, why: "folder" });
      continue;
    }
    if (!placed.has(named)) {
      if (placed.size >= caps.files || bytes + f.size > caps.bytes) {
        skipped.push({ href: f.target.href, why: "cap" });
        continue;
      }
      placed.set(named, { real: f.real, size: f.size });
      bytes += f.size;
    }
    if (absolute && f.target.span !== null) {
      const rel = escapeHref(relPath(docDir, named));
      rewrites.push({ ...f.target.span, insert: f.fragment === null ? rel : `${rel}#${f.fragment}` });
    }
  }
  const root = commonDir([docDir, ...[...placed.keys()].map(dirname)]);
  const folder = stemOf(docPath);
  const entry = (p: string): string => `${folder}/${relPath(root, p)}`;
  return {
    root,
    doc: entry(docPath),
    files: [...placed].map(([named, f]) => ({ entry: entry(named), real: f.real, size: f.size })),
    rewrites,
    skipped,
    bytes,
  };
}

/** `text` with non-overlapping `edits` applied (any order). */
export function applyEdits(text: string, edits: readonly Edit[]): string {
  let out = text;
  for (const e of [...edits].sort((a, b) => b.from - a.from)) out = out.slice(0, e.from) + e.insert + out.slice(e.to);
  return out;
}

// --- inlining pictures ---------------------------------------------------------------

/** What an HTML export inlines at most, in the pictures' own bytes (the
 *  page grows by a third more: base64). */
export const INLINE_CAP = 50 * 1024 * 1024;

/** The pictures an export inlines, in document order until the next one
 *  would pass `cap`; the rest become links. Each use counts: a picture
 *  drawn twice is inlined twice. */
export function planInline<T extends { size: number }>(items: readonly T[], cap = INLINE_CAP): { inline: T[]; over: T[]; bytes: number } {
  const inline: T[] = [];
  const over: T[] = [];
  let bytes = 0;
  for (const item of items) {
    if (bytes + item.size <= cap) {
      inline.push(item);
      bytes += item.size;
    } else {
      over.push(item);
    }
  }
  return { inline, over, bytes };
}

// --- the page ---------------------------------------------------------------------

export interface TitleBlock {
  /** The page's `<title>`. */
  title: string;
  /** The title the block shows; null when the document's own first
   *  heading already says it. */
  heading: string | null;
  summary: string | null;
  updated: string | null;
}

/** The export's title block from the frontmatter's `title`, `summary` and
 *  `updated` (the portable dialect's keys); the page title falls back to
 *  the first heading, then the file name. */
export function titleBlock(frontmatter: string | null, firstHeading: string | null, docPath: string): TitleBlock {
  const entries = frontmatter === null ? null : parseFrontmatter(frontmatter);
  const text = (key: string): string | null => {
    const e = entries?.find((x) => x.key.toLowerCase() === key);
    if (e === undefined) return null;
    const v = e.value;
    const s = v.kind === "text" ? v.text : v.kind === "list" ? v.items.join(", ") : v.kind === "raw" ? v.text : null;
    return s === null || s.trim() === "" ? null : s.trim();
  };
  const fmTitle = text("title");
  const same = (a: string, b: string | null): boolean =>
    b !== null && a.replace(/\s+/g, " ").toLowerCase() === b.replace(/\s+/g, " ").trim().toLowerCase();
  return {
    title: fmTitle ?? firstHeading ?? stemOf(docPath),
    heading: fmTitle !== null && !same(fmTitle, firstHeading) ? fmTitle : null,
    summary: text("summary"),
    updated: text("updated"),
  };
}

function escapeAttr(s: string): string {
  return escapeText(s).replace(/"/g, "&quot;");
}

/**
 * Nothing on the page may load or run: pictures are inlined `data:` URLs,
 * styles inline, and there is no script. The policy says so to the
 * browser, so a stray reference in the document's own HTML stays inert.
 */
export const PAGE_POLICY = "default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:";

/** The exported page: `body` is the rendered article's markup. */
export function htmlPage(block: TitleBlock, css: string, body: string): string {
  const meta: string[] = [];
  if (block.heading !== null) meta.push(`<h1 class="doc-title">${escapeText(block.heading)}</h1>`);
  if (block.summary !== null) meta.push(`<p class="doc-summary">${escapeText(block.summary)}</p>`);
  if (block.updated !== null) meta.push(`<p class="doc-updated">Updated ${escapeText(block.updated)}</p>`);
  const header = meta.length === 0 ? "" : `<header class="doc-meta">${meta.join("")}</header>\n`;
  return `<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="${escapeAttr(PAGE_POLICY)}">
<meta name="generator" content="chimaera">
<title>${escapeText(block.title)}</title>
<style>${css}</style>
</head>
<body>
<main class="page">
${header}<article class="md-doc">
${body}
</article>
</main>
</body>
</html>
`;
}
