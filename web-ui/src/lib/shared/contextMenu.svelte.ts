/**
 * The global right-click context menu: one store, one <ContextMenu/> rendered
 * at App level. A surface's oncontextmenu handler calls openAt(e, items); the
 * component positions itself at the pointer, clamps to the viewport, and
 * closes on outside pointerdown / Escape / selection. Suppressing the native
 * menu is openAt's preventDefault — only elements that attach a handler lose
 * it, so inputs and terminals keep the browser menu.
 */

export interface ContextMenuItem {
  label: string;
  /** Err-tinted destructive row (Delete…). */
  danger?: boolean;
  /** Rendered but inert; `hint` says why (shown as the row's title). */
  disabled?: boolean;
  hint?: string;
  onSelect: () => void;
}

export type ContextMenuEntry = ContextMenuItem | "separator";

let open = $state(false);
let x = $state(0);
let y = $state(0);
let items = $state<ContextMenuEntry[]>([]);

export const contextMenu = {
  get open(): boolean {
    return open;
  },
  get x(): number {
    return x;
  },
  get y(): number {
    return y;
  },
  get items(): ContextMenuEntry[] {
    return items;
  },
  /** Open (or retarget, when already open) at the event's pointer position. */
  openAt(e: MouseEvent, entries: ContextMenuEntry[]): void {
    e.preventDefault();
    e.stopPropagation();
    x = e.clientX;
    y = e.clientY;
    items = entries;
    open = true;
  },
  close(): void {
    open = false;
    items = [];
  },
};
