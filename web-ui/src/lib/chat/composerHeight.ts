export interface ManualComposerHeight {
  /** Height explicitly chosen with the grip. */
  height: number;
  /** Natural content height when that choice was made. */
  contentHeight: number;
}

/** Resolve the composer height without turning a manual resize into a lock.
 *
 * Expanding the box reserves room until the draft fills it, then content-fit
 * growth resumes. Contracting it preserves that deliberate compact offset,
 * while additional lines still grow the box by the space they require.
 */
export function composerHeightForContent(
  contentHeight: number,
  manual: ManualComposerHeight | null,
  minHeight: number,
  maxHeight: number,
): number {
  let height = contentHeight;
  if (manual !== null) {
    const growthThreshold = Math.max(manual.height, manual.contentHeight);
    height = manual.height + Math.max(0, contentHeight - growthThreshold);
  }
  return Math.max(minHeight, Math.min(maxHeight, height));
}
