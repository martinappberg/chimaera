/**
 * The `slides` node's data: a deck of pictures a plugin rendered (one or
 * more builds a slide), read defensively from the tree, and the messages
 * two windows on the same deck exchange while one presents and the other
 * shows the presenter's view. Pure; `deck.test.ts`. The viewer is
 * `DeckNode.svelte`; the paging rules are `previews/slideShow.ts`.
 */
import type { Place } from "../../previews/slideShow";

export interface DeckBuild {
  /** The picture: a workspace path or `output:<path>`. */
  src: string;
  notes: string;
}

export interface DeckSlide {
  title: string;
  /** Skipped by a show (PowerPoint's hidden slide). */
  hidden: boolean;
  /** One short word about the slide beside its title ("unused", "stale"). */
  tag: string | null;
  notes: string;
  /** Where the slide is written, for the reference an agent gets. */
  file: string | null;
  line: number | null;
  builds: DeckBuild[];
}

export const DECK_SLIDES_MAX = 600;
export const DECK_BUILDS_MAX = 40;
const NOTES_MAX = 8_000;

function text(v: unknown, max: number): string {
  return typeof v === "string" ? v.slice(0, max) : "";
}

/** The node's `slides`: malformed rows dropped, sizes capped. A slide with
 *  no picture stays (its place in the show is real), with no builds. */
export function parseDeck(raw: unknown): DeckSlide[] {
  if (!Array.isArray(raw)) return [];
  const out: DeckSlide[] = [];
  for (const row of raw.slice(0, DECK_SLIDES_MAX)) {
    if (typeof row !== "object" || row === null) continue;
    const r = row as Record<string, unknown>;
    const builds: DeckBuild[] = [];
    for (const b of Array.isArray(r.builds) ? r.builds.slice(0, DECK_BUILDS_MAX) : []) {
      if (typeof b === "string" && b !== "") builds.push({ src: b, notes: "" });
      else if (typeof b === "object" && b !== null) {
        const src = text((b as Record<string, unknown>).src, 2048);
        if (src !== "") builds.push({ src, notes: text((b as Record<string, unknown>).notes, NOTES_MAX) });
      }
    }
    const line = typeof r.line === "number" && Number.isInteger(r.line) && r.line > 0 ? r.line : null;
    const file = text(r.file, 2048);
    const tag = text(r.tag, 32);
    out.push({
      title: text(r.title, 200) || `Slide ${out.length + 1}`,
      hidden: r.hidden === true,
      tag: tag === "" ? null : tag,
      notes: text(r.notes, NOTES_MAX),
      file: file === "" ? null : file,
      line,
      builds,
    });
  }
  return out;
}

/** A place clamped onto the deck (a re-render can shorten it). */
export function clampPlace(slides: readonly DeckSlide[], at: Place): Place {
  if (slides.length === 0) return { slide: 0, build: 0 };
  const slide = Math.min(Math.max(0, at.slide), slides.length - 1);
  const last = Math.max(0, (slides[slide]?.builds.length ?? 1) - 1);
  return { slide, build: Math.min(Math.max(0, at.build), last) };
}

/** The slide called `title`, at `build` (clamped; "last" for its whole
 *  picture): null when the deck has no such slide. */
export function placeOf(slides: readonly DeckSlide[], title: string, build: number | "last"): Place | null {
  const slide = slides.findIndex((s) => s.title === title);
  if (slide === -1) return null;
  const last = Math.max(0, (slides[slide]?.builds.length ?? 1) - 1);
  return { slide, build: build === "last" ? last : Math.min(Math.max(0, build), last) };
}

/** What the presenter reads at a place: the slide's notes, then the build's. */
export function notesAt(slides: readonly DeckSlide[], at: Place): string {
  const s = slides[at.slide];
  if (s === undefined) return "";
  return [s.notes, s.builds[at.build]?.notes ?? ""].filter((n) => n.trim() !== "").join("\n\n");
}

/** The channel two windows on one deck share. */
export function syncName(ws: string, plugin: string, id: string): string {
  return `chimaera-deck:${ws}:${plugin}:${id}`;
}

/** Where a window stands (`at`), or a window asking where the others are. */
export type DeckSync =
  | { kind: "at"; from: string; title: string; build: number }
  | { kind: "where"; from: string };

/** Another window's message, checked: anything else is ignored. */
export function readSync(data: unknown): DeckSync | null {
  if (typeof data !== "object" || data === null) return null;
  const d = data as Record<string, unknown>;
  if (typeof d.from !== "string" || d.from === "") return null;
  if (d.kind === "where") return { kind: "where", from: d.from };
  if (d.kind === "at" && typeof d.title === "string" && typeof d.build === "number" && Number.isInteger(d.build) && d.build >= 0) {
    return { kind: "at", from: d.from, title: d.title, build: d.build };
  }
  return null;
}

/** A running time as the presenter reads it: 4:07, 1:02:15. */
export function clockText(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const two = (n: number) => String(n).padStart(2, "0");
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  return h > 0 ? `${h}:${two(m)}:${two(s % 60)}` : `${m}:${two(s % 60)}`;
}
