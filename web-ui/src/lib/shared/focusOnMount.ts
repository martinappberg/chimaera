/** Svelte action: focus the node as soon as it mounts (confirm buttons,
 *  blocking overlays). `enabled` false skips it, so one of two buttons can
 *  take the initial focus. A microtask later, not at once: a child's action
 *  runs before its modal's, and `modalFocus` must record the focus it hands
 *  back on close — the control that opened it, not this button. */
export function focusOnMount(node: HTMLElement, enabled: boolean = true): void {
  if (enabled) queueMicrotask(() => node.focus());
}
