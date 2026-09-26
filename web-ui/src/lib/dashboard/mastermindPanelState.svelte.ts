/**
 * The window's Mastermind panel: ONE right-hand panel per window (never per
 * pane), reachable from every view. It never opens on its own — the user's
 * click on the corner icon, ⌘J, or Quick Open does — and a workspace with no
 * Mastermind shows no icon at all.
 *
 * Open/closed is remembered per WINDOW (sessionStorage: survives a reload of
 * this window, not shared with other windows); the width per browser profile
 * (localStorage, the rail-width idiom). Every access is try/caught — private
 * mode or blocked storage degrades to "closed, default width".
 *
 * Runes discipline: state is mutated only through this module's functions.
 */

const OPEN_KEY = "chimaera.mastermind.panelOpen";
const WIDTH_KEY = "chimaera.mastermind.panelWidth";
export const PANEL_MIN = 300;
export const PANEL_DEFAULT = 380;
export const PANEL_MAX = 640;

function readOpen(): boolean {
  try {
    return sessionStorage.getItem(OPEN_KEY) === "1";
  } catch {
    return false;
  }
}

function readWidth(): number {
  try {
    const saved = Number(localStorage.getItem(WIDTH_KEY));
    if (Number.isFinite(saved) && saved >= PANEL_MIN) return Math.min(Math.round(saved), PANEL_MAX);
  } catch {
    // storage blocked: the default below
  }
  return PANEL_DEFAULT;
}

class MastermindPanelState {
  /** The panel is open in this window. */
  open = $state(readOpen());
  /** Preferred width (px); the panel clamps it to what the window allows. */
  width = $state(readWidth());
  /** A Mastermind is bound in this window's workspace (the icon's gate). */
  available = $state(false);
  /** The pane whose tab bar touches the window's top-right corner — the one
   *  place the icon renders (exactly one per window). */
  cornerPaneId = $state<string | null>(null);
  /** The Mastermind needs you (a permission) or finished a reply you haven't
   *  seen — the icon's dot. Nothing else ever signals. */
  attention = $state(false);
}

export const mastermindPanel = new MastermindPanelState();

function persistOpen(open: boolean): void {
  try {
    if (open) sessionStorage.setItem(OPEN_KEY, "1");
    else sessionStorage.removeItem(OPEN_KEY);
  } catch {
    // unpersisted: this window still honours the click
  }
}

export function setMastermindPanelOpen(open: boolean): void {
  if (mastermindPanel.open === open) return;
  mastermindPanel.open = open;
  persistOpen(open);
}

export function toggleMastermindPanel(): void {
  setMastermindPanelOpen(!mastermindPanel.open);
}

/** Commit a drag-resize (clamped) and remember it. */
export function setMastermindPanelWidth(px: number): void {
  const w = Math.min(Math.max(Math.round(px), PANEL_MIN), PANEL_MAX);
  mastermindPanel.width = w;
  try {
    localStorage.setItem(WIDTH_KEY, String(w));
  } catch {
    // unpersisted
  }
}

/** The app publishes what the corner icon needs, once per change. */
export function setMastermindChrome(chrome: {
  available: boolean;
  cornerPaneId: string | null;
  attention: boolean;
}): void {
  if (mastermindPanel.available !== chrome.available) mastermindPanel.available = chrome.available;
  if (mastermindPanel.cornerPaneId !== chrome.cornerPaneId) {
    mastermindPanel.cornerPaneId = chrome.cornerPaneId;
  }
  if (mastermindPanel.attention !== chrome.attention) mastermindPanel.attention = chrome.attention;
}
