import { writable } from "svelte/store";

export type FindCommand = "open" | "next" | "previous" | "close";
export type FindHandler = (command: FindCommand) => boolean;

const targets = new Map<HTMLElement, FindHandler>();
export const findTargetsChanged = writable(0);

/** A view registers its own search engine; parked tabs never receive commands. */
export function findTarget(node: HTMLElement, handler: FindHandler) {
  targets.set(node, handler);
  findTargetsChanged.update((n) => n + 1);
  return { destroy() {
    targets.delete(node);
    findTargetsChanged.update((n) => n + 1);
  } };
}

export function targetIn(root: HTMLElement | null): FindHandler | null {
  if (root === null) return null;
  const candidates = [...targets].filter(([node]) =>
    root.contains(node) && node.closest("[inert], [hidden], .hidden") === null &&
    node.getClientRects().length > 0);
  // A diff may contain two editors; the one the user is editing owns Find.
  return (candidates.find(([node]) => node.contains(document.activeElement)) ?? candidates[0])?.[1] ?? null;
}

export function findLayer(): HTMLElement | null {
  // Tab can move keyboard focus without a pointer event updating layout.focusedPaneId.
  // Find follows the actual keyboard owner; palette commands use the layout fallback.
  const keyboardPane = document.activeElement?.closest(".pane");
  return keyboardPane?.querySelector<HTMLElement>(".layer.active") ??
    document.querySelector<HTMLElement>(".pane.focused .layer.active");
}

export function findInFocusedPane(command: FindCommand): boolean {
  return targetIn(findLayer())?.(command) ?? false;
}

/** Conventional next/previous keys, without taking shell Ctrl+G or Ctrl+F. */
export function findNavigation(e: KeyboardEvent, terminal: boolean): FindCommand | null {
  if (e.isComposing || e.altKey) return null;
  if (e.key === "F3" && !e.metaKey && !e.ctrlKey) return e.shiftKey ? "previous" : "next";
  if ((e.metaKey && !e.ctrlKey || !terminal && e.ctrlKey && !e.metaKey) && e.key.toLowerCase() === "g") {
    return e.shiftKey ? "previous" : "next";
  }
  if (!terminal && e.ctrlKey && !e.metaKey && !e.shiftKey && e.key.toLowerCase() === "f") return "open";
  return null;
}
