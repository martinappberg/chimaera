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

/** An inline embed, `![alt](target)`: the target is its first token. */
const EMBED = /!\[[^\]]*\]\(\s*<?([^)\s>]*)[^)]*\)/g;
const PICTURE_EXTS = new Set(["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "avif"]);
const CHIP_EXTS = new Set(["md", "markdown", "docx", "pptx"]);

/** What a message's inline embeds add, in lines: a card, not a line of text
 *  — a figure up to ~420px tall (18), a PDF page up to ~560px (26), an
 *  excerpt card for a table, code, a report or a notebook (14); a document
 *  is an inline chip (none). On a figure-heavy reply these are most of its
 *  height, and weighing them as text left the estimate swinging by tens of
 *  thousands of px with whichever window calibrated it. */
function embedLines(text: string): number {
  if (!text.includes("![")) return 0;
  let lines = 0;
  for (const m of text.matchAll(EMBED)) {
    const target = (m[1] ?? "").split(/[#?]/)[0].toLowerCase();
    const ext = target.slice(target.lastIndexOf(".") + 1);
    if (PICTURE_EXTS.has(ext)) lines += 18;
    else if (ext === "pdf") lines += 26;
    else if (!CHIP_EXTS.has(ext)) lines += 14;
  }
  return lines;
}

/** Markdown's length as rendered, roughly: a link's target never shows (an
 *  agent citing files writes `[plan.md](/long/absolute/path/plan.md)`, the
 *  path often longer than the words), nor does emphasis or code markup. */
function renderedMarkdown(text: string): string {
  return text
    .replace(EMBED, "")
    .replace(/\]\([^)\s]*\)/g, "]")
    .replace(/[*`#~[\]]/g, "");
}

/** Thought and tool rows: a settled run of them folds into one line under
 *  the reply that followed (activityFold.ts), and a run of tool calls is one
 *  group line even unfolded. */
function isActivity(block: ChatBlock | null): boolean {
  return block?.kind === "tool" || block?.kind === "thought";
}

/** Rough rendered height of one block, in lines. `previous` matters because
 *  a settled run of activity rows (thoughts and tool calls) shares one line.
 *  `settled` is false for the trailing run no reply has followed yet — it
 *  renders unfolded, a line per thought and per group of tool calls — which
 *  only the live tail has (its window calibrates the model). */
export function blockWeight(
  block: ChatBlock,
  previous: ChatBlock | null,
  charsPerLine: number,
  settled = true,
): number {
  switch (block.kind) {
    case "message":
      return textLines(renderedMarkdown(block.text), charsPerLine) + embedLines(block.text) + 1;
    case "user":
      // Bubbles wrap narrower than the column and carry padding; a row of
      // picture tiles (AttachmentStrip, 112px) sits above one with images.
      return (
        textLines(block.text, charsPerLine * 0.75) +
        1.5 +
        (block.attachmentPaths.length > 0 ? 5.5 : 0)
      );
    case "tool":
      return (settled ? isActivity(previous) : previous?.kind === "tool") ? 0 : 1;
    case "thought":
      return settled && isActivity(previous) ? 0 : 1;
    case "question":
      return block.resolved ? 2 + 2 * block.questions.length : 0;
    case "turn_end":
      // The written-files line (a chip per file, wrapping to a second line
      // past a handful); a bare turn end renders nothing.
      return block.artifacts.length > 0 || block.mentioned.length > 0 ? 2 : 0;
    case "usage":
      return 4;
    case "notice":
      return textLines(block.text, charsPerLine) + 0.5;
    case "wake":
    case "finished":
      return 1;
  }
}

/** Weights of the blocks from `from` to the end, NOT cached: that stretch
 *  includes the live tail (a reply still streaming, a run not yet folded),
 *  which the prefix sums below must never freeze. `at` maps a weight into
 *  the stretch back to its block (the index `from` counts from). */
export function tailWeights(
  blocks: readonly ChatBlock[],
  from: number,
  charsPerLine: number,
): { total: number; at(weight: number): number } {
  const start = Math.max(0, Math.min(from, blocks.length));
  const prefix = [0];
  for (let i = start; i < blocks.length; i++) {
    const weight = blockWeight(blocks[i], i > 0 ? blocks[i - 1] : null, charsPerLine);
    prefix.push(prefix[prefix.length - 1] + weight);
  }
  return {
    total: prefix[prefix.length - 1],
    at(weight: number): number {
      let lo = 0;
      let hi = prefix.length - 1;
      while (lo < hi) {
        const mid = (lo + hi + 1) >> 1;
        if (prefix[mid] <= weight) lo = mid;
        else hi = mid - 1;
      }
      return start + Math.min(lo, Math.max(0, prefix.length - 2));
    },
  };
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
