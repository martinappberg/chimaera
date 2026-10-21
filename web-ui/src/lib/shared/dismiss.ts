/**
 * Shared "close on outside pointerdown or Escape" behavior — the window-listener
 * idiom that ChatView's menus and PaneTabs' link-menu both used to hand-roll. A
 * Svelte action so the listeners are torn down with the node.
 *
 * Two inside-tests, matching the two call sites:
 *   - default: a pointerdown inside the HOST node (node.contains) is kept open;
 *   - `keepOpenWithin` set: inside = the target's closest(selector) — use when
 *     the dismissable surfaces live in SIBLING nodes (e.g. several `.menu-host`
 *     chips plus a detached overlay), not inside one host subtree.
 *
 * `region` makes it pane-local (ForkDialog): only events whose target lies in
 * that region count, and a host parked in a hidden keep-alive layer (`inert`)
 * ignores everything. Without it a menu answers Escape from anywhere in the
 * window — right for a transient menu, wrong for a dialog that must not eat
 * another pane's Escape (a terminal's, an editor's).
 */
import type { Action } from "svelte/action";

export type DismissReason = "outside" | "escape";

export interface DismissParams {
  /** Only listen while true (the menu/overlay is open). */
  enabled: boolean;
  /** Called on an outside pointerdown or Escape. The reason lets a menu give
   *  focus back on a keyboard close but leave it to the press on a pointer
   *  one. */
  onDismiss: (reason: DismissReason) => void;
  /** Selector for elements to treat as "inside". When set it REPLACES the
   *  node.contains test (the host node is then just the lifecycle anchor). */
  keepOpenWithin?: string;
  /** The region whose events count (pane-local surfaces); events elsewhere
   *  pass through untouched. Resolved per event: the region can mount late. */
  region?: () => Element | null;
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

  /** False when a pane-local host should leave this event alone. */
  const owns = (target: EventTarget | null): boolean => {
    if (current.region === undefined) return true;
    if (node.closest("[inert]") !== null) return false;
    const region = current.region();
    return region !== null && target instanceof Node && region.contains(target);
  };

  const onDown = (e: PointerEvent) => {
    if (!current.enabled || inside(e.target) || !owns(e.target)) return;
    current.onDismiss("outside");
  };
  const onKey = (e: KeyboardEvent) => {
    if (!current.enabled || e.key !== "Escape" || !owns(e.target)) return;
    e.stopPropagation();
    current.onDismiss("escape");
  };

  // Capture phase mirrors the hand-rolled listeners: dismissal wins before a
  // click inside the transcript can act on it, and Escape never also reaches
  // the surface behind (a chat composer's interrupt).
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
