/**
 * draw.io / diagrams.net files (`.drawio`, `.dio`): reading the file, and the
 * geometry of the common subset this viewer draws. Pure (tested); the SVG is
 * built in drawioSvg.ts.
 *
 * A file is an `<mxfile>` of `<diagram>` pages whose content is either a
 * plain `<mxGraphModel>` or that model URI-encoded, raw-deflated and base64'd
 * (draw.io's default "compressed" save). A bare `<mxGraphModel>` file is one
 * page. Cells are vertices and edges with a style string; child geometry is
 * relative to its container. Edges store only the waypoints a person set, so
 * their routes are computed here, as draw.io would draw them for the common
 * styles: straight, orthogonal (with or without waypoints and fixed ports),
 * elbow, and entity-relation.
 *
 * No renderer on npm fits: the diagrams.net viewer is a CDN script, mxGraph
 * and maxGraph (~115 KB gzipped) don't know draw.io's own shapes or its HTML
 * label sanitizing, and the lighter converters are GPL. This covers what
 * people and agents actually draw; stencil shapes it doesn't know draw as a
 * labelled box and the bar says how many.
 */

import { findAll, findFirst, parseXml, type XmlNode } from "./xml";
import type { Rect } from "./format";

export interface Pt {
  x: number;
  y: number;
}

export interface DrawioPage {
  name: string;
  model: XmlNode | null;
  /** Why the page couldn't be read (a corrupt compressed page, say). */
  error: string | null;
}

/** base64 → raw inflate → URI decode: draw.io's compressed page content. */
export async function inflateDiagram(data: string): Promise<string> {
  const bin = atob(data.replace(/\s+/g, ""));
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  const stream = new Blob([bytes]).stream().pipeThrough(new DecompressionStream("deflate-raw"));
  const text = await new Response(stream).text();
  try {
    return decodeURIComponent(text);
  } catch {
    return text;
  }
}

export async function readDrawio(text: string): Promise<DrawioPage[]> {
  const doc = parseXml(text);
  const bare = doc.children.find((c) => c.name === "mxGraphModel");
  if (bare !== undefined) return [{ name: "Page-1", model: bare, error: null }];
  const file = doc.children.find((c) => c.name === "mxfile");
  if (file === undefined) throw new Error("this isn't a draw.io file (no <mxfile> or <mxGraphModel>)");
  const pages: DrawioPage[] = [];
  for (const [i, d] of findAll(file, "diagram").entries()) {
    const name = d.attrs.name ?? `Page-${i + 1}`;
    const inline = findFirst(d, "mxGraphModel");
    if (inline !== null) {
      pages.push({ name, model: inline, error: null });
      continue;
    }
    const data = d.text.trim();
    if (data === "") {
      pages.push({ name, model: null, error: "this page is empty" });
      continue;
    }
    try {
      const xml = await inflateDiagram(data);
      const model = findFirst(parseXml(xml), "mxGraphModel");
      pages.push({ name, model, error: model === null ? "this page has no diagram" : null });
    } catch {
      pages.push({ name, model: null, error: "this page's compressed content couldn't be read" });
    }
  }
  if (pages.length === 0) throw new Error("this draw.io file has no pages");
  return pages;
}

// --- styles -----------------------------------------------------------------------

export type Style = Record<string, string>;

/** draw.io's named styles (its default stylesheet) for the tokens people use. */
const NAMED: Record<string, Style> = {
  text: { fillColor: "none", strokeColor: "none", gradientColor: "none", align: "left", verticalAlign: "top" },
  edgeLabel: { fillColor: "none", strokeColor: "none", labelBackgroundColor: "default", fontSize: "11" },
  label: { fontStyle: "1", align: "left", verticalAlign: "middle", spacing: "2", spacingLeft: "52", imageWidth: "42", imageHeight: "42", rounded: "1" },
  icon: { shape: "label", align: "center", verticalAlign: "top", verticalLabelPosition: "bottom" },
  swimlane: { shape: "swimlane", fontStyle: "1", startSize: "23", verticalAlign: "top", fillColor: "default" },
  group: { verticalAlign: "top", fillColor: "none", strokeColor: "none", gradientColor: "none" },
  ellipse: { shape: "ellipse", perimeter: "ellipsePerimeter" },
  rhombus: { shape: "rhombus", perimeter: "rhombusPerimeter" },
  triangle: { shape: "triangle", perimeter: "trianglePerimeter" },
  line: { shape: "line", strokeWidth: "4", labelBackgroundColor: "default", verticalAlign: "top", spacingTop: "8" },
  image: { shape: "image", labelBackgroundColor: "default", verticalAlign: "top", verticalLabelPosition: "bottom" },
  roundImage: { shape: "image", perimeter: "ellipsePerimeter", verticalAlign: "top", verticalLabelPosition: "bottom" },
  rhombusImage: { shape: "image", perimeter: "rhombusPerimeter", verticalAlign: "top", verticalLabelPosition: "bottom" },
  arrow: { shape: "arrow", edgeStyle: "none", fillColor: "default" },
};

const VERTEX_DEFAULTS: Style = {
  shape: "rectangle",
  fillColor: "default",
  strokeColor: "default",
  fontColor: "default",
  fontSize: "12",
  fontFamily: "Helvetica",
  align: "center",
  verticalAlign: "middle",
};

const EDGE_DEFAULTS: Style = {
  shape: "connector",
  strokeColor: "default",
  fontColor: "default",
  fontSize: "11",
  fontFamily: "Helvetica",
  align: "center",
  verticalAlign: "middle",
  endArrow: "classic",
  labelBackgroundColor: "default",
};

/** A cell style string → its effective style (defaults, then named styles in
 *  order, then its own `key=value` pairs). */
export function resolveStyle(raw: string, edge: boolean): Style {
  const out: Style = { ...(edge ? EDGE_DEFAULTS : VERTEX_DEFAULTS) };
  const own: Style = {};
  for (const tok of raw.split(";")) {
    const t = tok.trim();
    if (t === "") continue;
    const eq = t.indexOf("=");
    if (eq === -1) {
      Object.assign(out, NAMED[t] ?? {});
      continue;
    }
    own[t.slice(0, eq).trim()] = t.slice(eq + 1).trim();
  }
  return Object.assign(out, own);
}

export const num = (s: Style, key: string, d: number): number => {
  const v = Number.parseFloat(s[key] ?? "");
  return Number.isFinite(v) ? v : d;
};
export const flag = (s: Style, key: string, d = false): boolean => (s[key] === undefined ? d : s[key] === "1" || s[key] === "true");

// --- cells ------------------------------------------------------------------------

export interface Geo {
  x: number;
  y: number;
  w: number;
  h: number;
  relative: boolean;
  points: Pt[];
  sourcePoint: Pt | null;
  targetPoint: Pt | null;
  offset: Pt | null;
}

export interface Cell {
  id: string;
  parent: string | null;
  value: string;
  style: Style;
  vertex: boolean;
  edge: boolean;
  source: string | null;
  target: string | null;
  geo: Geo | null;
  visible: boolean;
  collapsed: boolean;
  children: Cell[];
}

export interface Model {
  cells: Map<string, Cell>;
  /** Top-level cells (the root's children: usually one layer, cell "1"). */
  roots: Cell[];
  background: string | null;
}

const MAX_CELLS = 50_000;

function numAttr(v: string | undefined, d = 0): number {
  const n = v === undefined ? NaN : Number.parseFloat(v);
  return Number.isFinite(n) ? n : d;
}

function point(n: XmlNode): Pt {
  return { x: numAttr(n.attrs.x), y: numAttr(n.attrs.y) };
}

function geometry(n: XmlNode | undefined): Geo | null {
  if (n === undefined) return null;
  const points: Pt[] = [];
  let sourcePoint: Pt | null = null;
  let targetPoint: Pt | null = null;
  let offset: Pt | null = null;
  for (const c of n.children) {
    if (c.name === "Array" && c.attrs.as === "points") {
      for (const p of c.children) if (p.name === "mxPoint") points.push(point(p));
    } else if (c.name === "mxPoint") {
      if (c.attrs.as === "sourcePoint") sourcePoint = point(c);
      else if (c.attrs.as === "targetPoint") targetPoint = point(c);
      else if (c.attrs.as === "offset") offset = point(c);
    }
  }
  return {
    x: numAttr(n.attrs.x),
    y: numAttr(n.attrs.y),
    w: Math.max(0, numAttr(n.attrs.width)),
    h: Math.max(0, numAttr(n.attrs.height)),
    relative: n.attrs.relative === "1",
    points: points.slice(0, 500),
    sourcePoint,
    targetPoint,
    offset,
  };
}

export function parseModel(model: XmlNode): Model {
  const root = model.children.find((c) => c.name === "root") ?? model;
  const cells = new Map<string, Cell>();
  const order: Cell[] = [];
  for (const node of root.children) {
    if (order.length >= MAX_CELLS) break;
    // Cells with custom properties are wrapped: <UserObject label=…><mxCell/></UserObject>.
    const wrapped = node.name === "UserObject" || node.name === "object";
    const mx = wrapped ? node.children.find((c) => c.name === "mxCell") : node.name === "mxCell" ? node : undefined;
    if (mx === undefined) continue;
    const id = (wrapped ? node.attrs.id : mx.attrs.id) ?? `cell-${order.length}`;
    if (cells.has(id)) continue;
    const edge = mx.attrs.edge === "1";
    const cell: Cell = {
      id,
      parent: mx.attrs.parent ?? null,
      value: (wrapped ? node.attrs.label : mx.attrs.value) ?? "",
      style: resolveStyle(mx.attrs.style ?? "", edge),
      vertex: mx.attrs.vertex === "1",
      edge,
      source: mx.attrs.source ?? null,
      target: mx.attrs.target ?? null,
      geo: geometry(mx.children.find((c) => c.name === "mxGeometry")),
      visible: mx.attrs.visible !== "0",
      collapsed: mx.attrs.collapsed === "1",
      children: [],
    };
    cells.set(id, cell);
    order.push(cell);
  }
  const roots: Cell[] = [];
  for (const c of order) {
    const p = c.parent === null ? undefined : cells.get(c.parent);
    if (p === undefined || p === c) roots.push(c);
    else p.children.push(c);
  }
  const bg = model.attrs.background;
  return { cells, roots, background: bg !== undefined && bg !== "none" ? bg : null };
}

/** The origin a cell's geometry is relative to: its container vertex's
 *  absolute corner (layers and the root contribute nothing). */
export function originOf(cell: Cell, cells: Map<string, Cell>): Pt {
  let x = 0;
  let y = 0;
  let p = cell.parent === null ? undefined : cells.get(cell.parent);
  for (let guard = 0; p !== undefined && guard < 64; guard++) {
    if (p.vertex && p.geo !== null && !p.geo.relative) {
      x += p.geo.x;
      y += p.geo.y;
    }
    p = p.parent === null ? undefined : cells.get(p.parent);
  }
  return { x, y };
}

export function absBounds(cell: Cell, cells: Map<string, Cell>): Rect | null {
  if (cell.geo === null || cell.geo.relative) return null;
  const o = originOf(cell, cells);
  return { x: o.x + cell.geo.x, y: o.y + cell.geo.y, w: cell.geo.w, h: cell.geo.h };
}

// --- perimeters -------------------------------------------------------------------

const center = (r: Rect): Pt => ({ x: r.x + r.w / 2, y: r.y + r.h / 2 });

/** Where a ray from `r`'s center toward `to` leaves the shape. */
export function perimeterPoint(r: Rect, perimeter: string, to: Pt): Pt {
  const c = center(r);
  const dx = to.x - c.x;
  const dy = to.y - c.y;
  if (dx === 0 && dy === 0) return c;
  if (perimeter === "ellipsePerimeter") {
    const a = r.w / 2;
    const b = r.h / 2;
    const t = 1 / Math.sqrt((dx * dx) / (a * a || 1) + (dy * dy) / (b * b || 1));
    return { x: c.x + dx * t, y: c.y + dy * t };
  }
  if (perimeter === "rhombusPerimeter") {
    const a = r.w / 2;
    const b = r.h / 2;
    const t = 1 / (Math.abs(dx) / (a || 1) + Math.abs(dy) / (b || 1));
    return { x: c.x + dx * t, y: c.y + dy * t };
  }
  // Rectangle (and the fallback for everything else).
  const tx = dx === 0 ? Infinity : r.w / 2 / Math.abs(dx);
  const ty = dy === 0 ? Infinity : r.h / 2 / Math.abs(dy);
  const t = Math.min(tx, ty);
  return { x: c.x + dx * t, y: c.y + dy * t };
}

// --- routing ----------------------------------------------------------------------

export type Dir = "N" | "S" | "E" | "W";
const VEC: Record<Dir, Pt> = { N: { x: 0, y: -1 }, S: { x: 0, y: 1 }, E: { x: 1, y: 0 }, W: { x: -1, y: 0 } };
const JETTY = 20;

export interface Port {
  p: Pt;
  dir: Dir;
}

/** A fixed connection point (`exitX/exitY` or `entryX/entryY`) and the side
 *  it faces. */
export function constraintPort(r: Rect, fx: number, fy: number, dx = 0, dy = 0): Port {
  const p = { x: r.x + fx * r.w + dx, y: r.y + fy * r.h + dy };
  const dists: [Dir, number][] = [
    ["W", fx],
    ["E", 1 - fx],
    ["N", fy],
    ["S", 1 - fy],
  ];
  dists.sort((a, b) => a[1] - b[1]);
  return { p, dir: dists[0][0] };
}

export function sidePort(r: Rect, dir: Dir): Port {
  const c = center(r);
  switch (dir) {
    case "N":
      return { p: { x: c.x, y: r.y }, dir };
    case "S":
      return { p: { x: c.x, y: r.y + r.h }, dir };
    case "E":
      return { p: { x: r.x + r.w, y: c.y }, dir };
    case "W":
      return { p: { x: r.x, y: c.y }, dir };
  }
}

/** Drop repeated and collinear interior points. */
export function simplify(pts: Pt[]): Pt[] {
  const out: Pt[] = [];
  for (const p of pts) {
    const last = out[out.length - 1];
    if (last !== undefined && Math.abs(last.x - p.x) < 0.01 && Math.abs(last.y - p.y) < 0.01) continue;
    out.push(p);
    while (out.length >= 3) {
      const [a, b, c] = out.slice(-3);
      const cross = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
      if (Math.abs(cross) > 0.01) break;
      out.splice(out.length - 2, 1);
    }
  }
  return out;
}

const horizontal = (d: Dir) => d === "E" || d === "W";

/** An orthogonal path leaving `a` along its side's direction and arriving at
 *  `b` from outside its side. */
export function connectPorts(a: Port, b: Port): Pt[] {
  const va = VEC[a.dir];
  const vb = VEC[b.dir];
  const a1 = { x: a.p.x + va.x * JETTY, y: a.p.y + va.y * JETTY };
  const b1 = { x: b.p.x + vb.x * JETTY, y: b.p.y + vb.y * JETTY };
  // A clean L when leaving along one axis and arriving along the other, with
  // the corner ahead of both ports.
  if (horizontal(a.dir) !== horizontal(b.dir)) {
    const corner = horizontal(a.dir) ? { x: b.p.x, y: a.p.y } : { x: a.p.x, y: b.p.y };
    const ahead = (p: Port, q: Pt) => (q.x - p.p.x) * VEC[p.dir].x + (q.y - p.p.y) * VEC[p.dir].y >= 0;
    if (ahead(a, corner) && ahead(b, corner)) return simplify([a.p, corner, b.p]);
  }
  // Facing each other on one axis: a Z through the middle.
  if (horizontal(a.dir) && horizontal(b.dir)) {
    const mx = (a1.x + b1.x) / 2;
    const straight = Math.abs(a.p.y - b.p.y) < 0.5 && (b.p.x - a.p.x) * va.x > 0;
    if (straight) return [a.p, b.p];
    const useMid = (mx - a.p.x) * va.x >= 0 && (mx - b.p.x) * vb.x >= 0;
    const x1 = useMid ? mx : a1.x;
    const x2 = useMid ? mx : b1.x;
    return simplify([a.p, { x: x1, y: a.p.y }, { x: x1, y: (a.p.y + b.p.y) / 2 }, { x: x2, y: (a.p.y + b.p.y) / 2 }, { x: x2, y: b.p.y }, b.p]);
  }
  if (!horizontal(a.dir) && !horizontal(b.dir)) {
    const my = (a1.y + b1.y) / 2;
    const straight = Math.abs(a.p.x - b.p.x) < 0.5 && (b.p.y - a.p.y) * va.y > 0;
    if (straight) return [a.p, b.p];
    const useMid = (my - a.p.y) * va.y >= 0 && (my - b.p.y) * vb.y >= 0;
    const y1 = useMid ? my : a1.y;
    const y2 = useMid ? my : b1.y;
    return simplify([a.p, { x: a.p.x, y: y1 }, { x: (a.p.x + b.p.x) / 2, y: y1 }, { x: (a.p.x + b.p.x) / 2, y: y2 }, { x: b.p.x, y: y2 }, b.p]);
  }
  // Perpendicular but the L corner is behind a port: step out, then L.
  const corner = horizontal(a.dir) ? { x: a1.x, y: b1.y } : { x: b1.x, y: a1.y };
  return simplify([a.p, a1, corner, b1, b.p]);
}

/** Sides for two boxes connected with no fixed ports: a straight run where
 *  they overlap on an axis, else the L draw.io draws. */
export function autoPorts(s: Rect, t: Rect): [Port, Port] {
  const sc = center(s);
  const tc = center(t);
  const overlapY = Math.min(s.y + s.h, t.y + t.h) - Math.max(s.y, t.y);
  const overlapX = Math.min(s.x + s.w, t.x + t.w) - Math.max(s.x, t.x);
  const gapX = Math.max(t.x - (s.x + s.w), s.x - (t.x + t.w));
  const gapY = Math.max(t.y - (s.y + s.h), s.y - (t.y + t.h));
  if (overlapY > 0 && gapX > 0) {
    const y = (Math.max(s.y, t.y) + Math.min(s.y + s.h, t.y + t.h)) / 2;
    const right = tc.x > sc.x;
    return [
      { p: { x: right ? s.x + s.w : s.x, y }, dir: right ? "E" : "W" },
      { p: { x: right ? t.x : t.x + t.w, y }, dir: right ? "W" : "E" },
    ];
  }
  if (overlapX > 0 && gapY > 0) {
    const x = (Math.max(s.x, t.x) + Math.min(s.x + s.w, t.x + t.w)) / 2;
    const down = tc.y > sc.y;
    return [
      { p: { x, y: down ? s.y + s.h : s.y }, dir: down ? "S" : "N" },
      { p: { x, y: down ? t.y : t.y + t.h }, dir: down ? "N" : "S" },
    ];
  }
  if (gapX >= gapY) {
    return [sidePort(s, tc.x > sc.x ? "E" : "W"), sidePort(t, tc.y > sc.y ? "N" : "S")];
  }
  return [sidePort(s, tc.y > sc.y ? "S" : "N"), sidePort(t, tc.x > sc.x ? "W" : "E")];
}

/** Orthogonal through waypoints: each leg takes one elbow. */
export function orthThrough(pts: Pt[]): Pt[] {
  const out: Pt[] = [];
  for (let i = 0; i < pts.length; i++) {
    const p = pts[i];
    const prev = out[out.length - 1];
    if (prev !== undefined && Math.abs(prev.x - p.x) > 0.5 && Math.abs(prev.y - p.y) > 0.5) {
      // Continue the previous leg's axis if there was one, else go across first.
      const before = out[out.length - 2];
      const wasVertical = before !== undefined && Math.abs(before.x - prev.x) < 0.5;
      out.push(wasVertical ? { x: prev.x, y: p.y } : { x: p.x, y: prev.y });
    }
    out.push(p);
  }
  return simplify(out);
}

export interface Terminal {
  rect: Rect;
  perimeter: string;
}

/** The start point toward `next` for a terminal, orthogonal when asked:
 *  in line with the waypoint where the box spans it. */
function towards(t: Terminal, next: Pt, orth: boolean): Pt {
  const r = t.rect;
  if (orth) {
    if (next.y >= r.y && next.y <= r.y + r.h) return { x: next.x > r.x + r.w / 2 ? r.x + r.w : r.x, y: next.y };
    if (next.x >= r.x && next.x <= r.x + r.w) return { x: next.x, y: next.y > r.y + r.h / 2 ? r.y + r.h : r.y };
    const c = center(r);
    return Math.abs(next.x - c.x) * r.h >= Math.abs(next.y - c.y) * r.w
      ? sidePort(r, next.x > c.x ? "E" : "W").p
      : sidePort(r, next.y > c.y ? "S" : "N").p;
  }
  return perimeterPoint(r, t.perimeter, next);
}

/**
 * An edge's route in absolute coordinates. `src`/`tgt` are the terminals'
 * boxes (null for a dangling end, which uses `start`/`end`), `waypoints` the
 * points a person set, `style` the edge's style.
 */
export function routeEdge(
  src: Terminal | null,
  tgt: Terminal | null,
  start: Pt | null,
  end: Pt | null,
  waypoints: Pt[],
  style: Style,
): Pt[] {
  const es = style.edgeStyle ?? "none";
  const orth = /orthogonal|elbow|segment|entityRelation|sideToSide|topToBottom|isometric/.test(es);
  const fixedS =
    src !== null && style.exitX !== undefined && style.exitY !== undefined
      ? constraintPort(src.rect, num(style, "exitX", 0.5), num(style, "exitY", 0.5), num(style, "exitDx", 0), num(style, "exitDy", 0))
      : null;
  const fixedT =
    tgt !== null && style.entryX !== undefined && style.entryY !== undefined
      ? constraintPort(tgt.rect, num(style, "entryX", 0.5), num(style, "entryY", 0.5), num(style, "entryDx", 0), num(style, "entryDy", 0))
      : null;

  // A loop on one box.
  if (src !== null && tgt !== null && src.rect === tgt.rect && waypoints.length === 0) {
    const r = src.rect;
    const c = center(r);
    return [
      { x: r.x + r.w, y: c.y - 10 },
      { x: r.x + r.w + 24, y: c.y - 10 },
      { x: r.x + r.w + 24, y: c.y + 10 },
      { x: r.x + r.w, y: c.y + 10 },
    ];
  }

  const sAnchor = fixedS?.p ?? (src !== null ? center(src.rect) : (start ?? { x: 0, y: 0 }));
  const tAnchor = fixedT?.p ?? (tgt !== null ? center(tgt.rect) : (end ?? { x: 0, y: 0 }));

  if (es === "entityRelationEdgeStyle" && src !== null && tgt !== null && waypoints.length === 0) {
    const right = center(tgt.rect).x >= center(src.rect).x;
    const a = sidePort(src.rect, right ? "E" : "W").p;
    const b = sidePort(tgt.rect, right ? "W" : "E").p;
    const seg = 30 * (right ? 1 : -1);
    return [a, { x: a.x + seg, y: a.y }, { x: b.x - seg, y: b.y }, b];
  }

  if (es === "elbowEdgeStyle" && waypoints.length <= 1) {
    const s = src !== null ? center(src.rect) : sAnchor;
    const t = tgt !== null ? center(tgt.rect) : tAnchor;
    const vertical = style.elbow === "vertical";
    const hint = waypoints[0];
    if (vertical) {
      const my = hint?.y ?? (s.y + t.y) / 2;
      const a = src !== null ? towards(src, { x: s.x, y: my }, true) : s;
      const b = tgt !== null ? towards(tgt, { x: t.x, y: my }, true) : t;
      return simplify([a, { x: a.x, y: my }, { x: b.x, y: my }, b]);
    }
    const mx = hint?.x ?? (s.x + t.x) / 2;
    const a = src !== null ? towards(src, { x: mx, y: s.y }, true) : s;
    const b = tgt !== null ? towards(tgt, { x: mx, y: t.y }, true) : t;
    return simplify([a, { x: mx, y: a.y }, { x: mx, y: b.y }, b]);
  }

  if (orth && waypoints.length === 0 && src !== null && tgt !== null) {
    const [ap, bp] = autoPorts(src.rect, tgt.rect);
    return connectPorts(fixedS ?? (fixedT !== null ? portFacing(src.rect, fixedT.p) : ap), fixedT ?? (fixedS !== null ? portFacing(tgt.rect, fixedS.p) : bp));
  }

  const first = waypoints[0] ?? tAnchor;
  const last = waypoints[waypoints.length - 1] ?? sAnchor;
  const a = fixedS?.p ?? (src !== null ? towards(src, first, orth) : sAnchor);
  const b = fixedT?.p ?? (tgt !== null ? towards(tgt, last, orth) : tAnchor);
  const pts = [a, ...waypoints, b];
  return orth ? orthThrough(pts) : simplify(pts);
}

/** The side of `r` facing point `p`, as a port. */
function portFacing(r: Rect, p: Pt): Port {
  const c = center(r);
  const dx = p.x - c.x;
  const dy = p.y - c.y;
  return Math.abs(dx) * r.h >= Math.abs(dy) * r.w ? sidePort(r, dx >= 0 ? "E" : "W") : sidePort(r, dy >= 0 ? "S" : "N");
}

// --- along a route -----------------------------------------------------------------

/** The point `t` (0–1) of the way along a polyline, and its direction. */
export function pointAlong(pts: Pt[], t: number): { p: Pt; angle: number } {
  if (pts.length === 0) return { p: { x: 0, y: 0 }, angle: 0 };
  if (pts.length === 1) return { p: pts[0], angle: 0 };
  const lens = pts.slice(1).map((p, i) => Math.hypot(p.x - pts[i].x, p.y - pts[i].y));
  const total = lens.reduce((a, b) => a + b, 0);
  let want = Math.min(1, Math.max(0, t)) * total;
  for (let i = 0; i < lens.length; i++) {
    const a = pts[i];
    const b = pts[i + 1];
    if (want <= lens[i] || i === lens.length - 1) {
      const f = lens[i] === 0 ? 0 : Math.min(1, want / lens[i]);
      return { p: { x: a.x + (b.x - a.x) * f, y: a.y + (b.y - a.y) * f }, angle: Math.atan2(b.y - a.y, b.x - a.x) };
    }
    want -= lens[i];
  }
  return { p: pts[pts.length - 1], angle: 0 };
}

/** Where an edge label goes: `geo.x` in [-1, 1] along the route (0 = the
 *  middle), `geo.y` across it, then the offset. */
export function edgeLabelPoint(route: Pt[], geo: Geo | null): Pt {
  const along = pointAlong(route, ((geo?.x ?? 0) + 1) / 2);
  const off = geo?.y ?? 0;
  const nx = -Math.sin(along.angle);
  const ny = Math.cos(along.angle);
  return {
    x: along.p.x + nx * off + (geo?.offset?.x ?? 0),
    y: along.p.y + ny * off + (geo?.offset?.y ?? 0),
  };
}

/** SVG path data for a route: straight segments, rounded corners, or a
 *  curve through the points (draw.io's `curved`). */
export function routePath(pts: Pt[], rounded: boolean, curved: boolean, radius = 10): string {
  const r = (v: number) => Math.round(v * 100) / 100;
  if (pts.length === 0) return "";
  let d = `M${r(pts[0].x)} ${r(pts[0].y)}`;
  if (curved && pts.length > 2) {
    for (let i = 1; i < pts.length - 1; i++) {
      const p = pts[i];
      const q = i < pts.length - 2 ? { x: (p.x + pts[i + 1].x) / 2, y: (p.y + pts[i + 1].y) / 2 } : pts[i + 1];
      d += `Q${r(p.x)} ${r(p.y)} ${r(q.x)} ${r(q.y)}`;
    }
    return d;
  }
  for (let i = 1; i < pts.length; i++) {
    const p = pts[i];
    const next = pts[i + 1];
    if (rounded && next !== undefined) {
      const prev = pts[i - 1];
      const l1 = Math.hypot(p.x - prev.x, p.y - prev.y);
      const l2 = Math.hypot(next.x - p.x, next.y - p.y);
      const k = Math.min(radius, l1 / 2, l2 / 2);
      if (k > 0.5) {
        const a = { x: p.x + ((prev.x - p.x) / l1) * k, y: p.y + ((prev.y - p.y) / l1) * k };
        const b = { x: p.x + ((next.x - p.x) / l2) * k, y: p.y + ((next.y - p.y) / l2) * k };
        d += `L${r(a.x)} ${r(a.y)}Q${r(p.x)} ${r(p.y)} ${r(b.x)} ${r(b.y)}`;
        continue;
      }
    }
    d += `L${r(p.x)} ${r(p.y)}`;
  }
  return d;
}
