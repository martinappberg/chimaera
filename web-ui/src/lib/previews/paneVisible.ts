/**
 * Whether a view's pane layer is the one on screen. Pane keeps parked tabs
 * mounted and marks their layer `inert` (layout/Pane.svelte), so a view's
 * recurring work — a poll, a follow tick — watches that flag and stops while
 * parked. Pair with `$pageVisible` for the window itself.
 *
 * Views inside a Pane get the same answer as their `visible` prop; this is
 * the fallback for a view mounted outside one (an embed, a hover preview)
 * or one whose host does not pass the prop. Components use it through
 * `viewLifecycle.svelte.ts` (`paneVisibility`), not directly.
 */

/** The slice of `MutationObserver` this needs (a test passes a fake). */
export type InertObserver = new (callback: () => void) => {
  observe(target: Node, options: MutationObserverInit): void;
  disconnect(): void;
};

export function watchPaneVisible(
  node: Element,
  set: (visible: boolean) => void,
  Observer: InertObserver = MutationObserver,
): () => void {
  const layer = node.closest<HTMLElement>(".layer");
  if (layer === null) {
    set(true);
    return () => {};
  }
  set(!layer.inert);
  const watch = new Observer(() => set(!layer.inert));
  watch.observe(layer, { attributes: true, attributeFilter: ["inert"] });
  return () => watch.disconnect();
}
