/**
 * Pointing at part of an HTML page shown in a native pane view (the area
 * tool in `HtmlView.svelte`). The user draws on a snapshot of the view's
 * viewport, laid where the (frozen) view was; these turn that drawing into
 * view px, snapshot pixels and a document locator. Pure, so the arithmetic
 * is unit-tested.
 */

import { cropSize, dragRegion, type Point, type Region } from "./imageRegion";
import { xywhFragment } from "../shared/locator";

/** A width × height, CSS px. */
export interface Size {
  w: number;
  h: number;
}

/**
 * A point on the snapshot as displayed (`shown`, px from its top-left) in
 * the view's viewport px (`view`, the snapshot's own CSS size). The two
 * differ only when the pane was resized after the snapshot was taken.
 */
export function shownToView(p: Point, shown: Size, view: Size): Point {
  const sx = shown.w > 0 ? view.w / shown.w : 1;
  const sy = shown.h > 0 ? view.h / shown.h : 1;
  return { x: p.x * sx, y: p.y * sy };
}

/** The box two drag corners span in view px, whole px, clipped to the
 *  viewport; null when nothing of it is inside. */
export function viewBox(a: Point, b: Point, view: Size): Region | null {
  return dragRegion(a, b, view.w, view.h);
}

/** A view-px box as displayed (the mark drawn over the snapshot). */
export function viewToShown(r: Region, shown: Size, view: Size): Region {
  const sx = view.w > 0 ? shown.w / view.w : 1;
  const sy = view.h > 0 ? shown.h / view.h : 1;
  return { x: r.x * sx, y: r.y * sy, w: r.w * sx, h: r.h * sy };
}

/**
 * Which snapshot pixels a view-px box covers: the PNG is the viewport at
 * the screen's density (`png` natural size over `view` CSS size), so a box
 * scales by that before cropping. Clipped to the PNG.
 */
export function snapshotSource(r: Region, view: Size, png: Size): Region | null {
  if (view.w <= 0 || view.h <= 0 || png.w <= 0 || png.h <= 0) return null;
  const kx = png.w / view.w;
  const ky = png.h / view.h;
  const x0 = Math.max(0, Math.floor(r.x * kx));
  const y0 = Math.max(0, Math.floor(r.y * ky));
  const x1 = Math.min(png.w, Math.ceil((r.x + r.w) * kx));
  const y1 = Math.min(png.h, Math.ceil((r.y + r.h) * ky));
  if (x1 <= x0 || y1 <= y0) return null;
  return { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
}

/** The crop's output raster: the snapshot's own pixels (1×), the long side
 *  capped where a model downsamples anyway. */
export function cropRaster(src: Region, cap: number): { w: number; h: number } {
  const size = cropSize(src, 1, cap);
  return { w: size.w, h: size.h };
}

/** The locator of a view-px box in the document (`xywh=` in CSS px from
 *  the page's top-left): the box moved by where the page was scrolled. */
export function pageFragment(r: Region, scrollX: number, scrollY: number): string {
  const dx = Number.isFinite(scrollX) ? scrollX : 0;
  const dy = Number.isFinite(scrollY) ? scrollY : 0;
  return xywhFragment({ x: r.x + dx, y: r.y + dy, w: r.w, h: r.h });
}
