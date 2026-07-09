/** Svelte action: focus the node as soon as it mounts (confirm buttons,
 *  blocking overlays). */
export function focusOnMount(node: HTMLElement): void {
  node.focus();
}
