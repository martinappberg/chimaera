/**
 * JSON Canvas 1.0 (Obsidian's `.canvas`): parsing with every field checked,
 * the preset colors mapped to theme tokens, and the edge geometry — cubic
 * curves leaving and entering the named sides of their nodes, with arrow
 * ends. Pure: CanvasBoard.svelte draws what this computes.
 * Spec: https://github.com/obsidianmd/jsoncanvas/blob/main/spec/1.0.md
 */

import { safeColor, unionRects, type Rect } from "./format";

export type Side = "top" | "right" | "bottom" | "left";
export type End = "none" | "arrow";

export interface CanvasNodeBase extends Rect {
  id: string;
  /** A CSS color: a theme-token expression for presets, else a checked hex. */
  color: string | null;
}
export interface TextNode extends CanvasNodeBase {
  type: "text";
  text: string;
}
export interface FileNode extends CanvasNodeBase {
  type: "file";
  file: string;
  subpath: string | null;
}
export interface LinkNode extends CanvasNodeBase {
  type: "link";
  url: string;
}
export interface GroupNode extends CanvasNodeBase {
  type: "group";
  label: string | null;
  background: string | null;
  backgroundStyle: "cover" | "ratio" | "repeat";
}
export type CanvasNode = TextNode | FileNode | LinkNode | GroupNode;

export interface CanvasEdge {
  id: string;
  from: string;
  to: string;
  fromSide: Side | null;
  toSide: Side | null;
  fromEnd: End;
  toEnd: End;
  color: string | null;
  label: string | null;
}

export interface CanvasDoc {
  nodes: CanvasNode[];
  edges: CanvasEdge[];
  bounds: Rect;
  /** Nodes or edges dropped as malformed (or past the caps). */
  dropped: number;
}

const MAX_NODES = 10_000;
const MAX_EDGES = 20_000;
const MAX_TEXT = 100_000;
const MAX_DIM = 100_000;

/** The spec's six presets, as the theme's own hues. */
const PRESETS: Record<string, string> = {
  "1": "var(--err)",
  "2": "color-mix(in oklab, var(--err) 45%, var(--warn))",
  "3": "var(--warn)",
  "4": "var(--syn-string)",
  "5": "var(--syn-type)",
  "6": "var(--rate)",
};

export function canvasColor(c: unknown): string | null {
  if (typeof c !== "string") return null;
  const preset = PRESETS[c.trim()];
  if (preset !== undefined) return preset;
  const s = safeColor(c);
  return s !== null && s.startsWith("#") ? s : null;
}

const SIDES = new Set<Side>(["top", "right", "bottom", "left"]);
const side = (v: unknown): Side | null => (typeof v === "string" && SIDES.has(v as Side) ? (v as Side) : null);
const str = (v: unknown, max = 4096): string | null => (typeof v === "string" ? v.slice(0, max) : null);
const num = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);

export function parseCanvas(text: string): CanvasDoc {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    throw new Error("this canvas isn't valid JSON");
  }
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) throw new Error("this isn't a JSON Canvas file");
  const obj = raw as { nodes?: unknown; edges?: unknown };
  const rawNodes = Array.isArray(obj.nodes) ? obj.nodes : [];
  const rawEdges = Array.isArray(obj.edges) ? obj.edges : [];
  let dropped = Math.max(0, rawNodes.length - MAX_NODES) + Math.max(0, rawEdges.length - MAX_EDGES);
  const nodes: CanvasNode[] = [];
  const ids = new Set<string>();
  for (const n of rawNodes.slice(0, MAX_NODES) as Record<string, unknown>[]) {
    const id = n !== null && typeof n === "object" ? str(n.id, 256) : null;
    const x = num(n?.x);
    const y = num(n?.y);
    const w = num(n?.width);
    const h = num(n?.height);
    if (id === null || ids.has(id) || x === null || y === null || w === null || h === null || w <= 0 || h <= 0) {
      dropped += 1;
      continue;
    }
    const base = {
      id,
      x,
      y,
      w: Math.min(w, MAX_DIM),
      h: Math.min(h, MAX_DIM),
      color: canvasColor(n.color),
    };
    let node: CanvasNode | null = null;
    switch (n.type) {
      case "text":
        node = { ...base, type: "text", text: str(n.text, MAX_TEXT) ?? "" };
        break;
      case "file": {
        const file = str(n.file);
        if (file !== null && file !== "") {
          node = { ...base, type: "file", file, subpath: str(n.subpath, 512) };
        }
        break;
      }
      case "link": {
        const url = str(n.url);
        if (url !== null) node = { ...base, type: "link", url };
        break;
      }
      case "group": {
        const bs = n.backgroundStyle;
        node = {
          ...base,
          type: "group",
          label: str(n.label, 512),
          background: str(n.background),
          backgroundStyle: bs === "ratio" || bs === "repeat" ? bs : "cover",
        };
        break;
      }
    }
    if (node === null) {
      dropped += 1;
      continue;
    }
    ids.add(id);
    nodes.push(node);
  }
  const edges: CanvasEdge[] = [];
  for (const e of rawEdges.slice(0, MAX_EDGES) as Record<string, unknown>[]) {
    const id = e !== null && typeof e === "object" ? str(e.id, 256) : null;
    const from = str(e?.fromNode, 256);
    const to = str(e?.toNode, 256);
    if (id === null || from === null || to === null || !ids.has(from) || !ids.has(to)) {
      dropped += 1;
      continue;
    }
    edges.push({
      id,
      from,
      to,
      fromSide: side(e.fromSide),
      toSide: side(e.toSide),
      fromEnd: e.fromEnd === "arrow" ? "arrow" : "none",
      toEnd: e.toEnd === "none" ? "none" : "arrow",
      color: canvasColor(e.color),
      label: str(e.label, 512),
    });
  }
  const bounds = unionRects(nodes, 0) ?? { x: 0, y: 0, w: 1, h: 1 };
  // Group labels sit above their group: leave room for one at the top.
  const labelled = nodes.some((n) => n.type === "group" && n.label !== null && n.y - bounds.y < 28);
  return {
    nodes,
    edges,
    bounds: labelled ? { ...bounds, y: bounds.y - 28, h: bounds.h + 28 } : bounds,
    dropped,
  };
}

export interface Point {
  x: number;
  y: number;
}

/** The middle of a node's side. */
export function anchor(n: Rect, s: Side): Point {
  switch (s) {
    case "top":
      return { x: n.x + n.w / 2, y: n.y };
    case "bottom":
      return { x: n.x + n.w / 2, y: n.y + n.h };
    case "left":
      return { x: n.x, y: n.y + n.h / 2 };
    case "right":
      return { x: n.x + n.w, y: n.y + n.h / 2 };
  }
}

/** The outward unit normal of a side. */
export function normal(s: Side): Point {
  switch (s) {
    case "top":
      return { x: 0, y: -1 };
    case "bottom":
      return { x: 0, y: 1 };
    case "left":
      return { x: -1, y: 0 };
    case "right":
      return { x: 1, y: 0 };
  }
}

/** Sides for an edge that names none: the ones facing each other, by where
 *  the two nodes' centers lie relative to each other. */
export function facingSides(a: Rect, b: Rect): [Side, Side] {
  const dx = b.x + b.w / 2 - (a.x + a.w / 2);
  const dy = b.y + b.h / 2 - (a.y + a.h / 2);
  // Compare the gaps, not the raw offsets: wide nodes side by side should
  // still connect left-right.
  const gapX = Math.abs(dx) - (a.w + b.w) / 2;
  const gapY = Math.abs(dy) - (a.h + b.h) / 2;
  if (gapX >= gapY) return dx >= 0 ? ["right", "left"] : ["left", "right"];
  return dy >= 0 ? ["bottom", "top"] : ["top", "bottom"];
}

export interface EdgeGeometry {
  /** SVG path data for the curve. */
  d: string;
  start: Point;
  end: Point;
  /** Where a label goes (the curve's midpoint). */
  mid: Point;
  /** Direction the curve travels at each end, pointing out of the curve
   *  (radians; an arrowhead points this way). */
  startAngle: number;
  endAngle: number;
}

/** A cubic from `from`'s side to `to`'s side; each end leaves along its
 *  side's normal, farther for longer edges. */
export function edgeGeometry(from: Rect, fromSide: Side, to: Rect, toSide: Side): EdgeGeometry {
  const p0 = anchor(from, fromSide);
  const p3 = anchor(to, toSide);
  const n0 = normal(fromSide);
  const n3 = normal(toSide);
  const dist = Math.hypot(p3.x - p0.x, p3.y - p0.y);
  const k = Math.min(160, Math.max(40, dist * 0.4));
  const c1 = { x: p0.x + n0.x * k, y: p0.y + n0.y * k };
  const c2 = { x: p3.x + n3.x * k, y: p3.y + n3.y * k };
  const mid = {
    x: (p0.x + 3 * c1.x + 3 * c2.x + p3.x) / 8,
    y: (p0.y + 3 * c1.y + 3 * c2.y + p3.y) / 8,
  };
  const r = (v: number) => Math.round(v * 100) / 100;
  return {
    d: `M${r(p0.x)} ${r(p0.y)}C${r(c1.x)} ${r(c1.y)} ${r(c2.x)} ${r(c2.y)} ${r(p3.x)} ${r(p3.y)}`,
    start: p0,
    end: p3,
    mid,
    startAngle: Math.atan2(p0.y - c1.y, p0.x - c1.x),
    endAngle: Math.atan2(p3.y - c2.y, p3.x - c2.x),
  };
}

/** An arrowhead: a filled triangle with its tip at `tip`, pointing `angle`. */
export function arrowPath(tip: Point, angle: number, length = 12, width = 9): string {
  const bx = tip.x - Math.cos(angle) * length;
  const by = tip.y - Math.sin(angle) * length;
  const px = -Math.sin(angle) * (width / 2);
  const py = Math.cos(angle) * (width / 2);
  const r = (v: number) => Math.round(v * 100) / 100;
  return `M${r(tip.x)} ${r(tip.y)}L${r(bx + px)} ${r(by + py)}L${r(bx - px)} ${r(by - py)}Z`;
}

/** The label's text for a link node (host + path), never parsed as markup. */
export function linkLabel(url: string): { host: string; rest: string; web: boolean } {
  try {
    const u = new URL(url);
    const web = u.protocol === "http:" || u.protocol === "https:";
    return { host: web ? u.host : u.protocol.replace(/:$/, ""), rest: web ? `${u.pathname}${u.search}` : url, web };
  } catch {
    return { host: url, rest: "", web: false };
  }
}
