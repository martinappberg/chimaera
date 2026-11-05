/**
 * Pointing at a PowerPoint slide, or at part of one, for an agent
 * (`PptxView`'s @ button and area tool). A slide goes out as
 * `@deck.pptx#slide=3 (<title>) "<text>"`; a box or a clicked shape as
 * `@deck.pptx#slide=3&xywh=x,y,w,h (<shape names>) "<their text>"`.
 *
 * Boxes are in points from the slide's top-left, the unit PDF areas use: a
 * deck exported to PDF has the same page size in points, so the box names
 * the same spot there, and python-pptx's EMU are a fixed multiple (12700 per
 * point). The renderer's model is in 96-dpi CSS pixels (`PX_TO_PT`).
 *
 * Shape names are the ones PowerPoint's selection pane shows and python-pptx
 * reads as `shape.name`, so an agent can find the shape in the file.
 */

import {
  buildTextIndex,
  materializeSlideNodes,
  type PresentationData,
  type TextIndexEntry,
} from "@aiden0z/pptx-renderer";
import type { FileSelection } from "../shared/reference";
import { xywhFragment } from "../shared/locator";
import type { Point, Region } from "./imageRegion";

/** CSS pixels (the renderer's unit, 96 dpi) to points. */
export const PX_TO_PT = 0.75;

/** The longest slide text a reference carries (a slide is short; a dense
 *  one is cut with `…` by the composer's own cap). */
const TEXT_MAX = 2000;
/** Shape names typed in a box's context before "+N". */
const NAMES_MAX = 3;

/** One top-level shape on a slide, in points. */
export interface SlideItem {
  /** The shape's name (`Title 1`, `Chart 3`); "" when the deck gives none. */
  name: string;
  kind: string;
  /** The title (or centered title) placeholder. */
  title: boolean;
  /** Its text: paragraphs on lines, table cells `a | b` with rows on lines. */
  text: string;
  box: Region;
}

const flat = (s: string) => s.replace(/\s+/g, " ").trim();

/** An entry belongs to the top-level node whose path it starts with. */
function belongs(entry: TextIndexEntry, prefix: string): boolean {
  return entry.nodePath === prefix || entry.nodePath.startsWith(`${prefix}/`);
}

/** The text of one node's entries: shapes (a group's children too) a line
 *  each, a table on one line as `a | b / c | d` (cells across a row, rows
 *  apart), which survives the one-line quote an agent receives. */
export function entriesText(entries: readonly TextIndexEntry[]): string {
  const lines: string[] = [];
  let rows: string[][] = [];
  let table: string | null = null;
  let rowAt: number | null = null;
  const flush = () => {
    const text = rows
      .map((r) => r.join(" | "))
      .filter((r) => r !== "")
      .join(" / ");
    if (text !== "") lines.push(text);
    rows = [];
    table = null;
    rowAt = null;
  };
  for (const e of entries) {
    if (e.textKind === "table-cell") {
      if (table !== e.nodeId) flush();
      table = e.nodeId;
      const r = e.rowIndex ?? 0;
      if (rowAt !== r) rows.push([]);
      rowAt = r;
      rows[rows.length - 1].push(flat(e.text));
    } else {
      flush();
      lines.push(e.text.trim());
    }
  }
  flush();
  return lines.filter((l) => l !== "").join("\n");
}

/**
 * The slide's own top-level shapes with their text, back to front (the
 * slide's z-order). Layout and master decorations are left out: they are
 * the template, not what the slide says. Never throws — a deck the text
 * index can't read yields shapes without text.
 */
export function slideItems(pres: PresentationData, index: number): SlideItem[] {
  const slide = pres.slides[index];
  if (slide === undefined) return [];
  let entries: TextIndexEntry[] = [];
  try {
    // Parsed lazily: a slide's shapes exist once it has been laid out once.
    materializeSlideNodes(pres, slide);
    // One slide's index, not the whole deck's: the index materializes every
    // slide it walks, and the slide keeps its own index for layout lookups.
    entries = buildTextIndex({ ...pres, slides: [slide] }, { includeShapes: true, includeTables: true, includeGroups: true });
  } catch {
    entries = [];
  }
  const own = entries.filter((e) => e.nodePath.startsWith("slides/0/nodes/"));
  const out: SlideItem[] = [];
  slide.nodes.forEach((node, i) => {
    const { w, h } = node.size;
    if (!(w > 0 && h > 0)) return;
    const prefix = `slides/0/nodes/${node.id || node.name || String(i)}`;
    const ph = node.placeholder?.type;
    out.push({
      name: node.name.trim(),
      kind: node.nodeType,
      title: ph === "title" || ph === "ctrTitle",
      text: entriesText(own.filter((e) => belongs(e, prefix))),
      box: {
        x: node.position.x * PX_TO_PT,
        y: node.position.y * PX_TO_PT,
        w: w * PX_TO_PT,
        h: h * PX_TO_PT,
      },
    });
  });
  return out;
}

/** Top to bottom, then left to right — how a slide is read. Rows within a
 *  few points of each other count as one line. */
function readingOrder(items: readonly SlideItem[]): SlideItem[] {
  return [...items].sort((a, b) => {
    const dy = a.box.y - b.box.y;
    return Math.abs(dy) > 4 ? dy : a.box.x - b.box.x;
  });
}

function overlap(a: Region, b: Region): number {
  const w = Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x);
  const h = Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y);
  return w > 0 && h > 0 ? w * h : 0;
}

/**
 * The shapes a box points at, in reading order: those mostly inside it, and
 * the one it mostly sits in (a box drawn inside a large text box means that
 * text box). A full-slide backdrop the box merely lies on is left out unless
 * nothing else is there.
 */
export function itemsUnder(items: readonly SlideItem[], box: Region): SlideItem[] {
  const area = box.w * box.h;
  if (!(area > 0)) return [];
  const hits = items.filter((it) => {
    const o = overlap(it.box, box);
    return o > 0 && (o >= 0.5 * it.box.w * it.box.h || o >= 0.5 * area);
  });
  // An empty shape (a panel, a divider) is decoration unless it's all there is.
  const said = hits.filter((it) => it.text !== "" || it.kind !== "shape");
  const pool = said.length > 0 ? said : hits;
  // A backdrop (a full-slide picture, an empty panel behind text) gives way
  // to what sits on it: drop a wordless picture or shape that wholly
  // contains a smaller hit. A chart, a table or a card with its own text stays.
  const size = (r: Region) => r.w * r.h;
  const backdrop = (a: SlideItem) => a.text === "" && (a.kind === "picture" || a.kind === "shape");
  const inner = pool.filter(
    (a) =>
      !backdrop(a) ||
      !pool.some((b) => b !== a && size(b.box) < size(a.box) && overlap(a.box, b.box) >= size(b.box) - 0.5),
  );
  return readingOrder(inner);
}

/** The frontmost shape under a point that says or shows something, or null. */
export function itemAt(items: readonly SlideItem[], p: Point): SlideItem | null {
  for (let i = items.length - 1; i >= 0; i--) {
    const it = items[i];
    const { x, y, w, h } = it.box;
    if (p.x < x || p.y < y || p.x > x + w || p.y > y + h) continue;
    if (it.kind === "shape" && it.text === "") continue;
    return it;
  }
  return null;
}

/** A shape's box snapped outward to whole points and kept on the slide. */
export function snapBox(r: Region, slideW: number, slideH: number): Region | null {
  const x0 = Math.max(0, Math.floor(r.x));
  const y0 = Math.max(0, Math.floor(r.y));
  const x1 = Math.min(Math.ceil(slideW), Math.ceil(r.x + r.w));
  const y1 = Math.min(Math.ceil(slideH), Math.ceil(r.y + r.h));
  return x1 > x0 && y1 > y0 ? { x: x0, y: y0, w: x1 - x0, h: y1 - y0 } : null;
}

function joinText(items: readonly SlideItem[]): string {
  return items
    .map((it) => flat(it.text))
    .filter((t) => t !== "")
    .join(" · ")
    .slice(0, TEXT_MAX);
}

/** The whole slide: its title as the context, the rest of its text as the quote. */
export function slideReference(path: string, n: number, items: readonly SlideItem[]): FileSelection {
  const ordered = readingOrder(items.filter((it) => it.text !== ""));
  const title = flat(ordered.filter((it) => it.title).map((it) => it.text).join(" "));
  const body = joinText(ordered.filter((it) => !it.title));
  const sel: FileSelection = {
    kind: "file",
    path,
    startLine: null,
    endLine: null,
    text: body !== "" ? body : title,
    quote: body,
    fragment: `slide=${n}`,
    label: `slide ${n}`,
  };
  if (title !== "") sel.context = title;
  return sel;
}

/** Shape names for a context: `Title 1, Chart 3 +2`. */
export function namesContext(items: readonly SlideItem[]): string {
  const names = items.map((it) => it.name).filter((s) => s !== "");
  if (names.length === 0) return "";
  const shown = names.slice(0, NAMES_MAX).join(", ");
  return names.length > NAMES_MAX ? `${shown} +${names.length - NAMES_MAX}` : shown;
}

/** A box on slide `n` (points): the shapes under it by name, their text quoted. */
export function areaReference(path: string, n: number, items: readonly SlideItem[], box: Region): FileSelection {
  const under = itemsUnder(items, box);
  const text = joinText(under);
  const sel: FileSelection = {
    kind: "file",
    path,
    startLine: null,
    endLine: null,
    text,
    quote: text,
    fragment: `slide=${n}&${xywhFragment(box)}`,
    label: under.length === 1 && under[0].name !== "" ? `${under[0].name} on slide ${n}` : `area on slide ${n}`,
  };
  const names = namesContext(under);
  if (names !== "") sel.context = names;
  return sel;
}
