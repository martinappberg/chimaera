/**
 * Give focus back to the element that had it when a transient surface
 * opened: `use:returnFocus` on the surface's root. This is the restore half
 * of `modalFocus.ts`, for a surface that is NOT a window modal — a
 * pane-local dialog, a popover — so it must neither trap Tab nor stand
 * App's chords down (`modalOpen()` stays false).
 *
 * The opener is recorded synchronously at mount, before a `focusOnMount`
 * microtask moves focus into the surface. On destroy, focus returns to the
 * opener only when it is still inside the surface or already on the body: a
 * user who moved focus elsewhere (another pane) keeps it.
 */
import type { Action } from "svelte/action";

export const returnFocus: Action<HTMLElement> = (node) => {
  const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  return {
    destroy() {
      const active = document.activeElement;
      if (active !== null && active !== document.body && !node.contains(active)) return;
      queueMicrotask(() => {
        if (opener?.isConnected) opener.focus({ preventScroll: true });
      });
    },
  };
};
