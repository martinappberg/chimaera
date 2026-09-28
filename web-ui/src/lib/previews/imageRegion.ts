/**
 * Framing a pixel region of an image (a `#xywh=` reveal) in the image
 * viewer's pan/zoom space. Pure, so the arithmetic is unit-tested.
 */

export interface Region {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** `r` clipped to a `width`×`height` image, or null when nothing of it is
 *  inside (or any coordinate is not a finite number). */
export function clampRegion(r: Region, width: number, height: number): Region | null {
  if (![r.x, r.y, r.w, r.h].every(Number.isFinite)) return null;
  const x0 = Math.max(0, Math.min(r.x, r.x + r.w));
  const y0 = Math.max(0, Math.min(r.y, r.y + r.h));
  const x1 = Math.min(width, Math.max(r.x, r.x + r.w));
  const y1 = Math.min(height, Math.max(r.y, r.y + r.h));
  if (x1 <= x0 || y1 <= y0) return null;
  return { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
}

export interface View {
  scale: number;
  tx: number;
  ty: number;
}

/**
 * The view that shows `r` centered in a `vw`×`vh` viewport, zoomed until it
 * spans `fill` of the tighter axis — but never below `minScale` (fit: a
 * region is never shown smaller than the whole image would be) nor above
 * `maxScale` (a 1-pixel region must not zoom to infinity).
 */
export function frameRegion(
  r: Region,
  vw: number,
  vh: number,
  opts: { fill: number; minScale: number; maxScale: number },
): View {
  const want = Math.min((vw * opts.fill) / r.w, (vh * opts.fill) / r.h);
  const scale = Math.min(opts.maxScale, Math.max(opts.minScale, want));
  return {
    scale,
    tx: vw / 2 - (r.x + r.w / 2) * scale,
    ty: vh / 2 - (r.y + r.h / 2) * scale,
  };
}

// --- pointing at a region (the area tool) ----------------------------------------

export interface Point {
  x: number;
  y: number;
}

/**
 * A client point in image pixels: the viewport's box sits at `origin` (its
 * client left/top) and shows the image panned by (tx, ty) and zoomed by
 * `scale` — the image viewer's transform, inverted.
 */
export function screenToImage(clientX: number, clientY: number, origin: Point, view: View): Point {
  return { x: (clientX - origin.x - view.tx) / view.scale, y: (clientY - origin.y - view.ty) / view.scale };
}

/**
 * A client point in PDF points from the page's top-left: `slot` is the
 * page's client box (it already carries the pane's scroll offset) drawn at
 * CSS `scale`, where one point is `scale` pixels.
 */
export function screenToPage(clientX: number, clientY: number, slot: Point, scale: number): Point {
  return { x: (clientX - slot.x) / scale, y: (clientY - slot.y) / scale };
}

/**
 * The region two drag corners span (in either order), clipped to a
 * `width`×`height` image or page and snapped outward to whole units so every
 * pointed-at pixel is inside. Null when nothing of it is inside.
 */
export function dragRegion(a: Point, b: Point, width: number, height: number): Region | null {
  const clipped = clampRegion({ x: a.x, y: a.y, w: b.x - a.x, h: b.y - a.y }, width, height);
  if (clipped === null) return null;
  const x0 = Math.max(0, Math.floor(clipped.x));
  const y0 = Math.max(0, Math.floor(clipped.y));
  const x1 = Math.min(Math.ceil(width), Math.ceil(clipped.x + clipped.w));
  const y1 = Math.min(Math.ceil(height), Math.ceil(clipped.y + clipped.h));
  if (x1 <= x0 || y1 <= y0) return null;
  return { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
}

/**
 * The raster a crop of `r` is drawn at: `factor` output pixels per unit (2
 * for a PDF, rendered crisp from its vectors; 1 for a raster image, whose
 * pixels are the truth), shrunk so the long side stays within `cap` (past
 * 1568 px a model downsamples anyway).
 */
export function cropSize(r: Region, factor: number, cap: number): { w: number; h: number; scale: number } {
  const scale = Math.min(factor, cap / Math.max(r.w, r.h));
  return {
    w: Math.max(1, Math.round(r.w * scale)),
    h: Math.max(1, Math.round(r.h * scale)),
    scale,
  };
}
