/**
 * The global right-click context menu: one store, one <ContextMenu/> rendered
 * at App level. A surface's oncontextmenu handler calls openAt(e, items); the
 * component positions itself at the pointer, clamps to the viewport, and
 * closes on outside pointerdown / Escape / selection. Suppressing the native
 * menu is openAt's preventDefault. ContextMenuHost suppresses unhandled
 * clicks on view chrome while retaining native text-editor menus.
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
  /** A muted second line under the label (where a pick leads). */
  detail?: string;
  onSelect: () => void;
}

export type ContextMenuEntry = ContextMenuItem | "separator";

let open = $state(false);
let x = $state(0);
let y = $state(0);
/** `x` is the menu's right edge (a menu hanging under a button at the
 *  right of its row), not its left. */
let alignRight = $state(false);
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
  get alignRight(): boolean {
    return alignRight;
  },
  /** Open (or retarget, when already open) at the event's pointer position. */
  openAt(e: MouseEvent, entries: ContextMenuEntry[]): void {
    e.preventDefault();
    e.stopPropagation();
    this.openAtPoint(e.clientX, e.clientY, entries);
  },
  /** Open at a viewport point — a control's bottom-left corner (or, with
   *  `alignRight`, its bottom-right), so a menu can hang under the button
   *  that opened it (keyboard-opened too), without fabricating a pointer
   *  event. */
  openAtPoint(px: number, py: number, entries: ContextMenuEntry[], opts: { alignRight?: boolean } = {}): void {
    if (!entries.some((entry) => entry !== "separator")) {
      this.close();
      return;
    }
    x = px;
    y = py;
    alignRight = opts.alignRight === true;
    items = entries;
    open = true;
  },
  close(): void {
    open = false;
    items = [];
  },
};
