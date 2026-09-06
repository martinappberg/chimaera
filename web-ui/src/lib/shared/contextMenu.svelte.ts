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
  /** A pick-list row's state mark: `true` draws a check in the gutter, `false`
   *  reserves the gutter so labels align. Undefined rows (ordinary actions)
   *  have no gutter at all — a menu grows one only when some row is checkable. */
  checked?: boolean;
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
    this.openAtPoint(e.clientX, e.clientY, entries);
  },
  /** Open at a viewport point — a control's bottom-left corner, so a menu can
   *  hang under the button that opened it (keyboard-opened too), without
   *  fabricating a pointer event. */
  openAtPoint(px: number, py: number, entries: ContextMenuEntry[]): void {
    x = px;
    y = py;
    items = entries;
    open = true;
  },
  close(): void {
    open = false;
    items = [];
  },
};
