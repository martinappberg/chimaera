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
