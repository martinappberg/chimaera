/**
 * Hover previews on a document's links — the pure pieces (the controller is
 * `hoverController.svelte.ts`, the popover `HoverPreview.svelte`): what a link
 * previews, which part of a markdown target shows, and where the popover
 * sits.
 */
import { anchorIds, classifyHref, parseLineFragment } from "../mdDoc";
import { pathTarget } from "../../shared/embed/embed";

/** What hovering a link shows. */
export type HoverTarget =
  /** A place in this document (`#results`, `#fn-1`). */
  | { kind: "self"; anchor: string }
  /** A file: `target` as the daemon resolves a document's link (an href,
   *  fragment included); `byName` for a wikilink. */
  | { kind: "file"; target: string; fragment: string | null; byName: boolean };

/** What `href` previews, or null for nothing (a web or mail link, a line
 *  of this document, a footnote's way back, an empty link). */
export function hoverTarget(href: string, byName = false): HoverTarget | null {
  const t = classifyHref(href);
  if (t.kind === "anchor") return /^(user-content-)?fnref-/.test(t.anchor) ? null : { kind: "self", anchor: t.anchor };
  if (t.kind !== "path") return null;
  // A `file:` URL names a path outright; anything else is the href a
  // document writes, which the daemon reads as a link.
  const bare = href.trim();
  const target = /^file:/i.test(bare)
    ? `${pathTarget(t.path)}${t.fragment !== null ? `#${t.fragment}` : ""}`
    : bare;
  return { kind: "file", target, fragment: t.fragment, byName };
}

/** Whether a fragment names lines (`#L12`), not a heading. */
export function isLineFragment(fragment: string | null): boolean {
  return fragment !== null && parseLineFragment(fragment) !== null;
}

// --- which part of a document ----------------------------------------------------------

export interface Heading {
  from: number;
  level: number;
  /** Unprefixed anchor (`results`; the element id is `user-content-results`). */
  id: string;
}

/**
 * The stretch of a document a preview shows: the section `anchor` names —
 * its heading through the next heading of the same or a higher rank — or,
 * with no anchor or none such, the opening (`found` says which).
 */
export function sectionBounds(
  headings: readonly Heading[],
  anchor: string | null,
  end: number,
): { from: number; to: number; found: boolean } {
  if (anchor === null || anchor === "") return { from: 0, to: end, found: true };
  const wanted = new Set(anchorIds(anchor));
  const i = headings.findIndex((h) => wanted.has(`user-content-${h.id}`));
  if (i < 0) return { from: 0, to: end, found: false };
  const h = headings[i];
  const next = headings.slice(i + 1).find((n) => n.level <= h.level);
  return { from: h.from, to: next?.from ?? end, found: true };
}

// --- where the popover sits --------------------------------------------------------------

export interface Rect {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

export interface Placement {
  left: number;
  width: number;
  /** Below the link: its top edge; above: its bottom edge's distance from
   *  the container's bottom (so it grows away from the link as it loads). */
  top: number | null;
  bottom: number | null;
  maxHeight: number;
}

/**
 * Place a popover of up to `want` width and height against `link` (both in
 * the container's coordinates) inside a `box`-sized container: below the
 * link when that has room for most of it (or more room than above), else
 * above; never past an edge (`margin`), never covering the link.
 */
export function placePopover(
  link: Rect,
  box: { width: number; height: number },
  want: { width: number; height: number },
  gap = 6,
  margin = 8,
): Placement {
  const width = Math.max(0, Math.min(want.width, box.width - 2 * margin));
  const left = Math.max(margin, Math.min(link.left, box.width - width - margin));
  const below = box.height - link.bottom - gap - margin;
  const above = link.top - gap - margin;
  const down = below >= Math.min(want.height, 220) || below >= above;
  const room = Math.max(0, down ? below : above);
  return {
    left,
    width,
    top: down ? link.bottom + gap : null,
    bottom: down ? null : box.height - link.top + gap,
    maxHeight: Math.min(want.height, room),
  };
}
