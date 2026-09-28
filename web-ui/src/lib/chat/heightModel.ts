import type { ChatBlock } from "./store.svelte";

/**
 * A content-based height model for transcript blocks that are NOT mounted —
 * what the history spacer stands in for. A flat per-block average taken from
 * the rendered window is badly wrong whenever history is uneven (a prose-heavy
 * tail over a tool-heavy past measured 4× off), and every px of error shows up
 * as blank space at the top of history or an early stop. Weights are in
 * rough text lines; only their relative size matters, because the caller
 * calibrates them against the rendered window's measured height.
 */

/** Visual lines of `text` wrapped at `charsPerLine`: its length over the
 *  measure, plus a half line per hard break (code, lists, short paragraphs). */
function textLines(text: string, charsPerLine: number): number {
  let breaks = 0;
  for (let at = text.indexOf("\n"); at !== -1; at = text.indexOf("\n", at + 1)) breaks++;
  return Math.ceil(text.length / Math.max(8, charsPerLine)) + breaks * 0.5;
}

/** Rough rendered height of one block, in lines. `previous` matters because
 *  consecutive tool calls share one group line. */
export function blockWeight(
  block: ChatBlock,
  previous: ChatBlock | null,
  charsPerLine: number,
): number {
  switch (block.kind) {
    case "message":
      return textLines(block.text, charsPerLine) + 1;
    case "user":
      // Bubbles wrap narrower than the column and carry padding; a row of
      // picture tiles (AttachmentStrip, 112px) sits above one with images.
      return (
        textLines(block.text, charsPerLine * 0.75) +
        1.5 +
        (block.attachmentPaths.length > 0 ? 5.5 : 0)
      );
    case "tool":
      return previous?.kind === "tool" ? 0 : 1;
    case "question":
      return block.resolved ? 2 + 2 * block.questions.length : 0;
    case "turn_end":
      // The artifact gallery; a bare turn end renders nothing.
      return block.artifacts.length > 0 ? 12 : 0;
    case "usage":
      return 4;
    case "notice":
      return textLines(block.text, charsPerLine) + 0.5;
    case "thought":
    case "wake":
    case "finished":
      return 1;
  }
}

/** Prefix sums of {@link blockWeight} over the reducer's blocks, extended
 *  incrementally. Rebuilt when the array's front moved (a cap trim or a
 *  journal reset) or the measure changed. Rows that already have a prefix
 *  are not re-weighed when they change in place — history above the window
 *  is settled, and the estimate is recalibrated at every use anyway. */
export class HistoryWeights {
  private prefix: number[] = [0];
  private key = "";

  /** Summed weight of blocks [0, n). */
  upTo(blocks: readonly ChatBlock[], n: number, charsPerLine: number, generation: string): number {
    const key = `${generation}|${Math.round(charsPerLine)}`;
    if (key !== this.key) {
      this.key = key;
      this.prefix = [0];
    }
    const target = Math.max(0, Math.min(n, blocks.length));
    for (let i = this.prefix.length - 1; i < target; i++) {
      this.prefix.push(
        this.prefix[i] + blockWeight(blocks[i], i > 0 ? blocks[i - 1] : null, charsPerLine),
      );
    }
    return this.prefix[target];
  }

  /** The block at `weight` into the weighed prefix: the last i whose
   *  preceding blocks weigh no more than `weight`. */
  indexAt(weight: number): number {
    let lo = 0;
    let hi = this.prefix.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (this.prefix[mid] <= weight) lo = mid;
      else hi = mid - 1;
    }
    return Math.min(lo, Math.max(0, this.prefix.length - 2));
  }
}
