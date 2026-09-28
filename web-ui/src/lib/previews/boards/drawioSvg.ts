/**
 * A draw.io page drawn as SVG: shapes from their style, labels in
 * foreignObjects (HTML labels through DOMPurify with a text-only profile and
 * a style filter that drops anything that could fetch), edges routed by
 * drawio.ts with draw.io's markers. Images draw only from the file's own
 * data: URLs. Needs a DOM.
 */

import DOMPurify from "dompurify";
import { paperColor, safeColor, unionRects, type Rect } from "./format";
import { cssValueFetches } from "../officeSafety";
import {
  absBounds,
  edgeLabelPoint,
  flag,
  num,
  originOf,
  routeEdge,
  routePath,
  type Cell,
  type Model,
  type Pt,
  type Style,
  type Terminal,
} from "./drawio";

const NS = "http://www.w3.org/2000/svg";
const XHTML = "http://www.w3.org/1999/xhtml";
let seq = 0;

export interface DrawioScene {
  svg: SVGSVGElement;
  bounds: Rect;
  background: string;
  /** Shapes this renderer doesn't know, drawn as labelled boxes. */
  approximated: number;
  /** Images that point outside the file (never loaded). */
  blocked: number;
}

const purify = DOMPurify(window);
const LABEL_TAGS = [
  "b", "i", "u", "s", "strike", "em", "strong", "br", "span", "div", "p", "font", "sub", "sup",
  "ul", "ol", "li", "a", "h1", "h2", "h3", "h4", "h5", "h6", "table", "thead", "tbody", "tr", "td",
  "th", "hr", "pre", "code", "blockquote", "small", "big", "center", "img",
];
const SAFE_STYLE = /^(color|background-color|font-(family|size|style|weight|variant)|text-(align|decoration[\w-]*|transform|shadow)|line-height|letter-spacing|white-space|word-(wrap|break|spacing)|overflow-wrap|vertical-align|display|margin(-\w+)?|padding(-\w+)?|border(-(top|right|bottom|left))?(-(width|style|color))?|border-radius|width|height|max-width|min-width|opacity|list-style-type)$/;
purify.addHook("afterSanitizeAttributes", (node) => {
  if (!(node instanceof HTMLElement)) return;
  if (node.hasAttribute("style")) {
    const kept: string[] = [];
    for (let i = 0; i < node.style.length; i++) {
      const prop = node.style[i];
      const value = node.style.getPropertyValue(prop);
      if (SAFE_STYLE.test(prop) && !cssValueFetches(value, prop)) kept.push(`${prop}: ${value}`);
    }
    if (kept.length > 0) node.setAttribute("style", kept.join("; "));
    else node.removeAttribute("style");
  }
  if (node.tagName === "A") {
    const href = node.getAttribute("href") ?? "";
    if (/^https?:/i.test(href)) {
      node.setAttribute("target", "_blank");
      node.setAttribute("rel", "noopener noreferrer");
    } else node.removeAttribute("href");
  }
  if (node.tagName === "IMG" && !/^data:image\//i.test(node.getAttribute("src") ?? "")) node.removeAttribute("src");
});

function sanitizeLabel(html: string): DocumentFragment {
  return purify.sanitize(html, {
    ALLOWED_TAGS: LABEL_TAGS,
    ALLOWED_ATTR: ["style", "color", "face", "size", "align", "href", "src", "alt", "colspan", "rowspan", "width", "height"],
    RETURN_DOM_FRAGMENT: true,
  });
}

function color(v: string | undefined, fallback: string): string {
  if (v === undefined || v === "default" || v === "inherit" || v === "") return fallback;
  if (v === "none") return "none";
  return safeColor(v) ?? fallback;
}

function node<K extends keyof SVGElementTagNameMap>(tag: K, attrs: Record<string, string | number> = {}): SVGElementTagNameMap[K] {
  const n = document.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, String(v));
  return n;
}

/** A data: image URL from a style value (draw.io drops `;base64` there, as
 *  `;` separates style keys), or null for anything else. */
export function styleImage(v: string | undefined): string | null {
  if (v === undefined) return null;
  const m = /^data:image\/(png|jpe?g|gif|webp|svg\+xml|bmp)(;base64)?,(.*)$/is.exec(v.trim());
  if (m === null) return null;
  const payload = m[3];
  if (m[2] !== undefined || /^[a-z0-9+/=\s]+$/i.test(payload)) return `data:image/${m[1]};base64,${payload.replace(/\s+/g, "")}`;
  return `data:image/${m[1]},${payload}`;
}

// --- shapes -----------------------------------------------------------------------

interface ShapeOut {
  /** Filled + stroked outline. */
  d: string;
  /** Stroke-only extra lines (a cylinder's rim, a note's fold). */
  extra?: string;
  /** Draw as an ellipse instead of `d`. */
  ellipse?: boolean;
  /** Whether this shape was a fallback for one this renderer doesn't know. */
  approx?: boolean;
}

const poly = (pts: [number, number][]) => `M${pts.map(([x, y]) => `${x} ${y}`).join("L")}Z`;

const FLOWCHART: Record<string, string> = {
  decision: "rhombus",
  process: "rectangle",
  document: "document",
  data: "parallelogram",
  database: "cylinder",
  stored_data: "cylinder",
  direct_data: "cylinder",
  start_1: "ellipse",
  start_2: "ellipse",
  on_page_reference: "ellipse",
  "on-page_reference": "ellipse",
  predefined_process: "process",
  terminator: "terminator",
  manual_input: "manualInput",
  multi_document: "document",
};

export function shapeFor(style: Style, w: number, h: number): ShapeOut {
  let shape = style.shape ?? "rectangle";
  if (shape.startsWith("mxgraph.flowchart.")) shape = FLOWCHART[shape.slice(18)] ?? "unknown";
  const size = (d: number) => num(style, "size", d);
  switch (shape) {
    case "rectangle":
    case "label":
    case "table":
    case "swimlane":
    case "process2":
      return { d: rectPath(style, w, h) };
    case "terminator":
      return { d: roundRect(w, h, h / 2) };
    case "ellipse":
    case "doubleEllipse":
      return { d: "", ellipse: true };
    case "rhombus":
      return { d: poly([[w / 2, 0], [w, h / 2], [w / 2, h], [0, h / 2]]) };
    case "triangle": {
      const dir = style.direction ?? "east";
      if (dir === "north") return { d: poly([[0, h], [w / 2, 0], [w, h]]) };
      if (dir === "south") return { d: poly([[0, 0], [w, 0], [w / 2, h]]) };
      if (dir === "west") return { d: poly([[w, 0], [0, h / 2], [w, h]]) };
      return { d: poly([[0, 0], [w, h / 2], [0, h]]) };
    }
    case "hexagon": {
      const s = flag(style, "fixedSize") ? Math.min(size(20), w / 2) : w * size(0.25);
      return { d: poly([[s, 0], [w - s, 0], [w, h / 2], [w - s, h], [s, h], [0, h / 2]]) };
    }
    case "cylinder":
    case "cylinder3":
    case "datastore": {
      const dy = Math.min(h / 2, shape === "cylinder3" ? size(15) : Math.max(4, h * 0.1));
      return {
        d: `M0 ${dy}A${w / 2} ${dy} 0 0 1 ${w} ${dy}L${w} ${h - dy}A${w / 2} ${dy} 0 0 1 0 ${h - dy}Z`,
        extra: `M0 ${dy}A${w / 2} ${dy} 0 0 0 ${w} ${dy}`,
      };
    }
    case "cloud":
      return {
        d: `M${0.25 * w} ${0.25 * h}C${0.05 * w} ${0.25 * h} 0 ${0.5 * h} ${0.16 * w} ${0.55 * h}C0 ${0.66 * h} ${0.18 * w} ${0.9 * h} ${0.31 * w} ${0.8 * h}C${0.4 * w} ${h} ${0.7 * w} ${h} ${0.8 * w} ${0.8 * h}C${w} ${0.8 * h} ${w} ${0.6 * h} ${0.875 * w} ${0.5 * h}C${w} ${0.3 * h} ${0.8 * w} ${0.1 * h} ${0.625 * w} ${0.2 * h}C${0.5 * w} ${0.05 * h} ${0.3 * w} ${0.05 * h} ${0.25 * w} ${0.25 * h}Z`,
      };
    case "process": {
      const s = flag(style, "fixedSize") ? size(10) : w * size(0.1);
      return { d: poly([[0, 0], [w, 0], [w, h], [0, h]]), extra: `M${s} 0L${s} ${h}M${w - s} 0L${w - s} ${h}` };
    }
    case "document": {
      const dy = h * size(0.3);
      return { d: `M0 0L${w} 0L${w} ${h - dy / 2}Q${(3 * w) / 4} ${h - dy * 1.4} ${w / 2} ${h - dy / 2}Q${w / 4} ${h - dy * (1 - 1.4)} 0 ${h - dy / 2}Z` };
    }
    case "parallelogram": {
      const dx = flag(style, "fixedSize") ? size(20) : w * size(0.2);
      return { d: poly([[dx, 0], [w, 0], [w - dx, h], [0, h]]) };
    }
    case "trapezoid": {
      const dx = flag(style, "fixedSize") ? size(20) : w * size(0.2);
      return { d: poly([[dx, 0], [w - dx, 0], [w, h], [0, h]]) };
    }
    case "manualInput": {
      const dy = h * 0.3;
      return { d: poly([[0, dy], [w, 0], [w, h], [0, h]]) };
    }
    case "step": {
      const s = flag(style, "fixedSize") ? size(20) : w * size(0.2);
      return { d: poly([[0, 0], [w - s, 0], [w, h / 2], [w - s, h], [0, h], [s, h / 2]]) };
    }
    case "note": {
      const s = Math.min(size(30), w, h);
      return { d: poly([[0, 0], [w - s, 0], [w, s], [w, h], [0, h]]), extra: `M${w - s} 0L${w - s} ${s}L${w} ${s}` };
    }
    case "card": {
      const s = Math.min(size(15), w, h);
      return { d: poly([[s, 0], [w, 0], [w, h], [0, h], [0, s]]) };
    }
    case "umlActor":
      return {
        d: "",
        extra: `M${w / 2} ${h / 4}L${w / 2} ${(2 * h) / 3}M0 ${h / 3}L${w} ${h / 3}M${w / 2} ${(2 * h) / 3}L0 ${h}M${w / 2} ${(2 * h) / 3}L${w} ${h}`,
      };
    case "line":
      return { d: "", extra: `M0 ${h / 2}L${w} ${h / 2}` };
    case "partialRectangle": {
      const sides: string[] = [];
      if (flag(style, "top", true)) sides.push(`M0 0L${w} 0`);
      if (flag(style, "right", true)) sides.push(`M${w} 0L${w} ${h}`);
      if (flag(style, "bottom", true)) sides.push(`M0 ${h}L${w} ${h}`);
      if (flag(style, "left", true)) sides.push(`M0 0L0 ${h}`);
      return { d: `M0 0H${w}V${h}H0Z`, extra: sides.join("") };
    }
    case "tableRow":
    case "image":
    case "text":
      return { d: "" };
    case "connector":
    case "arrow":
      return { d: poly([[0, h * 0.3], [w * 0.7, h * 0.3], [w * 0.7, 0], [w, h / 2], [w * 0.7, h], [w * 0.7, h * 0.7], [0, h * 0.7]]) };
    default:
      return { d: rectPath(style, w, h), approx: true };
  }
}

function roundRect(w: number, h: number, r: number): string {
  const k = Math.max(0, Math.min(r, w / 2, h / 2));
  if (k === 0) return `M0 0H${w}V${h}H0Z`;
  return `M${k} 0H${w - k}A${k} ${k} 0 0 1 ${w} ${k}V${h - k}A${k} ${k} 0 0 1 ${w - k} ${h}H${k}A${k} ${k} 0 0 1 0 ${h - k}V${k}A${k} ${k} 0 0 1 ${k} 0Z`;
}

function rectPath(style: Style, w: number, h: number): string {
  if (!flag(style, "rounded")) return `M0 0H${w}V${h}H0Z`;
  const r = flag(style, "absoluteArcSize") ? num(style, "arcSize", 20) / 2 : (Math.min(w, h) * num(style, "arcSize", 15)) / 100;
  return roundRect(w, h, r);
}

// --- markers ----------------------------------------------------------------------

interface Marker {
  d: string;
  filled: boolean;
  /** How far the line's end steps back so it doesn't poke through. */
  back: number;
  /** Extra filled circles ([cx, cy, r] in marker space). */
  circles?: [number, number, number][];
}

/** A marker in its own space: the tip at (0, 0), the edge arriving from −x. */
export function markerShape(type: string, size: number, filledFlag: boolean): Marker | null {
  const s = size;
  switch (type) {
    case "none":
    case "":
      return null;
    case "classic":
    case "classicThin": {
      const wf = type === "classic" ? 2 : 3;
      const L = s + 1;
      return { d: `M0 0L${-L} ${-L / wf}L${(-L * 3) / 4} 0L${-L} ${L / wf}Z`, filled: filledFlag, back: (L * 3) / 4 };
    }
    case "block":
    case "blockThin": {
      const wf = type === "block" ? 2 : 3;
      const L = s + 1;
      return { d: `M0 0L${-L} ${-L / wf}L${-L} ${L / wf}Z`, filled: filledFlag, back: L };
    }
    case "open":
    case "openThin":
    case "async":
    case "openAsync":
    case "halfCircle": {
      const wf = type === "openThin" ? 3 : 2;
      const L = s + 1;
      return { d: `M${-L} ${-L / wf}L0 0L${-L} ${L / wf}`, filled: false, back: 0 };
    }
    case "oval":
      return { d: `M${-s} 0A${s / 2} ${s / 2} 0 1 1 0 0A${s / 2} ${s / 2} 0 1 1 ${-s} 0Z`, filled: filledFlag, back: s };
    case "diamond":
    case "diamondThin": {
      const tw = type === "diamond" ? 2 : 3.4;
      const L = s + 1;
      return { d: `M0 0L${-L / 2} ${-L / tw}L${-L} 0L${-L / 2} ${L / tw}Z`, filled: filledFlag, back: L };
    }
    case "box":
      return { d: `M0 ${-s / 2}H${-s}V${s / 2}H0Z`, filled: filledFlag, back: s };
    case "dash":
      return { d: `M${-s / 2} ${-s / 2}L${s / 2} ${s / 2}`, filled: false, back: 0 };
    case "cross":
      return { d: `M${-s} ${-s / 2}L0 ${s / 2}M${-s} ${s / 2}L0 ${-s / 2}`, filled: false, back: 0 };
    case "circle":
    case "circlePlus":
      return {
        d: `M${-s} 0A${s / 2} ${s / 2} 0 1 1 0 0A${s / 2} ${s / 2} 0 1 1 ${-s} 0Z${type === "circlePlus" ? `M${-s / 2} ${-s / 2}V${s / 2}M${-s} 0H0` : ""}`,
        filled: false,
        back: s,
      };
    case "ERone":
      return { d: `M${-s} ${-s / 2}V${s / 2}`, filled: false, back: 0 };
    case "ERmandOne":
      return { d: `M${-s} ${-s / 2}V${s / 2}M${-s * 1.6} ${-s / 2}V${s / 2}`, filled: false, back: 0 };
    case "ERmany":
      return { d: `M0 ${-s / 2}L${-s} 0L0 ${s / 2}`, filled: false, back: 0 };
    case "ERoneToMany":
      return { d: `M0 ${-s / 2}L${-s} 0L0 ${s / 2}M${-s * 1.4} ${-s / 2}V${s / 2}`, filled: false, back: 0 };
    case "ERzeroToOne":
      return {
        d: `M${-s * 0.7} ${-s / 2}V${s / 2}M${-s * 1.4} 0A${s / 3} ${s / 3} 0 1 1 ${-s * 2.07} 0A${s / 3} ${s / 3} 0 1 1 ${-s * 1.4} 0Z`,
        filled: false,
        back: 0,
      };
    case "ERzeroToMany":
      return {
        d: `M0 ${-s / 2}L${-s} 0L0 ${s / 2}M${-s * 1.1} 0A${s / 3} ${s / 3} 0 1 1 ${-s * 1.77} 0A${s / 3} ${s / 3} 0 1 1 ${-s * 1.1} 0Z`,
        filled: false,
        back: 0,
      };
    default:
      return markerShape("classic", size, filledFlag);
  }
}

// --- the page ---------------------------------------------------------------------

interface Ctx {
  model: Model;
  defs: SVGDefsElement;
  idp: string;
  paper: string;
  approximated: number;
  blocked: number;
  gradients: Map<string, string>;
  shadow: string | null;
  boxes: Rect[];
}

function gradient(ctx: Ctx, from: string, to: string, dir: string): string {
  const key = `${from}|${to}|${dir}`;
  const hit = ctx.gradients.get(key);
  if (hit !== undefined) return hit;
  const id = `${ctx.idp}g${ctx.gradients.size}`;
  const [x1, y1, x2, y2] = dir === "north" ? [0, 1, 0, 0] : dir === "east" ? [0, 0, 1, 0] : dir === "west" ? [1, 0, 0, 0] : [0, 0, 0, 1];
  const g = node("linearGradient", { id, x1, y1, x2, y2 });
  g.appendChild(node("stop", { offset: 0, "stop-color": from }));
  g.appendChild(node("stop", { offset: 1, "stop-color": to }));
  ctx.defs.appendChild(g);
  const ref = `url(#${id})`;
  ctx.gradients.set(key, ref);
  return ref;
}

function shadowFilter(ctx: Ctx): string {
  if (ctx.shadow !== null) return ctx.shadow;
  const id = `${ctx.idp}shadow`;
  const f = node("filter", { id, x: "-20%", y: "-20%", width: "140%", height: "140%" });
  f.appendChild(node("feDropShadow", { dx: 2, dy: 3, stdDeviation: 1.5, "flood-color": "#000", "flood-opacity": 0.25 }));
  ctx.defs.appendChild(f);
  ctx.shadow = `url(#${id})`;
  return ctx.shadow;
}

function paint(el: SVGElement, style: Style, ctx: Ctx, fillable = true): void {
  const fill = fillable ? color(style.fillColor, "#ffffff") : "none";
  const grad = color(style.gradientColor, "none");
  el.setAttribute("fill", fill !== "none" && grad !== "none" ? gradient(ctx, fill, grad, style.gradientDirection ?? "south") : fill);
  el.setAttribute("stroke", color(style.strokeColor, "#000000"));
  el.setAttribute("stroke-width", String(Math.min(40, Math.max(0, num(style, "strokeWidth", 1)))));
  if (flag(style, "dashed")) {
    const pattern = (style.dashPattern ?? "").split(/\s+/).map(Number).filter((v) => Number.isFinite(v) && v >= 0);
    const sw = num(style, "strokeWidth", 1);
    el.setAttribute("stroke-dasharray", (pattern.length > 0 ? pattern : [3, 3]).map((v) => v * sw).join(" "));
  }
  if (style.fillOpacity !== undefined) el.setAttribute("fill-opacity", String(num(style, "fillOpacity", 100) / 100));
  if (style.strokeOpacity !== undefined) el.setAttribute("stroke-opacity", String(num(style, "strokeOpacity", 100) / 100));
  el.setAttribute("stroke-linejoin", "round");
}

const FONT_STACKS: Record<string, string> = {
  helvetica: "Helvetica, Arial, sans-serif",
  verdana: "Verdana, Geneva, sans-serif",
  "courier new": '"Courier New", Courier, monospace',
  "times new roman": '"Times New Roman", Times, serif',
};

function fontFamily(v: string | undefined): string {
  const name = (v ?? "Helvetica").replace(/[^\w\s,'"-]/g, "").trim() || "Helvetica";
  return FONT_STACKS[name.toLowerCase()] ?? `"${name.replace(/"/g, "")}", Helvetica, Arial, sans-serif`;
}

/** A label in a foreignObject laid out like draw.io's HTML labels. */
function label(value: string, style: Style, box: Rect, ctx: Ctx, edgeLabel: boolean): SVGElement | null {
  if (value.trim() === "" || flag(style, "noLabel")) return null;
  const html = flag(style, "html");
  const wrap = style.whiteSpace === "wrap";
  const spacing = num(style, "spacing", 2);
  const pad = {
    t: spacing + num(style, "spacingTop", 0),
    r: spacing + num(style, "spacingRight", 0),
    b: spacing + num(style, "spacingBottom", 0),
    l: spacing + num(style, "spacingLeft", 0),
  };
  const vertical = style.horizontal === "0";
  const w = vertical ? box.h : box.w;
  const h = vertical ? box.w : box.h;
  const fo = node("foreignObject", {
    x: box.x + box.w / 2 - w / 2,
    y: box.y + box.h / 2 - h / 2,
    width: Math.max(1, w),
    height: Math.max(1, h),
    style: "overflow: visible",
  });
  if (vertical) fo.setAttribute("transform", `rotate(-90 ${box.x + box.w / 2} ${box.y + box.h / 2})`);
  const outer = document.createElementNS(XHTML, "div") as HTMLDivElement;
  const align = style.align ?? "center";
  const valign = style.verticalAlign ?? "middle";
  outer.style.cssText = [
    "display: flex",
    "box-sizing: border-box",
    `width: ${Math.max(1, w)}px`,
    `height: ${Math.max(1, h)}px`,
    `padding: ${pad.t}px ${pad.r}px ${pad.b}px ${pad.l}px`,
    `align-items: ${valign === "top" ? "flex-start" : valign === "bottom" ? "flex-end" : "center"}`,
    `justify-content: ${align === "left" ? "flex-start" : align === "right" ? "flex-end" : "center"}`,
  ].join(";");
  const inner = document.createElementNS(XHTML, "div") as HTMLDivElement;
  const fs = Math.min(400, Math.max(1, num(style, "fontSize", 11)));
  const bits = Math.trunc(num(style, "fontStyle", 0));
  const bg = color(style.labelBackgroundColor, edgeLabel ? ctx.paper : "none");
  const border = color(style.labelBorderColor, "none");
  inner.style.cssText = [
    `font-family: ${fontFamily(style.fontFamily)}`,
    `font-size: ${fs}px`,
    "line-height: 1.2",
    `color: ${color(style.fontColor, "#000000")}`,
    `text-align: ${align === "left" ? "left" : align === "right" ? "right" : "center"}`,
    `font-weight: ${bits & 1 ? "bold" : "normal"}`,
    `font-style: ${bits & 2 ? "italic" : "normal"}`,
    `text-decoration: ${[bits & 4 ? "underline" : "", bits & 8 ? "line-through" : ""].join(" ").trim() || "none"}`,
    wrap ? `max-width: ${Math.max(1, w - pad.l - pad.r)}px` : "white-space: nowrap",
    wrap ? "overflow-wrap: anywhere" : "",
    bg !== "none" ? `background-color: ${bg}` : "",
    border !== "none" ? `border: 1px solid ${border}` : "",
    edgeLabel ? "padding: 0 2px" : "",
    style.textOpacity !== undefined ? `opacity: ${num(style, "textOpacity", 100) / 100}` : "",
    html ? "" : "white-space: pre-wrap",
  ]
    .filter((s) => s !== "")
    .join(";");
  if (html) inner.appendChild(sanitizeLabel(value));
  else inner.textContent = value;
  outer.appendChild(inner);
  fo.appendChild(outer);
  ctx.boxes.push(box);
  return fo;
}

function drawVertex(cell: Cell, ctx: Ctx, parent: SVGElement): void {
  const r = absBounds(cell, ctx.model.cells);
  if (r === null) return;
  const s = cell.style;
  const g = node("g");
  const rot = num(s, "rotation", 0);
  if (rot !== 0) g.setAttribute("transform", `rotate(${rot} ${r.x + r.w / 2} ${r.y + r.h / 2})`);
  if (s.opacity !== undefined) g.setAttribute("opacity", String(num(s, "opacity", 100) / 100));
  ctx.boxes.push(r);
  const shape = shapeFor(s, r.w, r.h);
  if (shape.approx) ctx.approximated += 1;
  const local = node("g", { transform: `translate(${r.x} ${r.y})` });
  if (flag(s, "shadow")) local.setAttribute("filter", shadowFilter(ctx));
  const kind = s.shape ?? "rectangle";
  if (kind === "swimlane") {
    const start = Math.min(num(s, "startSize", 23), flag(s, "horizontal", true) ? r.h : r.w);
    const horiz = s.horizontal !== "0";
    const head = node("path", { d: horiz ? `M0 0H${r.w}V${start}H0Z` : `M0 0H${start}V${r.h}H0Z` });
    paint(head, s, ctx);
    const body = node("path", { d: horiz ? `M0 ${start}H${r.w}V${r.h}H0Z` : `M${start} 0H${r.w}V${r.h}H${start}Z` });
    paint(body, { ...s, fillColor: s.swimlaneFillColor ?? "none", gradientColor: "none" }, ctx);
    local.append(body, head);
  } else if (shape.ellipse) {
    const e = node("ellipse", { cx: r.w / 2, cy: r.h / 2, rx: r.w / 2, ry: r.h / 2 });
    paint(e, s, ctx);
    local.appendChild(e);
    if (kind === "doubleEllipse") {
      const inner = node("ellipse", { cx: r.w / 2, cy: r.h / 2, rx: Math.max(0, r.w / 2 - 4), ry: Math.max(0, r.h / 2 - 4) });
      paint(inner, s, ctx, false);
      local.appendChild(inner);
    }
  } else if (shape.d !== "") {
    const p = node("path", { d: shape.d });
    paint(p, s, ctx);
    if (kind === "partialRectangle") p.setAttribute("stroke", "none");
    local.appendChild(p);
  }
  if (kind === "umlActor") {
    const head = node("ellipse", { cx: r.w / 2, cy: r.h / 8, rx: r.w / 4, ry: r.h / 8 });
    paint(head, s, ctx);
    local.appendChild(head);
  }
  if (shape.extra !== undefined && shape.extra !== "") {
    const x = node("path", { d: shape.extra });
    paint(x, s, ctx, false);
    local.appendChild(x);
  }
  if (kind === "image" || s.image !== undefined) {
    const url = styleImage(s.image);
    if (url !== null) {
      const im = node("image", { width: r.w, height: r.h, class: "dio-img", preserveAspectRatio: flag(s, "imageAspect", true) ? "xMidYMid meet" : "none" });
      im.setAttribute("href", url);
      local.appendChild(im);
    } else if (s.image !== undefined) {
      ctx.blocked += 1;
      const ph = node("path", { d: `M0 0H${r.w}V${r.h}H0Z`, fill: "none", stroke: "#999", "stroke-dasharray": "4 3" });
      local.appendChild(ph);
    }
  }
  g.appendChild(local);
  // The label: in the swimlane's header, outside the box for label positions.
  let box: Rect = { ...r };
  if (kind === "swimlane") {
    const start = num(s, "startSize", 23);
    box = s.horizontal === "0" ? { x: r.x, y: r.y, w: start, h: r.h } : { x: r.x, y: r.y, w: r.w, h: start };
  }
  if (s.labelPosition === "left") box.x -= r.w;
  else if (s.labelPosition === "right") box.x += r.w;
  if (s.verticalLabelPosition === "top") box.y -= r.h;
  else if (s.verticalLabelPosition === "bottom") box.y += r.h;
  const text = label(cell.value, kind === "swimlane" ? { ...s, horizontal: s.horizontal === "0" ? "0" : "1" } : s, box, ctx, false);
  if (text !== null) g.appendChild(text);
  parent.appendChild(g);
}

function terminal(id: string | null, ctx: Ctx): Terminal | null {
  if (id === null) return null;
  const c = ctx.model.cells.get(id);
  if (c === undefined || !c.vertex) return null;
  const rect = absBounds(c, ctx.model.cells);
  if (rect === null) return null;
  return { rect, perimeter: c.style.perimeter ?? (c.style.shape === "ellipse" ? "ellipsePerimeter" : c.style.shape === "rhombus" ? "rhombusPerimeter" : "rectanglePerimeter") };
}

function drawEdge(cell: Cell, ctx: Ctx, parent: SVGElement): void {
  const s = cell.style;
  const geo = cell.geo;
  const o = originOf(cell, ctx.model.cells);
  const shift = (p: Pt): Pt => ({ x: p.x + o.x, y: p.y + o.y });
  const src = terminal(cell.source, ctx);
  const tgt = terminal(cell.target, ctx);
  const start = geo?.sourcePoint !== null && geo?.sourcePoint !== undefined ? shift(geo.sourcePoint) : null;
  const end = geo?.targetPoint !== null && geo?.targetPoint !== undefined ? shift(geo.targetPoint) : null;
  if ((src === null && start === null) || (tgt === null && end === null)) return;
  const route = routeEdge(src, tgt, start, end, (geo?.points ?? []).map(shift), s);
  if (route.length < 2) return;
  const g = node("g");
  if (s.opacity !== undefined) g.setAttribute("opacity", String(num(s, "opacity", 100) / 100));
  const stroke = color(s.strokeColor, "#000000");
  const sw = Math.min(40, Math.max(0, num(s, "strokeWidth", 1)));
  // Markers first: they decide how far each end of the line steps back.
  const ends: { at: Pt; from: Pt; type: string; size: number; fill: boolean }[] = [
    { at: route[route.length - 1], from: route[route.length - 2], type: s.endArrow ?? "classic", size: num(s, "endSize", 6), fill: flag(s, "endFill", true) },
    { at: route[0], from: route[1], type: s.startArrow ?? "none", size: num(s, "startSize", 6), fill: flag(s, "startFill", true) },
  ];
  const line = route.map((p) => ({ ...p }));
  const markers: SVGElement[] = [];
  ends.forEach((e, i) => {
    const size = /^ER/.test(e.type) ? Math.max(10, e.size) : e.size;
    const m = markerShape(e.type, size, e.fill);
    if (m === null) return;
    const angle = Math.atan2(e.at.y - e.from.y, e.at.x - e.from.x);
    const mk = node("path", {
      d: m.d,
      transform: `translate(${e.at.x} ${e.at.y}) rotate(${(angle * 180) / Math.PI})`,
      fill: m.filled ? stroke : "none",
      stroke,
      "stroke-width": sw,
      "stroke-linejoin": "round",
    });
    markers.push(mk);
    const seg = Math.hypot(e.at.x - e.from.x, e.at.y - e.from.y);
    const back = Math.min(m.back, seg * 0.8);
    const idx = i === 0 ? line.length - 1 : 0;
    line[idx] = { x: e.at.x - Math.cos(angle) * back, y: e.at.y - Math.sin(angle) * back };
  });
  const path = node("path", {
    d: routePath(line, flag(s, "rounded"), flag(s, "curved"), num(s, "arcSize", 20) / 2),
    fill: "none",
    stroke,
    "stroke-width": sw,
    "stroke-linejoin": "round",
  });
  if (flag(s, "dashed")) {
    const pattern = (s.dashPattern ?? "").split(/\s+/).map(Number).filter((v) => Number.isFinite(v) && v >= 0);
    path.setAttribute("stroke-dasharray", (pattern.length > 0 ? pattern : [3, 3]).map((v) => v * Math.max(1, sw)).join(" "));
  }
  if (flag(s, "shadow")) g.setAttribute("filter", shadowFilter(ctx));
  g.append(path, ...markers);
  for (const p of route) ctx.boxes.push({ x: p.x, y: p.y, w: 0, h: 0 });
  // The edge's own label, then its label children.
  const labels: [string, Style, Pt][] = [];
  if (cell.value !== "") labels.push([cell.value, s, edgeLabelPoint(route, geo)]);
  for (const child of cell.children) {
    if (!child.vertex || !child.visible) continue;
    labels.push([child.value, { ...s, ...child.style, labelBackgroundColor: child.style.labelBackgroundColor ?? s.labelBackgroundColor ?? "default" }, edgeLabelPoint(route, child.geo)]);
  }
  for (const [value, st, p] of labels) {
    const W = 600;
    const H = 300;
    const t = label(value, { ...st, whiteSpace: st.whiteSpace === "wrap" ? "nowrap" : (st.whiteSpace ?? "nowrap"), align: "center", verticalAlign: "middle", spacing: "0" }, { x: p.x - W / 2, y: p.y - H / 2, w: W, h: H }, ctx, true);
    if (t !== null) {
      ctx.boxes.pop();
      ctx.boxes.push({ x: p.x - 40, y: p.y - 10, w: 80, h: 20 });
      g.appendChild(t);
    }
  }
  parent.appendChild(g);
}

/** A cell, then (for a container) its children — draw.io's paint order. */
function drawCell(cell: Cell, ctx: Ctx, parent: SVGElement): void {
  if (!cell.visible) return;
  if (cell.vertex) drawVertex(cell, ctx, parent);
  else if (cell.edge) drawEdge(cell, ctx, parent);
  // An edge's children are its labels, drawn with it; a collapsed
  // container hides its contents.
  if (cell.edge || cell.collapsed) return;
  for (const child of cell.children) drawCell(child, ctx, parent);
}

export function renderDrawio(model: Model): DrawioScene {
  const idp = `dio${++seq}-`;
  const svg = node("svg", { xmlns: NS });
  const defs = node("defs");
  svg.appendChild(defs);
  const paper = paperColor(model.background);
  const ctx: Ctx = { model, defs, idp, paper, approximated: 0, blocked: 0, gradients: new Map(), shadow: null, boxes: [] };
  const content = node("g");
  // The root cell ("0") holds the layers, each layer the diagram; a hidden
  // layer hides its cells. Cells whose parent is missing draw on their own.
  for (const root of model.roots) {
    if (root.vertex || root.edge) {
      drawCell(root, ctx, content);
      continue;
    }
    for (const child of root.children) {
      if (child.vertex || child.edge) drawCell(child, ctx, content);
      else if (child.visible) for (const c of child.children) drawCell(c, ctx, content);
    }
  }
  const bounds = unionRects(ctx.boxes, 20) ?? { x: 0, y: 0, w: 200, h: 100 };
  svg.setAttribute("width", String(bounds.w));
  svg.setAttribute("height", String(bounds.h));
  svg.setAttribute("viewBox", `${bounds.x} ${bounds.y} ${bounds.w} ${bounds.h}`);
  svg.appendChild(node("rect", { x: bounds.x, y: bounds.y, width: bounds.w, height: bounds.h, fill: paper, class: "dio-paper" }));
  svg.appendChild(content);
  return { svg, bounds, background: paper, approximated: ctx.approximated, blocked: ctx.blocked };
}
