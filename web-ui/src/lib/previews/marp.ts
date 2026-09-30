/**
 * Marp decks: markdown whose frontmatter says `marp: true`, `---` between
 * slides. The pure half of the slides view — detection (FileView decides the
 * default view from it), image-target rewriting, and the sandboxed documents
 * the slides are drawn in. The renderer itself (`@marp-team/marp-core`) is
 * loaded only by SlidesView.
 */

/** A leading frontmatter block's text (fences optional) says `marp: true`. */
export function isMarpFrontmatter(yaml: string): boolean {
  return /^[ \t]*marp[ \t]*:[ \t]*(?:true|"true"|'true')[ \t]*(?:#.*)?$/m.test(yaml);
}

/** The frontmatter of a markdown source, fences excluded, or null. Mirrors
 *  the editor's shape: `---` on the first line, closed by `---` or `...`. */
export function frontmatterOf(source: string): string | null {
  const text = source.startsWith("﻿") ? source.slice(1) : source;
  if (!/^---[ \t]*\r?\n/.test(text)) return null;
  const lines = text.split(/\r?\n/);
  // A block longer than this is not frontmatter anyone meant (the daemon's
  // own reading render applies the same bound).
  const max = Math.min(lines.length, 200);
  for (let i = 1; i < max; i++) {
    if (/^(?:---|\.\.\.)[ \t]*$/.test(lines[i])) return lines.slice(1, i).join("\n");
  }
  return null;
}

/** Whether a markdown source is a Marp deck. */
export function isMarpSource(source: string): boolean {
  const fm = frontmatterOf(source);
  return fm !== null && isMarpFrontmatter(fm);
}

/** A target that needs no rewriting: a URL with a scheme, protocol-relative,
 *  or an in-page anchor. */
function isAbsoluteTarget(url: string): boolean {
  return /^[a-z][a-z0-9+.-]*:/i.test(url) || url.startsWith("//") || url.startsWith("#");
}

// `![alt](target "title")` — the target may be <angle-bracketed>. Alt text
// carries Marp's image keywords (`bg`, `w:200px`), which pass through.
const IMAGE = /(!\[[^\]\n]*\]\(\s*)(<[^>\n]*>|[^\s)]+)/g;
const FENCE = /^[ \t]{0,3}(`{3,}|~{3,})/;

/** Walk the image targets outside fenced code, replacing each via `swap`
 *  (null keeps it). Inline code spans are left alone too. */
function mapImages(markdown: string, swap: (target: string) => string | null): string {
  const lines = markdown.split("\n");
  let fence: string | null = null;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const f = FENCE.exec(line);
    if (fence !== null) {
      if (f !== null && f[1][0] === fence[0] && f[1].length >= fence.length) fence = null;
      continue;
    }
    if (f !== null) {
      fence = f[1];
      continue;
    }
    if (!line.includes("![")) continue;
    // Split out inline code spans so an example `![x](y)` is not rewritten.
    lines[i] = line
      .split(/(`+[^`]*`+)/)
      .map((part, j) =>
        j % 2 === 1
          ? part
          : part.replace(IMAGE, (m, head: string, raw: string) => {
              const bare = raw.startsWith("<") ? raw.slice(1, -1) : raw;
              const next = swap(bare);
              return next === null ? m : `${head}<${next}>`;
            }),
      )
      .join("");
  }
  return lines.join("\n");
}

/** Relative image targets (deck-folder paths) in document order, deduped. */
export function relativeImages(markdown: string): string[] {
  const out = new Set<string>();
  mapImages(markdown, (t) => {
    if (t !== "" && !isAbsoluteTarget(t)) out.add(t);
    return null;
  });
  return [...out];
}

/** `markdown` with each relative image target replaced from `urls`
 *  (targets without an entry stay as written). */
export function rewriteImages(markdown: string, urls: ReadonlyMap<string, string>): string {
  return mapImages(markdown, (t) => urls.get(t) ?? null);
}

/** Slide size from Marp's rendered inline SVG (`viewBox="0 0 W H"`). */
export function slideSize(html: string): { w: number; h: number } {
  const m = /data-marpit-svg=""[^>]*viewBox="0 0 (\d+(?:\.\d+)?) (\d+(?:\.\d+)?)"/.exec(html) ??
    /viewBox="0 0 (\d+(?:\.\d+)?) (\d+(?:\.\d+)?)"/.exec(html);
  if (m === null) return { w: 1280, h: 720 };
  return { w: Number(m[1]), h: Number(m[2]) };
}

/** How many slides a render holds. */
export function slideCount(html: string): number {
  return (html.match(/data-marpit-svg=""/g) ?? []).length;
}

/**
 * The width (px) a slide is printed at where the print engine ignores
 * `@page` size (WebKit: Safari and the macOS app print on the panel's
 * paper). WebKit lays a printed page out 1.25–2× its printable width and
 * clips anything wider, so 900px keeps a whole slide on every common paper,
 * portrait or landscape, with or without margins.
 */
export const PRINT_FIT_WIDTH = 900;

/** Whether this engine prints on the panel's paper whatever `@page` asks
 *  (WebKit), rather than on a page the slide's own size (Chromium). */
export function printIgnoresPageSize(userAgent: string): boolean {
  return /AppleWebKit/.test(userAgent) && !/Chrom(e|ium)\/|Edg\//.test(userAgent);
}

/**
 * The document one iframe draws: every slide at its native size, stacked
 * (`column`, the stage — the parent moves and scales the frame to show one)
 * or in a row with gaps (`row`, the thumbnail strip), or one per printed
 * page (`print`; with `fitWidth`, each slide scaled to that width). Nothing
 * in it runs: the frame is sandboxed without scripts, and Marp rendered
 * with `html: false` and no script.
 */
export function slideDocument(
  css: string,
  html: string,
  size: { w: number; h: number },
  layout: "column" | "row" | "print",
  gap = 0,
  fitWidth?: number,
): string {
  const { w, h } = size;
  const frame =
    layout === "print"
      ? printFrameCss(w, h, fitWidth)
      : `html,body{margin:0;padding:0;overflow:hidden;background:transparent}
.marpit{display:flex;flex-direction:${layout === "row" ? "row" : "column"};gap:${gap}px;width:max-content}
svg[data-marpit-svg]{display:block;flex:none;width:${w}px;height:${h}px}`;
  // `html` is Marp's own output (raw HTML disabled); `css` is its theme CSS.
  return `<!doctype html><html><head><meta charset="utf-8"><style>${css}</style><style>${frame}</style></head><body>${html}</body></html>`;
}

/**
 * One slide per printed page. These rules come after Marp's theme CSS and
 * reuse its selectors, so they win over its own `@media print` block, which
 * breaks WebKit: it sizes each slide 100vw × 100vh (resolved against the
 * hidden 0×0 print frame, so every slide collapses and the PDF is blank)
 * and forces a page break inside each slide's SVG. Scaling uses a CSS
 * transform, never the SVG's own width: WebKit does not scale the HTML in
 * an SVG `foreignObject` by its `viewBox`.
 */
function printFrameCss(w: number, h: number, fitWidth?: number): string {
  const base = `@page{size:${w}px ${h}px;margin:0}html,body{margin:0;padding:0;background:#fff;break-inside:auto;page-break-inside:auto}
div.marpit>svg>foreignObject>section{break-before:auto;page-break-before:auto}`;
  const k = fitWidth === undefined ? 1 : Math.min(1, fitWidth / w);
  const slide = `display:block;width:${w}px;height:${h}px;break-after:page;page-break-after:always`;
  if (k === 1) return `${base}\ndiv.marpit>svg[data-marpit-svg]{${slide}}`;
  // A transformed box adds only its scaled bounds to the page's overflow, so
  // the page is laid out no wider than the scaled slide. The negative bottom
  // margin pulls the box's height in to match (a right margin would be
  // ignored: the fixed width over-constrains it); the left margin centres it.
  const fw = Math.round(w * k);
  const fh = Math.round(h * k);
  return `${base}\ndiv.marpit>svg[data-marpit-svg]{${slide};transform:scale(${k});transform-origin:0 0;margin:0 0 ${fh - h}px max(0px,calc((100% - ${fw}px) / 2))}`;
}
