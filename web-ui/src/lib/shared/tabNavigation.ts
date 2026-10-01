import type { Action } from "svelte/action";

/** Surface autofocus must not interrupt keyboard navigation in the pane tabs. */
export function paneTabHasKeyboardFocus(): boolean {
  return document.activeElement?.matches('[role="tab"][data-tab-index]:focus-visible') ?? false;
}

/** One tab stop per strip; arrows activate neighbours without scrolling ancestors. */
export const tabNavigation: Action<HTMLElement> = (node) => {
  const onKey = (event: KeyboardEvent): void => {
    if (event.isComposing || event.metaKey || event.ctrlKey || event.altKey) return;
    const target = event.target;
    if (!(target instanceof HTMLElement) || target.getAttribute("role") !== "tab") return;
    const tabs = [...node.querySelectorAll<HTMLElement>('[role="tab"]')]
      .filter((tab) => !tab.matches(":disabled"));
    const index = tabs.indexOf(target);
    if (index < 0) return;
    let next: number;
    switch (event.key) {
      case "ArrowRight": next = (index + 1) % tabs.length; break;
      case "ArrowLeft": next = (index + tabs.length - 1) % tabs.length; break;
      case "Home": next = 0; break;
      case "End": next = tabs.length - 1; break;
      case "Enter":
      case " ": next = index; break;
      default: return;
    }
    event.preventDefault();
    event.stopPropagation();
    tabs[next].focus({ preventScroll: true });
    tabs[next].click();
  };
  node.addEventListener("keydown", onKey);
  return { destroy: () => node.removeEventListener("keydown", onKey) };
};
