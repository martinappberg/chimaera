/** Svelte action: focus the node as soon as it mounts (confirm buttons,
 *  blocking overlays). `enabled` false skips it, so one of two buttons can
 *  take the initial focus. */
export function focusOnMount(node: HTMLElement, enabled: boolean = true): void {
  if (enabled) node.focus();
}
