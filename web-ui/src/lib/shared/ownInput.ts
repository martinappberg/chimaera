/**
 * When this window last had the person's own press or key — never a press
 * inside a frame, which this window doesn't see. Tracked from the app's start
 * (App imports this), not from when a frame mounts: a page in an answer
 * mounts after the Enter that sent the prompt, and that Enter's activation
 * must still read as the app's, not the page's (`paneFrame.ts`).
 */

let last = Number.NEGATIVE_INFINITY;

const note = (e: Event): void => {
  if (e.isTrusted) last = performance.now();
};

if (typeof window !== "undefined") {
  window.addEventListener("pointerdown", note, true);
  window.addEventListener("keydown", note, true);
}

/** Milliseconds since this window's last own press or key (Infinity: none). */
export function ownInputAgoMs(): number {
  return performance.now() - last;
}
