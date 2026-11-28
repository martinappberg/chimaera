/**
 * What two windows on one deck say to each other while one presents and the
 * other shows the presenter's view, and the presenter's clock. Pure;
 * `showSync.test.ts`. Shared by the PowerPoint viewer (`../PptxView.svelte`,
 * through `showController.svelte.ts`) and the plugin deck viewer
 * (`plugins/ui/DeckNode.svelte`, through `plugins/ui/deck.ts`).
 */

/** The channel two windows on one deck share: a plugin's deck by its
 *  plugin and node id, a file by `"file"` and its absolute path. */
export function syncName(ws: string, plugin: string, id: string): string {
  return `chimaera-deck:${ws}:${plugin}:${id}`;
}

/** Where a window stands (`at`), or a window asking where the others are.
 *  `title` is the slide's stable key: the deck viewer's slide title, the
 *  PowerPoint viewer's slide part (`ppt/slides/slide3.xml`), which a
 *  reorder does not change; `slide` only tells apart slides that share it. */
export type DeckSync =
  | { kind: "at"; from: string; title: string; slide: number; build: number }
  | { kind: "where"; from: string };

/** Another window's message, checked: anything else is ignored. */
export function readSync(data: unknown): DeckSync | null {
  if (typeof data !== "object" || data === null) return null;
  const d = data as Record<string, unknown>;
  if (typeof d.from !== "string" || d.from === "") return null;
  if (d.kind === "where") return { kind: "where", from: d.from };
  const index = (v: unknown): v is number => typeof v === "number" && Number.isInteger(v) && v >= 0;
  if (d.kind === "at" && typeof d.title === "string" && index(d.build)) {
    // The index only tells same-titled slides apart: without one, the first.
    return { kind: "at", from: d.from, title: d.title, slide: index(d.slide) ? d.slide : 0, build: d.build };
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
