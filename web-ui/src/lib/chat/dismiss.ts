/**
 * Shared "close on outside pointerdown or Escape" behavior — the window-listener
 * idiom that ChatView's menus and PaneTabs' link-menu each hand-rolled. A Svelte
 * action so the listeners are torn down with the node.
 *
 * Two inside-tests, matching the two call sites:
 *   - default: a pointerdown inside the HOST node (node.contains) is kept open;
 *   - `keepOpenWithin` set: inside = the target's closest(selector) — use when
 *     the dismissable surfaces live in SIBLING nodes (e.g. several `.menu-host`
 *     chips plus a detached overlay), not inside one host subtree.
 *
 * PaneTabs owns its own copy today; it could adopt this later.
 */
import type { Action } from "svelte/action";

export interface DismissParams {
  /** Only listen while true (the menu/overlay is open). */
  enabled: boolean;
  /** Called on an outside pointerdown or Escape. */
  onDismiss: () => void;
  /** Selector for elements to treat as "inside". When set it REPLACES the
   *  node.contains test (the host node is then just the lifecycle anchor). */
  keepOpenWithin?: string;
}

export const dismiss: Action<HTMLElement, DismissParams> = (node, params) => {
  let current = params;

  const inside = (target: EventTarget | null): boolean => {
    if (!(target instanceof Element)) return false;
    if (current.keepOpenWithin !== undefined) {
      return target.closest(current.keepOpenWithin) !== null;
    }
    return node.contains(target);
  };

  const onDown = (e: PointerEvent) => {
    if (!current.enabled || inside(e.target)) return;
    current.onDismiss();
  };
  const onKey = (e: KeyboardEvent) => {
    if (!current.enabled) return;
    if (e.key === "Escape") {
      e.stopPropagation();
      current.onDismiss();
    }
  };

  // Capture phase mirrors the hand-rolled listeners: dismissal wins before a
  // click inside the transcript can act on it.
  window.addEventListener("pointerdown", onDown, true);
  window.addEventListener("keydown", onKey, true);
  return {
    update(next: DismissParams) {
      current = next;
    },
    destroy() {
      window.removeEventListener("pointerdown", onDown, true);
      window.removeEventListener("keydown", onKey, true);
    },
  };
};
