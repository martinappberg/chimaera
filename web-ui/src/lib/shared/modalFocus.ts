import type { Action } from "svelte/action";

interface ModalFocusOptions {
  /** Higher-priority blocking prompts stay active over ordinary modal stacks. */
  priority?: number;
}

interface Trap {
  node: HTMLElement;
  previous: HTMLElement | null;
  priority: number;
  order: number;
}

const FOCUSABLE = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(",");

function focusableWithin(node: HTMLElement): HTMLElement[] {
  return [...node.querySelectorAll<HTMLElement>(FOCUSABLE)].filter(
    (el) => el.getClientRects().length > 0 && el.getAttribute("aria-hidden") !== "true",
  );
}

let traps: Trap[] = [];
let nextOrder = 0;

function topTrap(): Trap | null {
  let top: Trap | null = null;
  for (const trap of traps) {
    if (
      top === null ||
      trap.priority > top.priority ||
      (trap.priority === top.priority && trap.order > top.order)
    ) {
      top = trap;
    }
  }
  return top;
}

function focusFirst(trap: Trap): void {
  (focusableWithin(trap.node)[0] ?? trap.node).focus();
}

function onKeydown(event: KeyboardEvent): void {
  if (event.key !== "Tab") return;
  const trap = topTrap();
  if (trap === null) return;
  const items = focusableWithin(trap.node);
  if (items.length === 0) {
    event.preventDefault();
    trap.node.focus();
    return;
  }
  const first = items[0];
  const last = items[items.length - 1];
  const active = document.activeElement;
  if (event.shiftKey && (active === first || !trap.node.contains(active))) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && (active === last || !trap.node.contains(active))) {
    event.preventDefault();
    first.focus();
  }
}

function onFocusin(event: FocusEvent): void {
  const trap = topTrap();
  if (trap === null || (event.target instanceof Node && trap.node.contains(event.target))) return;
  focusFirst(trap);
}

function syncDocumentListeners(): void {
  if (traps.length === 1) {
    document.addEventListener("keydown", onKeydown, true);
    document.addEventListener("focusin", onFocusin, true);
  } else if (traps.length === 0) {
    document.removeEventListener("keydown", onKeydown, true);
    document.removeEventListener("focusin", onFocusin, true);
  }
}

/**
 * Trap Tab focus inside a modal and restore the previous focus on close.
 * Initial focus remains the component's choice (`focusOnMount`); this action
 * owns only containment and restoration, so destructive dialogs can keep
 * landing on their safe Cancel button.
 */
export const modalFocus: Action<HTMLElement, ModalFocusOptions | undefined> = (
  node,
  options,
) => {
  const trap: Trap = {
    node,
    previous: document.activeElement instanceof HTMLElement ? document.activeElement : null,
    priority: options?.priority ?? 0,
    order: nextOrder++,
  };
  traps.push(trap);
  syncDocumentListeners();
  return {
    update(next) {
      trap.priority = next?.priority ?? 0;
    },
    destroy() {
      const wasTop = topTrap() === trap;
      traps = traps.filter((candidate) => candidate !== trap);
      // If a modal underneath another one disappears, preserve the restore
      // chain. The top modal must eventually return to the original invoking
      // control, not to a detached element inside the removed modal.
      for (const candidate of traps) {
        if (candidate.previous !== null && node.contains(candidate.previous)) {
          candidate.previous = trap.previous;
        }
      }
      syncDocumentListeners();
      if (!wasTop) return;
      queueMicrotask(() => {
        const next = topTrap();
        if (next !== null) {
          if (trap.previous?.isConnected && next.node.contains(trap.previous)) {
            trap.previous.focus();
          } else if (!next.node.contains(document.activeElement)) {
            focusFirst(next);
          }
        } else if (trap.previous?.isConnected) {
          trap.previous.focus();
        }
      });
    },
  };
};
