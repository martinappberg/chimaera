export const TAB_FADE_PX = 24;

export interface TabBounds {
  left: number;
  width: number;
}

export function tabInView(scroll: number, view: number, tab: TabBounds): boolean {
  return tab.left >= scroll - 2 && tab.left + tab.width <= scroll + view + 2;
}

/** Fade padding yields to the tab itself in narrow panes. Clamp both edges
 * together: alternating left/right corrections can hide the close button. */
export function revealTabScrollLeft(
  scroll: number,
  view: number,
  content: number,
  tab: TabBounds,
): number {
  const padding = Math.min(TAB_FADE_PX, Math.max(0, (view - tab.width) / 2));
  const min = tab.left + tab.width - view + padding;
  const max = Math.max(min, tab.left - padding);
  return Math.max(0, Math.min(Math.max(0, content - view), Math.max(min, Math.min(max, scroll))));
}

/** A fully revealed tab may fill the strip. Its controls take precedence
 * over fades; clipped tabs still get the normal overflow affordance. */
export function tabFadeWidths(scroll: number, view: number, tab: TabBounds | null) {
  if (tab === null || !tabInView(scroll, view, tab)) {
    return { left: TAB_FADE_PX, right: TAB_FADE_PX };
  }
  return {
    left: Math.min(TAB_FADE_PX, Math.max(0, tab.left - scroll)),
    right: Math.min(TAB_FADE_PX, Math.max(0, scroll + view - tab.left - tab.width)),
  };
}
