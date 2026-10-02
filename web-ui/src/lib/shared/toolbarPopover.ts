import { tick } from "svelte";
import type { Action } from "svelte/action";

/** Keep toolbar pickers above pane clipping, with keyboard focus owned by the popup. */
export const toolbarPopover: Action<HTMLElement, { onClose: () => void }> = (node, params) => {
  let current = params;
  const active = document.activeElement;
  const opener = node.parentElement?.querySelector<HTMLElement>("button[aria-haspopup]") ??
    (active instanceof HTMLElement ? active : null);
  let disposed = false;
  const items = () => [...node.querySelectorAll<HTMLButtonElement>("button:not(:disabled)")]
    .filter((item) => item.tabIndex >= 0);
  const place = () => {
    if (opener === null) return;
    const anchor = opener.getBoundingClientRect();
    const rect = node.getBoundingClientRect();
    node.style.left = `${Math.max(8, Math.min(anchor.left, window.innerWidth - rect.width - 8))}px`;
    node.style.top = `${Math.max(8, Math.min(anchor.bottom + 4, window.innerHeight - rect.height - 8))}px`;
  };
  node.setAttribute("popover", "manual");
  Object.assign(node.style, { position: "fixed", inset: "auto", margin: "0", maxWidth: "calc(100vw - 16px)", maxHeight: "min(var(--toolbar-menu-height, 100vh), calc(100vh - 16px))", overflowY: "auto" });
  const topLayer = typeof node.showPopover === "function";
  if (topLayer) node.showPopover();
  void tick().then(() => {
    if (disposed) return;
    place();
    const choices = items();
    const selected = choices.find((item) => item.matches('.current, [aria-checked="true"], [aria-pressed="true"]')) ?? choices[0];
    // A status-only popup still needs to own focus so Tab and focusout close it.
    if (selected === undefined) node.tabIndex = -1;
    (selected ?? node).focus({ preventScroll: true });
  });
  const onKey = (event: KeyboardEvent) => {
    if (event.isComposing || event.metaKey || event.ctrlKey || event.altKey) return;
    if (event.key === "Tab" && node.getAttribute("role") === "menu") {
      opener?.focus({ preventScroll: true });
      current.onClose();
      return;
    }
    const choices = items();
    if (choices.length === 0) return;
    const index = choices.indexOf(document.activeElement as HTMLButtonElement);
    let next: number;
    switch (event.key) {
      case "ArrowDown": case "ArrowRight": next = (index + 1) % choices.length; break;
      case "ArrowUp": case "ArrowLeft": next = (index + choices.length - 1) % choices.length; break;
      case "Home": next = 0; break;
      case "End": next = choices.length - 1; break;
      default: return;
    }
    event.preventDefault();
    event.stopPropagation();
    choices[next].focus();
  };
  const onPointerDown = (event: PointerEvent) => {
    // WebKit focuses the containing pane when a menu button is clicked.
    // Keep focus in the popup until click dispatch, or focusout dismisses the
    // option before its command runs. Touch keeps its native scroll behavior.
    if (event.button === 0 && event.pointerType === "mouse" &&
        event.target instanceof Element && event.target.closest("button")) {
      event.preventDefault();
    }
  };
  const onFocusOut = (event: FocusEvent) => {
    // Use the intended destination: activeElement can briefly be body between
    // blur and focus, which would dismiss a clicked option before its click.
    const next = event.relatedTarget;
    if (next instanceof Node && !node.contains(next) && next !== opener) {
      current.onClose();
    }
  };
  node.addEventListener("pointerdown", onPointerDown);
  node.addEventListener("keydown", onKey);
  node.addEventListener("focusout", onFocusOut);
  window.addEventListener("resize", place);
  return { update(next) { current = next; }, destroy() {
    disposed = true;
    const restore = node.contains(document.activeElement) || document.activeElement === document.body;
    if (topLayer) node.hidePopover();
    if (restore && opener?.isConnected) opener.focus({ preventScroll: true });
    node.removeEventListener("pointerdown", onPointerDown);
    node.removeEventListener("keydown", onKey);
    node.removeEventListener("focusout", onFocusOut);
    window.removeEventListener("resize", place);
  } };
};
