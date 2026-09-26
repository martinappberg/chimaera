/**
 * Whether a view's pane layer is the one on screen. Pane keeps parked tabs
 * mounted and marks their layer `inert` (layout/Pane.svelte), so a view's
 * recurring work — a poll, a follow tick — watches that flag and stops while
 * parked. Pair with `$pageVisible` for the window itself.
 */
export function watchPaneVisible(node: Element, set: (visible: boolean) => void): () => void {
  const layer = node.closest<HTMLElement>(".layer");
  if (layer === null) {
    set(true);
    return () => {};
  }
  set(!layer.inert);
  const watch = new MutationObserver(() => set(!layer.inert));
  watch.observe(layer, { attributes: true, attributeFilter: ["inert"] });
  return () => watch.disconnect();
}
