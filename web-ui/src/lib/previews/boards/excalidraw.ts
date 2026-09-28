/**
 * Excalidraw scenes (`.excalidraw`, `.excalidraw.json`) drawn to SVG with the
 * same hand-drawn engine Excalidraw uses (roughjs, seeded per element, with
 * Excalidraw's own stroke options) and perfect-freehand for pen strokes.
 *
 * Why not Excalidraw's own `exportToSvg`: @excalidraw/utils ships as one
 * 19.6 MB module (14 MB gzipped) with every font subset inlined for export
 * subsetting; the full @excalidraw/excalidraw package needs React and loads
 * fonts from a CDN. This renderer is ~11 KB gzipped plus roughjs. Text uses
 * the drawing's font names with system fallbacks (Virgil → a handwriting
 * face where one is installed); nothing is ever fetched, and embedded images
 * are drawn only from the file's own data URLs.
 *
 * The geometry helpers are pure (tested); `renderExcalidraw` needs a DOM.
 */

import rough from "roughjs";
import { getStroke } from "perfect-freehand";
import { paperColor, safeColor, unionRects, type Rect } from "./format";

type Point = [number, number];

export interface ExElement {
  id: string;
  type: string;
  x: number;
  y: number;
  width: number;
  height: number;
  angle: number;
  strokeColor: string;
  backgroundColor: string;
  fillStyle: string;
  strokeWidth: number;
  strokeStyle: string;
  roughness: number;
  opacity: number;
  seed: number;
  roundness: { type: number; value?: number } | null;
  points: Point[];
  pressures: number[];
  simulatePressure: boolean;
  startArrowhead: string | null;
  endArrowhead: string | null;
  text: string;
  fontSize: number;
  fontFamily: number;
  textAlign: string;
  verticalAlign: string;
  lineHeight: number | null;
  containerId: string | null;
  frameId: string | null;
  name: string | null;
  fileId: string | null;
  scale: Point;
  crop: { x: number; y: number; width: number; height: number; naturalWidth: number; naturalHeight: number } | null;
  link: string | null;
  boundText: string[];
  elbowed: boolean;
}

export interface ExScene {
  elements: ExElement[];
  /** fileId → a checked `data:image/…` URL. */
  files: Map<string, string>;
  background: string;
  bounds: Rect;
  /** Elements drawn as a placeholder (embeds) or skipped (unknown kinds). */
  skipped: number;
}

const KNOWN = new Set([
  "rectangle",
  "diamond",
  "ellipse",
  "line",
  "arrow",
  "freedraw",
  "text",
  "image",
  "frame",
  "magicframe",
  "embeddable",
  "iframe",
]);
const MAX_ELEMENTS = 20_000;
const DATA_IMAGE = /^data:image\/(png|jpe?g|gif|webp|svg\+xml|avif|bmp);base64,[a-z0-9+/=\s]+$/i;

const n = (v: unknown, d = 0): number => (typeof v === "number" && Number.isFinite(v) ? v : d);
const s = (v: unknown, d = ""): string => (typeof v === "string" ? v : d);

function points(v: unknown): Point[] {
  if (!Array.isArray(v)) return [];
  const out: Point[] = [];
  for (const p of v.slice(0, 50_000)) {
    if (Array.isArray(p) && Number.isFinite(p[0]) && Number.isFinite(p[1])) out.push([p[0], p[1]]);
  }
  return out;
}

export function parseExcalidraw(text: string): ExScene {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    throw new Error("this drawing isn't valid JSON");
  }
  const obj = (raw ?? {}) as { type?: unknown; elements?: unknown; appState?: unknown; files?: unknown };
  if (!Array.isArray(obj.elements)) throw new Error("this isn't an Excalidraw drawing (no elements)");
  const elements: ExElement[] = [];
  let skipped = 0;
  for (const r of obj.elements.slice(0, MAX_ELEMENTS) as Record<string, unknown>[]) {
    if (r === null || typeof r !== "object" || r.isDeleted === true) continue;
    const type = s(r.type);
    if (!KNOWN.has(type)) {
      if (type !== "selection") skipped += 1;
      continue;
    }
    const roundness = r.roundness as { type?: unknown; value?: unknown } | null | undefined;
    const bound = Array.isArray(r.boundElements) ? (r.boundElements as { type?: unknown; id?: unknown }[]) : [];
    const scale = Array.isArray(r.scale) ? (r.scale as unknown[]) : [1, 1];
    const crop = r.crop as Record<string, unknown> | null | undefined;
    elements.push({
      id: s(r.id),
      type,
      x: n(r.x),
      y: n(r.y),
      width: n(r.width),
      height: n(r.height),
      angle: n(r.angle),
      strokeColor: safeColor(r.strokeColor) ?? "#1e1e1e",
      backgroundColor: safeColor(r.backgroundColor) ?? "transparent",
      fillStyle: s(r.fillStyle, "hachure"),
      strokeWidth: Math.min(64, Math.max(0.1, n(r.strokeWidth, 1))),
      strokeStyle: s(r.strokeStyle, "solid"),
      roughness: Math.min(5, Math.max(0, n(r.roughness, 1))),
      opacity: Math.min(100, Math.max(0, n(r.opacity, 100))),
      seed: Math.trunc(n(r.seed, 1)) || 1,
      roundness:
        roundness !== null && typeof roundness === "object"
          ? { type: n(roundness.type, 2), value: typeof roundness.value === "number" ? roundness.value : undefined }
          : null,
      points: points(r.points),
      pressures: Array.isArray(r.pressures) ? (r.pressures as unknown[]).map((p) => n(p, 0.5)) : [],
      simulatePressure: r.simulatePressure !== false,
      startArrowhead: typeof r.startArrowhead === "string" ? r.startArrowhead : null,
      endArrowhead: typeof r.endArrowhead === "string" ? r.endArrowhead : r.type === "arrow" && r.endArrowhead === undefined ? "arrow" : null,
      text: s(r.text).slice(0, 100_000),
      fontSize: Math.min(1000, Math.max(1, n(r.fontSize, 20))),
      fontFamily: Math.trunc(n(r.fontFamily, 1)),
      textAlign: s(r.textAlign, "left"),
      verticalAlign: s(r.verticalAlign, "top"),
      lineHeight: typeof r.lineHeight === "number" && r.lineHeight > 0 ? r.lineHeight : null,
      containerId: typeof r.containerId === "string" ? r.containerId : null,
      frameId: typeof r.frameId === "string" ? r.frameId : null,
      name: typeof r.name === "string" ? r.name.slice(0, 200) : null,
      fileId: typeof r.fileId === "string" ? r.fileId : null,
      scale: [scale[0] === -1 ? -1 : 1, scale[1] === -1 ? -1 : 1],
      crop:
        crop !== null && typeof crop === "object" && n(crop.width) > 0 && n(crop.naturalWidth) > 0
          ? {
              x: n(crop.x),
              y: n(crop.y),
              width: n(crop.width),
              height: n(crop.height),
              naturalWidth: n(crop.naturalWidth),
              naturalHeight: n(crop.naturalHeight),
            }
          : null,
      link: typeof r.link === "string" ? r.link : null,
      boundText: bound.filter((b) => b?.type === "text" && typeof b.id === "string").map((b) => b.id as string),
      elbowed: r.elbowed === true,
    });
  }
  const files = new Map<string, string>();
  if (obj.files !== null && typeof obj.files === "object") {
    for (const [id, f] of Object.entries(obj.files as Record<string, { dataURL?: unknown }>)) {
      const url = f?.dataURL;
      if (typeof url === "string" && DATA_IMAGE.test(url.slice(0, 64)) && !/[^a-z0-9+/=\s]/i.test(url.slice(url.indexOf(",") + 1, url.indexOf(",") + 4097))) {
        files.set(id, url);
      }
    }
  }
  const appState = (obj.appState ?? {}) as { viewBackgroundColor?: unknown };
  const background = paperColor(appState.viewBackgroundColor);
  const bounds = unionRects(elements.map(elementBounds), 16) ?? { x: 0, y: 0, w: 100, h: 100 };
  return { elements, files, background, bounds, skipped };
}

/** An element's unrotated box in scene coordinates (linear kinds from their points). */
export function elementBox(el: ExElement): Rect {
  if ((el.type === "line" || el.type === "arrow" || el.type === "freedraw") && el.points.length > 0) {
    let x0 = Infinity;
    let y0 = Infinity;
    let x1 = -Infinity;
    let y1 = -Infinity;
    for (const [px, py] of el.points) {
      x0 = Math.min(x0, px);
      y0 = Math.min(y0, py);
      x1 = Math.max(x1, px);
      y1 = Math.max(y1, py);
    }
    return { x: el.x + x0, y: el.y + y0, w: x1 - x0, h: y1 - y0 };
  }
  return { x: el.x, y: el.y, w: el.width, h: el.height };
}

/** The axis-aligned bounds of an element as drawn (rotation included), with
 *  room for its stroke. */
export function elementBounds(el: ExElement): Rect {
  const b = elementBox(el);
  const pad = el.strokeWidth * 2 + (el.type === "arrow" ? 16 : 0);
  if (el.angle === 0) return { x: b.x - pad, y: b.y - pad, w: b.w + pad * 2, h: b.h + pad * 2 };
  const cx = b.x + b.w / 2;
  const cy = b.y + b.h / 2;
  const cos = Math.cos(el.angle);
  const sin = Math.sin(el.angle);
  const corners: Point[] = [
    [b.x, b.y],
    [b.x + b.w, b.y],
    [b.x + b.w, b.y + b.h],
    [b.x, b.y + b.h],
  ].map(([x, y]) => [cx + (x - cx) * cos - (y - cy) * sin, cy + (x - cx) * sin + (y - cy) * cos]);
  const xs = corners.map((c) => c[0]);
  const ys = corners.map((c) => c[1]);
  const x0 = Math.min(...xs);
  const y0 = Math.min(...ys);
  return { x: x0 - pad, y: y0 - pad, w: Math.max(...xs) - x0 + pad * 2, h: Math.max(...ys) - y0 + pad * 2 };
}

/** Excalidraw's corner radius for a shape side of `size`. */
export function cornerRadius(size: number, roundness: ExElement["roundness"]): number {
  if (roundness === null) return 0;
  if (roundness.type === 1 || roundness.type === 2) return size * 0.25;
  if (roundness.type === 3) {
    const fixed = roundness.value ?? 32;
    return size <= fixed / 0.25 ? size * 0.25 : fixed;
  }
  return 0;
}

/** Excalidraw keeps small shapes from looking scribbled: less roughness. */
export function adjustRoughness(el: ExElement): number {
  const max = Math.max(el.width, el.height);
  const min = Math.min(el.width, el.height);
  const linear = el.type === "line" || el.type === "arrow";
  if ((min >= 20 && max >= 50) || (min >= 15 && el.roundness !== null && !linear) || (linear && max >= 50)) {
    return el.roughness;
  }
  return Math.min(el.roughness / (max < 10 ? 3 : 2), 2.5);
}

const ARROWHEAD_SIZE: Record<string, number> = {
  arrow: 25,
  diamond: 12,
  diamond_outline: 12,
  crowfoot_many: 20,
  crowfoot_one: 20,
  crowfoot_one_or_many: 20,
};

/**
 * Arrowhead geometry after Excalidraw's `getArrowheadPoints`, taking the
 * direction from the end segment: `[tipX, tipY, …]` — for "dot"/"circle",
 * `[cx, cy, diameter]`; for diamonds four points; otherwise tip and two wings.
 */
export function arrowheadPoints(
  pts: readonly Point[],
  position: "start" | "end",
  arrowhead: string,
  strokeWidth: number,
): number[] | null {
  if (pts.length < 2) return null;
  const tip = position === "end" ? pts[pts.length - 1] : pts[0];
  const prev = position === "end" ? pts[pts.length - 2] : pts[1];
  const dist = Math.hypot(tip[0] - prev[0], tip[1] - prev[1]);
  if (dist === 0) return null;
  const nx = (tip[0] - prev[0]) / dist;
  const ny = (tip[1] - prev[1]) / dist;
  const size = ARROWHEAD_SIZE[arrowhead] ?? 15;
  const lengthMultiplier = arrowhead === "diamond" || arrowhead === "diamond_outline" ? 0.25 : 0.5;
  const minSize = Math.min(size, dist * lengthMultiplier);
  const xs = tip[0] - nx * minSize;
  const ys = tip[1] - ny * minSize;
  if (arrowhead === "dot" || arrowhead === "circle" || arrowhead === "circle_outline") {
    return [tip[0], tip[1], Math.hypot(ys - tip[1], xs - tip[0]) + strokeWidth - 2];
  }
  const angle = ((arrowhead === "bar" ? 90 : arrowhead === "arrow" ? 20 : 25) * Math.PI) / 180;
  const rot = (a: number): Point => {
    const cos = Math.cos(a);
    const sin = Math.sin(a);
    return [tip[0] + (xs - tip[0]) * cos - (ys - tip[1]) * sin, tip[1] + (xs - tip[0]) * sin + (ys - tip[1]) * cos];
  };
  const [x3, y3] = rot(-angle);
  const [x4, y4] = rot(angle);
  if (arrowhead === "diamond" || arrowhead === "diamond_outline") {
    return [tip[0], tip[1], x3, y3, tip[0] - nx * minSize * 2, tip[1] - ny * minSize * 2, x4, y4];
  }
  return [tip[0], tip[1], x3, y3, x4, y4];
}

/** Font stacks by Excalidraw font id: the drawing's own face first (used
 *  when installed), then system look-alikes. Never a web font. */
export function fontStack(id: number): string {
  switch (id) {
    case 2:
      return 'Helvetica, "Helvetica Neue", Arial, sans-serif';
    case 3:
      return '"Cascadia Code", "Cascadia Mono", ui-monospace, Menlo, Consolas, monospace';
    case 6:
      return 'Nunito, "Segoe UI", system-ui, sans-serif';
    case 7:
      return '"Lilita One", "Arial Rounded MT Bold", system-ui, sans-serif';
    case 8:
      return '"Comic Shanns", "Comic Sans MS", "Comic Neue", system-ui, sans-serif';
    case 9:
      return '"Liberation Sans", Arial, sans-serif';
    default:
      return 'Excalifont, Virgil, "Segoe Print", "Bradley Hand", "Chalkboard SE", "Comic Sans MS", "Comic Neue", system-ui, sans-serif';
  }
}

/** [unitsPerEm, ascender, descender] per font, for baseline placement. */
const METRICS: Record<number, [number, number, number]> = {
  1: [1000, 886, -374],
  2: [2048, 1577, -471],
  3: [2048, 1900, -480],
  5: [1000, 886, -374],
  6: [1000, 1011, -353],
  7: [1000, 923, -220],
  8: [1000, 750, -250],
  9: [2048, 1854, -434],
};

export function defaultLineHeight(font: number): number {
  return font === 2 ? 1.15 : font === 3 ? 1.2 : 1.25;
}

/** The first line's baseline below the text box's top, as Excalidraw places it. */
export function baselineOffset(font: number, fontSize: number, lineHeightPx: number): number {
  const [upm, asc, desc] = METRICS[font] ?? METRICS[1];
  const em = fontSize / upm;
  const gap = (lineHeightPx - em * asc + em * desc) / 2;
  return em * asc + gap;
}

/** perfect-freehand's outline as an SVG path (Excalidraw's own conversion). */
export function freedrawPath(el: ExElement): string {
  const input: number[][] = el.simulatePressure
    ? el.points
    : el.points.length > 0
      ? el.points.map(([x, y], i) => [x, y, el.pressures[i] ?? 0.5])
      : [[0, 0, 0.5]];
  const stroke = getStroke(input, {
    simulatePressure: el.simulatePressure,
    size: el.strokeWidth * 4.25,
    thinning: 0.6,
    smoothing: 0.5,
    streamline: 0.5,
    easing: (t) => Math.sin((t * Math.PI) / 2),
    last: true,
  });
  if (stroke.length === 0) return "";
  const r = (v: number) => Math.round(v * 100) / 100;
  const med = (a: number[], b: number[]) => [(a[0] + b[0]) / 2, (a[1] + b[1]) / 2];
  const parts: string[] = [`M${r(stroke[0][0])} ${r(stroke[0][1])}Q`];
  for (let i = 0; i < stroke.length; i++) {
    const p = stroke[i];
    const q = med(p, stroke[(i + 1) % stroke.length]);
    parts.push(`${r(p[0])} ${r(p[1])} ${r(q[0])} ${r(q[1])}`);
  }
  parts.push("Z");
  return parts.join(" ");
}

function isLoop(pts: readonly Point[], strokeWidth: number): boolean {
  if (pts.length < 3) return false;
  const [a, b] = [pts[0], pts[pts.length - 1]];
  return Math.hypot(a[0] - b[0], a[1] - b[1]) <= Math.max(8, strokeWidth * 2);
}

// --- rendering (DOM) ---------------------------------------------------------------

const NS = "http://www.w3.org/2000/svg";
let renderSeq = 0;

type Options = Parameters<ReturnType<typeof rough.generator>["rectangle"]>[4] & object;

function roughOptions(el: ExElement, continuous = false): Options {
  const dashed = el.strokeStyle === "dashed";
  const dotted = el.strokeStyle === "dotted";
  const o: Options = {
    seed: el.seed,
    strokeLineDash: dashed ? [8, 8 + el.strokeWidth] : dotted ? [1.5, 6 + el.strokeWidth] : undefined,
    disableMultiStroke: el.strokeStyle !== "solid",
    strokeWidth: el.strokeStyle !== "solid" ? el.strokeWidth + 0.5 : el.strokeWidth,
    fillWeight: el.strokeWidth / 2,
    hachureGap: el.strokeWidth * 4,
    roughness: adjustRoughness(el),
    stroke: el.strokeColor,
    preserveVertices: continuous || el.roughness < 2,
  };
  const fill = el.backgroundColor === "transparent" ? undefined : el.backgroundColor;
  if (el.type === "rectangle" || el.type === "diamond" || el.type === "ellipse" || el.type === "embeddable" || el.type === "iframe") {
    o.fillStyle = el.fillStyle;
    o.fill = fill;
    if (el.type === "ellipse") o.curveFitting = 1;
  } else if ((el.type === "line" || el.type === "freedraw") && isLoop(el.points, el.strokeWidth)) {
    o.fillStyle = el.fillStyle;
    o.fill = fill;
  }
  return o;
}

function el<K extends keyof SVGElementTagNameMap>(doc: Document, tag: K, attrs: Record<string, string | number> = {}): SVGElementTagNameMap[K] {
  const node = doc.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, String(v));
  return node;
}

/** Draw a scene as one SVG in scene coordinates (viewBox = its bounds). */
export function renderExcalidraw(scene: ExScene, doc: Document = document): SVGSVGElement {
  const { bounds: b } = scene;
  const svg = el(doc, "svg", {
    xmlns: NS,
    width: b.w,
    height: b.h,
    viewBox: `${b.x} ${b.y} ${b.w} ${b.h}`,
  });
  const idp = `ex${++renderSeq}-`;
  const defs = el(doc, "defs");
  svg.appendChild(defs);
  svg.appendChild(el(doc, "rect", { x: b.x, y: b.y, width: b.w, height: b.h, fill: scene.background, class: "ex-paper" }));
  const rc = rough.svg(svg);
  const byId = new Map(scene.elements.map((e) => [e.id, e]));
  const frames = new Set(scene.elements.filter((e) => e.type === "frame" || e.type === "magicframe").map((e) => e.id));
  for (const f of frames) {
    const fr = byId.get(f)!;
    const clip = el(doc, "clipPath", { id: `${idp}clip-${frames.size > 0 ? [...frames].indexOf(f) : 0}` });
    clip.appendChild(el(doc, "rect", { x: fr.x, y: fr.y, width: fr.width, height: fr.height }));
    defs.appendChild(clip);
  }
  const frameIndex = new Map([...frames].map((f, i) => [f, i]));

  for (const e of scene.elements) {
    let node: SVGElement | null;
    try {
      node = drawElement(e, doc, rc, scene, byId, idp, defs);
    } catch {
      node = null;
    }
    if (node === null) continue;
    if (e.opacity < 100) node.setAttribute("opacity", String(e.opacity / 100));
    const fi = e.frameId !== null ? frameIndex.get(e.frameId) : undefined;
    if (fi !== undefined) {
      const g = el(doc, "g", { "clip-path": `url(#${idp}clip-${fi})` });
      g.appendChild(node);
      svg.appendChild(g);
    } else svg.appendChild(node);
  }
  return svg;
}

function place(g: SVGElement, e: ExElement, box: Rect): void {
  const cx = box.x + box.w / 2 - e.x;
  const cy = box.y + box.h / 2 - e.y;
  const deg = (e.angle * 180) / Math.PI;
  g.setAttribute(
    "transform",
    deg === 0 ? `translate(${e.x} ${e.y})` : `translate(${e.x} ${e.y}) rotate(${deg} ${cx} ${cy})`,
  );
}

function drawElement(
  e: ExElement,
  doc: Document,
  rc: ReturnType<typeof rough.svg>,
  scene: ExScene,
  byId: Map<string, ExElement>,
  idp: string,
  defs: SVGDefsElement,
): SVGElement | null {
  const gen = rc.generator;
  const g = el(doc, "g");
  place(g, e, elementBox(e));
  const w = e.width;
  const h = e.height;
  switch (e.type) {
    case "rectangle":
    case "embeddable":
    case "iframe": {
      const r = cornerRadius(Math.min(w, h), e.roundness);
      const shape =
        r > 0
          ? gen.path(
              `M ${r} 0 L ${w - r} 0 Q ${w} 0, ${w} ${r} L ${w} ${h - r} Q ${w} ${h}, ${w - r} ${h} L ${r} ${h} Q 0 ${h}, 0 ${h - r} L 0 ${r} Q 0 0, ${r} 0`,
              roughOptions(e, true),
            )
          : gen.rectangle(0, 0, w, h, roughOptions(e));
      g.appendChild(rc.draw(shape));
      if (e.type !== "rectangle") {
        // An embed is never loaded: say what it was.
        const t = el(doc, "text", { x: w / 2, y: h / 2, "text-anchor": "middle", "dominant-baseline": "middle", fill: e.strokeColor, "font-size": 14, "font-family": fontStack(2) });
        t.textContent = e.link !== null ? `embedded: ${e.link.slice(0, 80)}` : "embedded content";
        g.appendChild(t);
      }
      return g;
    }
    case "diamond": {
      const tx = Math.floor(w / 2) + 1;
      const ry = Math.floor(h / 2) + 1;
      const pts: Point[] = [
        [tx, 0],
        [w, ry],
        [tx, h],
        [0, ry],
      ];
      let shape;
      if (e.roundness !== null) {
        const vr = cornerRadius(Math.abs(tx), e.roundness);
        const hr = cornerRadius(Math.abs(ry), e.roundness);
        shape = gen.path(
          `M ${tx + vr} ${hr} L ${w - vr} ${ry - hr} C ${w} ${ry}, ${w} ${ry}, ${w - vr} ${ry + hr} L ${tx + vr} ${h - hr} C ${tx} ${h}, ${tx} ${h}, ${tx - vr} ${h - hr} L ${vr} ${ry + hr} C 0 ${ry}, 0 ${ry}, ${vr} ${ry - hr} L ${tx - vr} ${hr} C ${tx} 0, ${tx} 0, ${tx + vr} ${hr}`,
          roughOptions(e, true),
        );
      } else shape = gen.polygon(pts, roughOptions(e));
      g.appendChild(rc.draw(shape));
      return g;
    }
    case "ellipse":
      g.appendChild(rc.draw(gen.ellipse(w / 2, h / 2, w, h, roughOptions(e))));
      return g;
    case "line":
    case "arrow": {
      const pts = e.points.length >= 2 ? e.points : ([[0, 0], [w, h]] as Point[]);
      const opts = roughOptions(e);
      const shape = e.roundness !== null && !e.elbowed && pts.length > 2 ? gen.curve(pts, opts) : gen.linearPath(pts, opts);
      const lineG = el(doc, "g");
      lineG.appendChild(rc.draw(shape));
      if (e.type === "arrow") {
        for (const [pos, head] of [
          ["start", e.startArrowhead],
          ["end", e.endArrowhead],
        ] as const) {
          if (head === null) continue;
          for (const d of arrowheadShapes(e, pts, pos, head, gen, scene.background)) lineG.appendChild(rc.draw(d));
        }
      }
      // An arrow's label sits on a gap in the line (Excalidraw masks it).
      const label = e.boundText.map((id) => byId.get(id)).find((t) => t !== undefined && t.type === "text");
      if (label !== undefined) {
        const mask = el(doc, "mask", { id: `${idp}mask-${e.id.replace(/[^\w-]/g, "")}`, maskUnits: "userSpaceOnUse" });
        const box = elementBox(e);
        mask.appendChild(el(doc, "rect", { x: box.x - e.x - 100, y: box.y - e.y - 100, width: box.w + 200, height: box.h + 200, fill: "#fff" }));
        mask.appendChild(el(doc, "rect", { x: label.x - e.x - 4, y: label.y - e.y - 4, width: label.width + 8, height: label.height + 8, fill: "#000" }));
        defs.appendChild(mask);
        lineG.setAttribute("mask", `url(#${mask.id})`);
      }
      g.appendChild(lineG);
      return g;
    }
    case "freedraw": {
      if (e.backgroundColor !== "transparent" && isLoop(e.points, e.strokeWidth)) {
        g.appendChild(rc.draw(gen.curve(e.points, { ...roughOptions(e), stroke: "none" })));
      }
      const d = freedrawPath(e);
      if (d !== "") g.appendChild(el(doc, "path", { d, fill: e.strokeColor }));
      return g;
    }
    case "text": {
      if (e.text === "") return null;
      const lh = e.fontSize * (e.lineHeight ?? defaultLineHeight(e.fontFamily));
      const x = e.textAlign === "center" ? w / 2 : e.textAlign === "right" ? w : 0;
      const top = baselineOffset(e.fontFamily, e.fontSize, lh);
      const rtl = /[֐-ࣿיִ-﷿ﹰ-﻿]/.test(e.text);
      const anchor = e.textAlign === "center" ? "middle" : e.textAlign === "right" || rtl ? "end" : "start";
      e.text
        .replace(/\r\n?/g, "\n")
        .split("\n")
        .forEach((line, i) => {
          const t = el(doc, "text", {
            x,
            y: i * lh + top,
            "font-family": fontStack(e.fontFamily),
            "font-size": `${e.fontSize}px`,
            fill: e.strokeColor,
            "text-anchor": anchor,
            direction: rtl ? "rtl" : "ltr",
            "dominant-baseline": "alphabetic",
            style: "white-space: pre;",
          });
          t.textContent = line;
          g.appendChild(t);
        });
      return g;
    }
    case "image": {
      const url = e.fileId !== null ? scene.files.get(e.fileId) : undefined;
      if (url === undefined) {
        g.appendChild(rc.draw(gen.rectangle(0, 0, w, h, { ...roughOptions(e), fill: undefined, strokeLineDash: [6, 6] })));
        return g;
      }
      const inner = el(doc, "g", { class: "ex-img" });
      const [sx, sy] = e.scale;
      if (sx !== 1 || sy !== 1) inner.setAttribute("transform", `translate(${sx < 0 ? w : 0} ${sy < 0 ? h : 0}) scale(${sx} ${sy})`);
      let img: SVGElement;
      if (e.crop !== null) {
        const c = e.crop;
        img = el(doc, "svg", { x: 0, y: 0, width: w, height: h, viewBox: `${c.x} ${c.y} ${c.width} ${c.height}`, preserveAspectRatio: "none" });
        const im = el(doc, "image", { width: c.naturalWidth, height: c.naturalHeight, preserveAspectRatio: "none" });
        im.setAttribute("href", url);
        img.appendChild(im);
      } else {
        img = el(doc, "image", { width: w, height: h, preserveAspectRatio: "none" });
        img.setAttribute("href", url);
      }
      const r = cornerRadius(Math.min(w, h), e.roundness);
      if (r > 0) {
        const clip = el(doc, "clipPath", { id: `${idp}img-${e.id.replace(/[^\w-]/g, "")}` });
        clip.appendChild(el(doc, "rect", { width: w, height: h, rx: r, ry: r }));
        defs.appendChild(clip);
        inner.setAttribute("clip-path", `url(#${clip.id})`);
      }
      inner.appendChild(img);
      g.appendChild(inner);
      return g;
    }
    case "frame":
    case "magicframe": {
      g.appendChild(el(doc, "rect", { x: 0, y: 0, width: w, height: h, rx: 8, ry: 8, fill: "none", stroke: "#bbb", "stroke-width": 1 }));
      const t = el(doc, "text", { x: 0, y: -6, fill: "#999", "font-size": 14, "font-family": fontStack(2) });
      t.textContent = e.name ?? "Frame";
      g.appendChild(t);
      return g;
    }
  }
  return null;
}

function arrowheadShapes(
  e: ExElement,
  pts: readonly Point[],
  position: "start" | "end",
  head: string,
  gen: ReturnType<typeof rough.generator>,
  paper: string,
) {
  const p = arrowheadPoints(pts, position, head, e.strokeWidth);
  if (p === null) return [];
  const base = { ...roughOptions(e) };
  delete base.strokeLineDash;
  const filled = !head.endsWith("_outline");
  const fill = filled ? e.strokeColor : paper;
  if (head === "dot" || head === "circle" || head === "circle_outline") {
    return [gen.circle(p[0], p[1], p[2], { ...base, fill, fillStyle: "solid", roughness: Math.min(0.5, base.roughness ?? 0) })];
  }
  if (head === "triangle" || head === "triangle_outline") {
    return [
      gen.polygon(
        [
          [p[0], p[1]],
          [p[2], p[3]],
          [p[4], p[5]],
        ],
        { ...base, fill, fillStyle: "solid", roughness: Math.min(1, base.roughness ?? 0) },
      ),
    ];
  }
  if (head === "diamond" || head === "diamond_outline") {
    return [
      gen.polygon(
        [
          [p[0], p[1]],
          [p[2], p[3]],
          [p[4], p[5]],
          [p[6], p[7]],
        ],
        { ...base, fill, fillStyle: "solid", roughness: Math.min(1, base.roughness ?? 0) },
      ),
    ];
  }
  const o = { ...base, roughness: Math.min(1, base.roughness ?? 0) };
  if (e.strokeStyle === "dotted") o.strokeLineDash = [1.5, 6 + e.strokeWidth - 2];
  return [gen.line(p[2], p[3], p[0], p[1], o), gen.line(p[4], p[5], p[0], p[1], o)];
}
