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
 *  reorder does not change; `slide` only tells apart slides that share it.
 *  `lead`: the sender leads the show (the presenter's view, or a window
 *  presenting on its own); `reply`: an answer to `where`, not a move. Both
 *  are absent from the plugin deck viewer's messages (read as false). */
export type DeckSync =
  | { kind: "at"; from: string; title: string; slide: number; build: number; lead?: boolean; reply?: boolean }
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
    return {
      kind: "at",
      from: d.from,
      title: d.title,
      slide: index(d.slide) ? d.slide : 0,
      build: d.build,
      ...(d.lead === true ? { lead: true } : {}),
      ...(d.reply === true ? { reply: true } : {}),
    };
  }
  return null;
}

/** What a window does with another's place.
 *
 *  The show has one leader, the presenter: a follower (the show window)
 *  always takes the leader's place and ignores other followers. A leader
 *  never takes an answer to its `where` (a follower's place is stale when
 *  the presenter joins or rejoins); a move a follower made (a click in the
 *  show window) the leader takes and tells again, so two moves at once
 *  end on the leader's resulting place in both windows. Another leader's
 *  move is taken without telling back (no echo between two leaders). */
export function syncAction(lead: boolean, msg: { lead?: boolean; reply?: boolean }): "take" | "take-and-tell" | "ignore" {
  if (!lead) return msg.lead === true ? "take" : "ignore";
  if (msg.reply === true) return "ignore";
  return msg.lead === true ? "take" : "take-and-tell";
}

/** A running time as the presenter reads it: 4:07, 1:02:15. */
export function clockText(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const two = (n: number) => String(n).padStart(2, "0");
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  return h > 0 ? `${h}:${two(m)}:${two(s % 60)}` : `${m}:${two(s % 60)}`;
}
