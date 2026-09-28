/**
 * Which diagram-board format a path is (FileView routes all of them to
 * BoardView), and the geometry every board shares. A leaf on purpose: the
 * format modules import it, and each is its own lazy chunk, so it must not
 * pull the app's API layer in with it (that reshuffles the startup chunks).
 */

export type BoardFormat = "canvas" | "excalidraw" | "drawio";

const basename = (path: string): string => path.slice(path.lastIndexOf("/") + 1);

export function boardFormat(path: string): BoardFormat | null {
  const name = basename(path).toLowerCase();
  if (name.endsWith(".excalidraw.json")) return "excalidraw";
  switch (name.slice(name.lastIndexOf(".") + 1)) {
    case "canvas":
      return "canvas";
    case "excalidraw":
      return "excalidraw";
    case "drawio":
    case "dio":
      return "drawio";
    default:
      return null;
  }
}

/** The file's name without its board extension, for export file names. */
export function boardStem(path: string): string {
  return basename(path).replace(/\.(canvas|excalidraw\.json|excalidraw|drawio|dio)$/i, "") || "board";
}

/** A world-space rectangle. */
export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** The smallest rect holding all of `rects`, grown by `pad` (null for none). */
export function unionRects(rects: Iterable<Rect>, pad = 0): Rect | null {
  let x0 = Infinity;
  let y0 = Infinity;
  let x1 = -Infinity;
  let y1 = -Infinity;
  for (const r of rects) {
    if (![r.x, r.y, r.w, r.h].every(Number.isFinite)) continue;
    x0 = Math.min(x0, r.x);
    y0 = Math.min(y0, r.y);
    x1 = Math.max(x1, r.x + r.w);
    y1 = Math.max(y1, r.y + r.h);
  }
  if (!Number.isFinite(x0)) return null;
  return { x: x0 - pad, y: y0 - pad, w: x1 - x0 + pad * 2, h: y1 - y0 + pad * 2 };
}

/** The pan/zoom that fits `bounds` in a `vw`×`vh` view with `pad` px of air,
 *  never enlarging past `maxScale`. */
export function fitView(
  bounds: Rect,
  vw: number,
  vh: number,
  pad = 24,
  maxScale = 1,
): { scale: number; tx: number; ty: number } {
  const s = Math.max(
    0.02,
    Math.min(maxScale, (vw - pad * 2) / Math.max(1, bounds.w), (vh - pad * 2) / Math.max(1, bounds.h)),
  );
  return {
    scale: s,
    tx: (vw - bounds.w * s) / 2 - bounds.x * s,
    ty: (vh - bounds.h * s) / 2 - bounds.y * s,
  };
}

/** Zoom by `factor` around view point (cx, cy), clamped to [min, max]. */
export function zoomAround(
  view: { scale: number; tx: number; ty: number },
  factor: number,
  cx: number,
  cy: number,
  min = 0.02,
  max = 8,
): { scale: number; tx: number; ty: number } {
  const next = Math.min(max, Math.max(min, view.scale * factor));
  const wx = (cx - view.tx) / view.scale;
  const wy = (cy - view.ty) / view.scale;
  return { scale: next, tx: cx - wx * next, ty: cy - wy * next };
}

/** A drawing's paper color, with every spelling of white (and anything
 *  unusable) as `#ffffff`, the default the view leaves off on screen. */
export function paperColor(c: unknown): string {
  const s = safeColor(c)?.toLowerCase() ?? "#ffffff";
  return s === "#fff" || s === "#ffff" || s === "#ffffffff" || s === "white" || s === "transparent" ? "#ffffff" : s;
}

/** Only colors that can't reference anything: hex, rgb()/hsl() with plain
 *  numbers, or a bare named color. Anything else (a url(), var(), an
 *  expression) is refused. */
export function safeColor(c: unknown): string | null {
  if (typeof c !== "string") return null;
  const s = c.trim();
  if (/^#(?:[0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i.test(s)) return s;
  if (/^(?:rgb|rgba|hsl|hsla)\(\s*[-\d.%\s,/]+\)$/i.test(s)) return s;
  if (/^[a-z]{3,24}$/i.test(s)) return s.toLowerCase();
  return null;
}
