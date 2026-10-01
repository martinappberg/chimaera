export const TAB_FADE_PX = 24;

export interface TabBounds {
  left: number;
  width: number;
}

export function tabInView(scroll: number, view: number, tab: TabBounds): boolean {
  return tab.left >= scroll - 2 && tab.left + tab.width <= scroll + view + 2;
}

/** Reveal a clipped tab from its leading edge, rather than leaving an empty
 * tail of the preceding tab before it. Already visible tabs stay put; a tab
 * wider than the strip gives its close button priority. */
export function revealTabScrollLeft(
  scroll: number,
  view: number,
  content: number,
  tab: TabBounds,
): number {
  const next = tab.width > view ? tab.left + tab.width - view
    : tabInView(scroll, view, tab) ? scroll : tab.left;
  return Math.max(0, Math.min(Math.max(0, content - view), next));
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
