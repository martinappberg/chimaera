/**
 * Paging a slide show. While presenting, a hidden slide is skipped, as in
 * PowerPoint and Keynote: the audience never lands on it by paging, though
 * a show started on a hidden slide shows it, and browsing a deck outside a
 * show walks every slide. A slide may have builds (a step a click): paging
 * walks a slide's builds, then moves to the next slide.
 *
 * Pure, shared by the PowerPoint viewer and the plugin deck viewer
 * (`plugins/ui/DeckNode.svelte`); `slideShow.test.ts`.
 */

/** The next slide from `from` in direction `delta` that isn't hidden;
 *  `from` itself when there is none that way. */
export function showStep(hidden: readonly boolean[], from: number, delta: 1 | -1): number {
  for (let i = from + delta; i >= 0 && i < hidden.length; i += delta) {
    if (!hidden[i]) return i;
  }
  return from;
}

/** The show's first or last slide: the outermost one that isn't hidden (the
 *  deck's own end when every slide is hidden). */
export function showEdge(hidden: readonly boolean[], end: "first" | "last"): number {
  const n = hidden.length;
  if (n === 0) return 0;
  if (end === "first") {
    const i = hidden.findIndex((h) => !h);
    return i === -1 ? 0 : i;
  }
  for (let i = n - 1; i >= 0; i--) if (!hidden[i]) return i;
  return n - 1;
}

/** Where `at` stands among the slides of the show ("3 / 12"): hidden slides
 *  aren't counted; standing on one reads as the last shown slide before it. */
export function showPosition(hidden: readonly boolean[], at: number): { n: number; of: number } {
  let n = 0;
  let of = 0;
  for (let i = 0; i < hidden.length; i++) {
    if (hidden[i]) continue;
    of++;
    if (i <= at) n++;
  }
  return { n: Math.max(n, of === 0 ? 0 : 1), of };
}

/** A place in a deck with builds. */
export interface Place {
  slide: number;
  build: number;
}

/**
 * One click forward or back: through the slide's builds, then to the next
 * slide's first build (forward) or the previous slide's last (back).
 * `builds[i]` is slide i's build count (a slide with none counts as one);
 * `skipHidden` is the show's rule. Stays put at either end.
 */
export function deckStep(
  builds: readonly number[],
  hidden: readonly boolean[],
  at: Place,
  delta: 1 | -1,
  skipHidden: boolean,
): Place {
  const count = (i: number) => Math.max(1, builds[i] ?? 1);
  if (at.slide < 0 || at.slide >= builds.length) return at;
  const build = Math.min(Math.max(0, at.build), count(at.slide) - 1);
  if (delta === 1 && build + 1 < count(at.slide)) return { slide: at.slide, build: build + 1 };
  if (delta === -1 && build > 0) return { slide: at.slide, build: build - 1 };
  const flags = skipHidden ? hidden : builds.map(() => false);
  const slide = showStep(flags, at.slide, delta);
  if (slide === at.slide) return { slide: at.slide, build };
  return { slide, build: delta === 1 ? 0 : count(slide) - 1 };
}
