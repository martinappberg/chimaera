/**
 * Platform-aware chord policy. App chords are Cmd-based on macOS and
 * Ctrl+Shift-based everywhere else; the terminal owns bare Ctrl on every
 * platform — no exceptions. Chords that stack a second layer on top of the
 * base modifier use Shift on macOS (⇧⌘) and Alt elsewhere (Ctrl+Shift+Alt),
 * because Shift is already spent in the base chord there.
 *
 * All tooltip/hint strings come from here so every surface teaches the
 * platform-correct symbols.
 */

interface NavigatorUAData {
  platform?: string;
}

export const isMac: boolean = (() => {
  if (typeof navigator === "undefined") return false;
  const uaData = (navigator as Navigator & { userAgentData?: NavigatorUAData }).userAgentData;
  if (uaData?.platform !== undefined && uaData.platform !== "") {
    return /mac/i.test(uaData.platform);
  }
  return /mac|iphone|ipad/i.test(navigator.platform);
})();

/** Base-modifier label for inline hints ("⌘1–9" / "Ctrl+Shift+1–9"). */
export const MOD_LABEL = isMac ? "⌘" : "Ctrl+Shift+";

function chord(macKeys: string, otherKey: string, layer2 = false): string {
  if (isMac) return (layer2 ? "⇧⌘" : "⌘") + macKeys;
  return (layer2 ? "Ctrl+Shift+Alt+" : "Ctrl+Shift+") + otherKey;
}

/** Display strings for every app chord, used verbatim in tooltips. */
export const KEYS = {
  picker: chord("O", "O"),
  quickOpen: chord("P", "P"),
  openN: chord("1–9", "1–9"),
  newTerminal: chord("E", "E"),
  newAgent: chord("E", "E", true),
  splitRight: chord("D", "D"),
  splitDown: chord("D", "D", true),
  closeView: isMac ? "⌘⌫" : "Ctrl+Shift+Backspace",
  zoom: isMac ? "⇧⌘↩" : "Ctrl+Shift+Alt+Enter",
  focusMode: chord("B", "B"),
  focusArrows: isMac ? "⌥⌘←↑↓→" : "Ctrl+Shift+Alt+←↑↓→",
  cycleTabs: isMac ? "⌥⌘[ ]" : "Ctrl+Shift+Alt+[ ]",
} as const;

/**
 * True when `e` carries the app's base modifier for this platform. Bare
 * Ctrl combos (Ctrl+B, Ctrl+D, Ctrl+O, ...) never match: on macOS the app
 * listens to Cmd only, elsewhere Ctrl must be accompanied by Shift.
 */
export function isAppChord(e: KeyboardEvent): boolean {
  if (isMac) return e.metaKey && !e.ctrlKey;
  return e.ctrlKey && e.shiftKey && !e.metaKey;
}

/**
 * The second chord layer on top of the base modifier: Shift on macOS,
 * Alt elsewhere. Only meaningful when isAppChord(e) is true.
 */
export function isLayer2(e: KeyboardEvent): boolean {
  return isMac ? e.shiftKey : e.altKey;
}

/** Digit 1..9 from the physical key (Shift+digit types symbols on non-mac). */
export function chordDigit(e: KeyboardEvent): number | null {
  const m = /^Digit([1-9])$/.exec(e.code);
  return m === null ? null : Number.parseInt(m[1], 10);
}
