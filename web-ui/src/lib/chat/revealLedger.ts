/**
 * Pure arithmetic for the streaming reveal cursor — the trickiest state in the
 * incremental markdown pipeline (Markdown.svelte), extracted so it is testable
 * without a DOM.
 *
 * The model: the streamed message renders as CLOSED prefix segments (DOM never
 * rebuilt) plus one OPEN tail segment (re-rendered per chunk). Words reveal in
 * document order on a ticker; only NOT-yet-revealed words carry spans. Three
 * mutations interact:
 *
 * - `closeSegment(n)`: the head of the open tail (n words) became a closed
 *   segment. Words of it the reader already saw must stay shown (the cursor
 *   carries over); the rest join the prefix-hidden pool.
 * - `rebuildTail(n)`: the open tail re-rendered with n words. The first
 *   `tailRevealed` stay shown — a rebuild must NEVER re-hide or re-fade text
 *   the reader has seen. THE ORDER CONTRACT: every advance that closed
 *   segments MUST rebuild the tail afterwards, even when the new tail's
 *   source is string-equal to the old one (the duplicate-paragraph trap:
 *   `closeSegment` consumed `tailRevealed`, so skipping the rebuild would
 *   leave shown tail words counted as hidden — they'd re-hide and re-fade).
 *   Markdown.svelte enforces this by invalidating its tail memo whenever a
 *   segment closed.
 * - `take()`: one reveal tick — prefix words first (document order), then
 *   tail words, advancing `tailRevealed` for the latter.
 *
 * The DOM-side queues in Markdown.svelte mirror these counts entry-for-entry.
 */

export interface RevealTake {
  fromPrefix: number;
  fromTail: number;
}

export class RevealLedger {
  /** Hidden word spans in closed (prefix) segments. */
  prefixHidden = 0;
  /** Hidden word spans in the current open tail. */
  tailHidden = 0;
  /** Words of the current tail already revealed. */
  tailRevealed = 0;

  get pending(): number {
    return this.prefixHidden + this.tailHidden;
  }

  /** A segment of `totalWords` closed from the HEAD of the open tail. Returns
   *  how many of its words are already shown; the remainder counts as
   *  prefix-hidden. MUST be followed by {@link rebuildTail} in the same
   *  advance (see the order contract above). */
  closeSegment(totalWords: number): number {
    const shown = Math.min(this.tailRevealed, totalWords);
    this.tailRevealed -= shown;
    this.prefixHidden += totalWords - shown;
    return shown;
  }

  /** The open tail re-rendered with `totalWords` words: returns how many stay
   *  shown up front (never re-hiding what the reader saw). */
  rebuildTail(totalWords: number): number {
    const shown = Math.min(this.tailRevealed, totalWords);
    this.tailRevealed = shown;
    this.tailHidden = totalWords - shown;
    return shown;
  }

  /** One reveal tick: a few words, more when the buffer runs ahead — the
   *  stream never lags visibly, it just breathes. Prefix words drain first
   *  (document order), then tail words. */
  take(): RevealTake {
    const pending = this.pending;
    if (pending === 0) return { fromPrefix: 0, fromTail: 0 };
    const take = Math.min(pending, Math.max(2, Math.ceil(pending / 6)));
    const fromPrefix = Math.min(take, this.prefixHidden);
    const fromTail = take - fromPrefix;
    this.prefixHidden -= fromPrefix;
    this.tailHidden -= fromTail;
    this.tailRevealed += fromTail;
    return { fromPrefix, fromTail };
  }

  reset(): void {
    this.prefixHidden = 0;
    this.tailHidden = 0;
    this.tailRevealed = 0;
  }
}
