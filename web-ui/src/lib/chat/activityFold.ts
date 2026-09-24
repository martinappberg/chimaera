/**
 * Settled activity folds away. A run of thought and tool lines that a reply
 * (or a finished-work line) has since followed is history: it collapses into
 * one quiet line — "Thought, ran 6 commands, read 2 files" — one click from
 * its rows. The trailing run (nothing after it yet) stays open, so live work
 * is always visible; finished-work lines never fold, since they are results
 * the reader came for (and a woken turn's only stated cause).
 */
import { capitalize, countPhrase, type LabelledTool } from "./toolLabels";

/** Runs of at least two activity items that a closing item directly
 *  follows, as half-open `[start, end)` spans. A lone line gains nothing
 *  from folding; an unclosed (trailing) run is still being written. */
export function foldSpans<T>(
  items: readonly T[],
  isActivity: (item: T) => boolean,
  closesRun: (item: T) => boolean,
): [number, number][] {
  const spans: [number, number][] = [];
  let start = -1;
  items.forEach((item, i) => {
    if (isActivity(item)) {
      if (start === -1) start = i;
      return;
    }
    if (start !== -1 && i - start >= 2 && closesRun(item)) spans.push([start, i]);
    start = -1;
  });
  return spans;
}

/** "Thought, ran 6 commands, read 2 files". Thoughts are counted only when
 *  they are all there is. */
export function foldTitle(thoughts: number, tools: LabelledTool[]): string {
  const parts: string[] = [];
  const counted = countPhrase(tools);
  if (thoughts > 0) {
    parts.push(counted === "" && thoughts > 1 ? `thought ${thoughts} times` : "thought");
  }
  if (counted !== "") parts.push(counted);
  return capitalize(parts.join(", "));
}
