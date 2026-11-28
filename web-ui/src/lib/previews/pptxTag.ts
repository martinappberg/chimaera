/**
 * Which drawn element is which shape: tags a slide pptx-renderer drew with
 * the file's shape ids (`data-sid`) and paragraph numbers (`data-para`), so
 * a build can show or hide exactly what PowerPoint would.
 *
 * `renderSlide` draws the background, then the master's and the layout's
 * shapes, then one element per `slide.nodes` entry, in order (a node that
 * fails still leaves its "Render Error" placeholder), and nothing after.
 * So the slide's own top-level shapes are its last `nodes.length`
 * children. Each is checked against its node's box before it is trusted;
 * a slide where that fails falls back to finding each shape by its box, as
 * the edit layer always did, and is reported approximate. Group children
 * are never tagged: PowerPoint animates a group as a whole.
 *
 * Tag before `sanitizeRendered`: it keeps `data-*` but may remove elements.
 */

import type { SlideNode } from "@aiden0z/pptx-renderer";

export interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** A drawn child of the slide element, as the matcher sees it. */
export interface Candidate {
  /** The element's absolute box in slide px; null when it isn't positioned. */
  box: Box | null;
  /** The renderer's placeholder for a node it failed to draw. */
  placeholder: boolean;
}

/** Slide px the edit layer has always allowed between a node's box and its element's. */
const NEAR = 0.75;

function near(actual: number, want: number): boolean {
  return Math.abs(actual - want) < NEAR;
}

/**
 * The drawn box is the node's: the same origin, and a size no smaller. A
 * drawn box may be larger: a text box that fits its shape to its text
 * (`spAutoFit`) is drawn at the size its text measures in this browser's
 * fonts, and a line with no height is drawn 1 px thick.
 */
export function boxMatches(actual: Box, want: Box): boolean {
  return near(actual.x, want.x) && near(actual.y, want.y) && actual.w > want.w - NEAR && actual.h > want.h - NEAR;
}

/** The drawn box is the node's to the edit layer's tolerance on all four sides. */
export function boxExact(actual: Box, want: Box): boolean {
  return near(actual.x, want.x) && near(actual.y, want.y) && near(actual.w, want.w) && near(actual.h, want.h);
}

/**
 * Each node's candidate by draw order (`null`: a placeholder, or no
 * trustworthy match), and whether the order failed for the slide: too few
 * children, or a child whose box isn't its node's.
 */
export function planByOrder(cands: readonly Candidate[], boxes: readonly Box[]): { at: (number | null)[]; failed: boolean } {
  const n = boxes.length;
  if (cands.length < n) return { at: boxes.map(() => null), failed: true };
  const base = cands.length - n;
  let failed = false;
  const at = boxes.map((want, k) => {
    const c = cands[base + k]!;
    if (c.placeholder) return null;
    if (c.box !== null && boxMatches(c.box, want)) return base + k;
    failed = true;
    return null;
  });
  return { at, failed };
}

/** The last candidate at exactly `want` not already `taken` (the later of
 *  two shapes sharing a box is drawn on top), else the last one at its
 *  origin drawn larger (a box grown to fit its text), else null. */
export function matchByBox(cands: readonly Candidate[], want: Box, taken: ReadonlySet<number>): number | null {
  for (const fits of [boxExact, boxMatches]) {
    for (let i = cands.length - 1; i >= 0; i--) {
      const c = cands[i]!;
      if (taken.has(i) || c.placeholder || c.box === null) continue;
      if (fits(c.box, want)) return i;
    }
  }
  return null;
}

// --- the DOM side ---------------------------------------------------------------------

export interface TaggedShape {
  el: HTMLElement;
  /** Paragraph elements in file order; null when the drawn paragraphs can't
   *  be matched one to one (a paragraph build then acts on the whole shape). */
  paras: HTMLElement[] | null;
  /** The node's paragraph count (0 without a text body). */
  paraCount: number;
}

export interface TaggedSlide {
  /** Shape id → its drawn element. No prototype concerns: a Map. */
  shapes: Map<string, TaggedShape>;
  /** The draw-order check failed: shapes were found by their boxes. */
  fellBack: boolean;
  /** Text shapes whose drawn paragraphs didn't match the file's one to one. */
  paraMismatches: number;
}

function px(v: string): number | null {
  if (!v.endsWith("px")) return null;
  const n = parseFloat(v);
  return Number.isFinite(n) ? n : null;
}

function boxOf(el: Element): Box | null {
  if (!(el instanceof HTMLElement) || el.style.position !== "absolute") return null;
  const s = el.style;
  const x = px(s.left);
  const y = px(s.top);
  const w = px(s.width);
  const h = px(s.height);
  return x === null || y === null || w === null || h === null ? null : { x, y, w, h };
}

function isPlaceholder(el: Element): boolean {
  return el instanceof HTMLElement && el.title.startsWith("Failed to render node:");
}

function nodeBox(n: SlideNode): Box {
  return { x: n.position.x, y: n.position.y, w: n.size.w, h: n.size.h };
}

/** The text container the renderer lays a shape's paragraphs into (the edit
 *  layer types into the same one). */
export function textBodyOf(shape: HTMLElement): HTMLElement | null {
  for (const el of Array.from(shape.querySelectorAll<HTMLElement>("div"))) {
    if (el.style.display === "flex" && el.style.flexDirection === "column") return el;
  }
  return null;
}

function paragraphCount(n: SlideNode): number {
  const body = (n as { textBody?: { paragraphs?: unknown[] } }).textBody;
  return Array.isArray(body?.paragraphs) ? body.paragraphs.length : 0;
}

/** The shape's drawn paragraphs, tagged `data-para`, when there is one per
 *  paragraph of the node; else null. */
function tagParagraphs(el: HTMLElement, count: number): HTMLElement[] | null {
  if (count === 0) return null;
  const body = textBodyOf(el);
  if (body === null) return null;
  const blocks = Array.from(body.children).filter(
    (c): c is HTMLElement => c instanceof HTMLElement && (c.tagName === "DIV" || c.tagName === "P"),
  );
  if (blocks.length !== count) return null;
  blocks.forEach((b, i) => b.setAttribute("data-para", String(i)));
  return blocks;
}

/**
 * Tag `slideEl`'s top-level shapes with their node ids and return them.
 * `nodes` is the slide's materialized `slide.nodes`. Ids that repeat on the
 * slide, and empty ids, are left untagged.
 */
export function tagSlide(slideEl: HTMLElement, nodes: readonly SlideNode[]): TaggedSlide {
  const children = Array.from(slideEl.children);
  const cands: Candidate[] = children.map((c) => ({ box: boxOf(c), placeholder: isPlaceholder(c) }));
  const boxes = nodes.map(nodeBox);
  const plan = planByOrder(cands, boxes);
  const seen = new Map<string, number>();
  for (const n of nodes) seen.set(n.id, (seen.get(n.id) ?? 0) + 1);

  const shapes = new Map<string, TaggedShape>();
  let paraMismatches = 0;
  const taken = new Set<number>(plan.at.filter((i): i is number => i !== null));
  for (const [k, n] of nodes.entries()) {
    if (n.id === "" || seen.get(n.id) !== 1) continue;
    let at = plan.at[k] ?? null;
    if (at === null && plan.failed) {
      const placeholderHere = cands.length >= nodes.length && cands[cands.length - nodes.length + k]?.placeholder === true;
      if (!placeholderHere) at = matchByBox(cands, boxes[k]!, taken);
      if (at !== null) taken.add(at);
    }
    if (at === null) continue;
    const el = children[at] as HTMLElement;
    el.setAttribute("data-sid", n.id);
    const paraCount = paragraphCount(n);
    const paras = tagParagraphs(el, paraCount);
    if (paras === null && paraCount > 0) paraMismatches += 1;
    shapes.set(n.id, { el, paras, paraCount });
  }
  // How the slide was matched, for a look at the live DOM (`order` or `boxes`).
  slideEl.setAttribute("data-tagged", plan.failed ? "boxes" : "order");
  return { shapes, fellBack: plan.failed, paraMismatches };
}

/** Drop what sanitizing removed from under a tagged slide. */
export function pruneTagged(slideEl: HTMLElement, tagged: TaggedSlide): void {
  for (const [id, s] of tagged.shapes) {
    if (!slideEl.contains(s.el)) {
      tagged.shapes.delete(id);
      tagged.fellBack = true;
    } else if (s.paras !== null && s.paras.some((p) => !s.el.contains(p))) {
      s.paras = null;
      tagged.paraMismatches += 1;
    }
  }
}

/**
 * The drawn element of shape `id` at `box` on `root`: its `data-sid` tag
 * when the slide was tagged, else the last positioned div at that box.
 */
export function drawnShapeEl(root: HTMLElement, id: string, box: Box): HTMLElement | null {
  for (const el of Array.from(root.children)) {
    if (el instanceof HTMLElement && el.getAttribute("data-sid") === id) return el;
  }
  let exact: HTMLElement | null = null;
  let grown: HTMLElement | null = null;
  for (const el of Array.from(root.querySelectorAll<HTMLElement>("div"))) {
    const b = boxOf(el);
    if (b === null) continue;
    if (boxExact(b, box)) exact = el;
    else if (boxMatches(b, box)) grown = el;
  }
  return exact ?? grown;
}
