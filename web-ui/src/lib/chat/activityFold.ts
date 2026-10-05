/**
 * Settled activity folds away. A run of thought and tool lines that any other
 * row has since followed (a reply, a finished-work line, a permission
 * decision, a message sent mid-turn) is history: it collapses into
 * one quiet line — "Thought, ran 6 commands, read 2 files" — one click from
 * its rows. The trailing run (nothing after it yet) stays open, so live work
 * is always visible. Finished-work lines never join that fold: they are
 * results the reader came for (and a woken turn's only stated cause). A long
 * run of them is a list, though — several background tasks ending together —
 * so three or more that something has since followed fold on their own, into
 * a line that still says what ended and how (`finishedTitle`).
 */
import { capitalize, countPhrase, type LabelledTool } from "./toolLabels";

/** Runs of at least `min` (two) activity items that a closing item directly
 *  follows, as half-open `[start, end)` spans. A lone line gains nothing
 *  from folding; an unclosed (trailing) run is still being written. */
export function foldSpans<T>(
  items: readonly T[],
  isActivity: (item: T) => boolean,
  closesRun: (item: T) => boolean,
  min = 2,
): [number, number][] {
  const spans: [number, number][] = [];
  let start = -1;
  items.forEach((item, i) => {
    if (isActivity(item)) {
      if (start === -1) start = i;
      return;
    }
    if (start !== -1 && i - start >= min && closesRun(item)) spans.push([start, i]);
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

/** Finished-work lines fold from this many on: two are still read at a
 *  glance, and each carries its own output link or report. */
export const FINISHED_FOLD_MIN = 3;

const FINISHED_NOUNS = [
  { source: "agent", one: "agent", many: "agents" },
  { source: "task", one: "background task", many: "background tasks" },
  { source: "monitor", one: "monitor", many: "monitors" },
] as const;
/** Good news first; any other status the wire sends follows, verbatim. */
const FINISHED_STATUSES = ["completed", "failed", "stopped"];

/** "1 agent finished, 2 background tasks finished, 5 stopped": what ended,
 *  by kind and by how. The kind is named once per kind. */
export function finishedTitle(rows: readonly { source: string; status: string }[]): string {
  const parts: string[] = [];
  for (const noun of FINISHED_NOUNS) {
    const of = rows.filter((row) => row.source === noun.source);
    const statuses = [
      ...FINISHED_STATUSES,
      ...of.map((row) => row.status).filter((status) => !FINISHED_STATUSES.includes(status)),
    ];
    let named = false;
    for (const status of new Set(statuses)) {
      const n = of.filter((row) => row.status === status).length;
      if (n === 0) continue;
      const how = status === "completed" ? "finished" : status;
      parts.push(named ? `${n} ${how}` : `${n} ${n === 1 ? noun.one : noun.many} ${how}`);
      named = true;
    }
  }
  return parts.join(", ");
}
