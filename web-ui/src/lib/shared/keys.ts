/**
 * The chord engine: every app keybinding as data. An ACTION carries a default
 * chord string; the user overrides it (or disables it with "") through the
 * `keys.*` settings, and picks the base modifier with `keys.modifier`. This
 * module is pure — no settings imports (schema.ts derives the `keys.*`
 * settings FROM this registry, so an import the other way would cycle);
 * keybindings.ts joins the registry with the live settings.
 *
 * Chord strings are "+"-joined tokens, modifiers first, one key token last:
 *   "Mod+e"  "Mod2+Enter"  "Mod+Alt+["  "Meta+Shift+r"  "Mod+Arrow"
 * `Mod` is the user-selected base modifier (Cmd on macOS / Ctrl+Shift
 * elsewhere under "auto"); `Mod2` stacks the second layer on top (Shift,
 * or Alt when the base already spends Shift). Concrete modifier names
 * (Meta/Ctrl/Alt/Shift) pin a chord regardless of the modifier setting —
 * captures from the rebinding UI are stored concrete. The "Arrow" key token
 * is a wildcard for all four arrows; the matcher reports which one fired.
 *
 * Policy invariant: the terminal owns bare Ctrl on every platform, so no
 * modifier option resolves to Ctrl alone.
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

// --- the action registry -----------------------------------------------------

export type ModifierSetting = "auto" | "cmd" | "ctrl-shift" | "alt";

export interface ActionDef {
  /** Registry id; the settings key is `keys.<id>`. */
  id: string;
  /** Row title in the settings UI ("Close view"). */
  label: string;
  /** One-line description for the settings row. */
  description: string;
  /** Default chord string ("" = unbound by default). */
  def: string;
  /** Arrow-set actions bind all four arrows at once ("…+Arrow"). */
  arrowSet?: boolean;
}

/**
 * Rebindable actions, in match priority order (when two bindings collide on
 * the same chord, the earlier action wins). openN (Mod+1–9), the reference
 * chord, and the terminal font chords (Mod +/−/0) are spec-pinned and are
 * not listed here.
 */
export const ACTIONS = [
  {
    id: "picker",
    label: "Open Folder Picker",
    description: "Toggle the workspace folder picker.",
    def: "Mod+o",
  },
  {
    id: "quickOpen",
    label: "Quick Open",
    description: "Toggle the files + sessions palette.",
    def: "Mod+p",
  },
  {
    id: "settings",
    label: "Open Settings",
    description: "Open this settings surface.",
    def: "Mod+,",
  },
  {
    id: "newTerminal",
    label: "New Terminal",
    description: "Start a shell in the focused pane's workspace.",
    def: "Mod+e",
  },
  {
    id: "newAgent",
    label: "New Agent",
    description: "Start the default agent.",
    def: "Mod2+e",
  },
  {
    id: "splitRight",
    label: "Split Right",
    description: "Split the focused pane horizontally.",
    def: "Mod+d",
  },
  {
    id: "splitDown",
    label: "Split Down",
    description: "Split the focused pane vertically.",
    def: "Mod2+d",
  },
  {
    id: "closeView",
    label: "Close View",
    description:
      "Close the focused pane's active tab (a pane closes with its last tab). The native app also binds ⌘W in its menu; browsers reserve ⌘W/⌘T for themselves, so those only work in the app.",
    def: "Mod+Backspace",
  },
  {
    id: "zoom",
    label: "Zoom Pane",
    description: "Toggle the focused pane full-stage.",
    def: "Mod2+Enter",
  },
  {
    id: "focusMode",
    label: "Focus Mode",
    description: "Hide or show the sidebar.",
    def: "Mod+b",
  },
  {
    id: "cyclePrev",
    label: "Previous Tab",
    description: "Activate the previous tab in the focused pane.",
    def: "Mod+Alt+[",
  },
  {
    id: "cycleNext",
    label: "Next Tab",
    description: "Activate the next tab in the focused pane.",
    def: "Mod+Alt+]",
  },
  {
    id: "focusArrows",
    label: "Move Pane Focus",
    description:
      "Switch focus to the neighboring pane (all four arrows) — the spatial cousin of Mod+1–9.",
    def: "Mod+Arrow",
    arrowSet: true,
  },
  {
    id: "moveTab",
    label: "Move Tab to Pane",
    description:
      "Carry the focused pane's active tab into the neighboring pane (all four arrows).",
    def: "Mod2+Arrow",
    arrowSet: true,
  },
] as const satisfies readonly ActionDef[];

export type ActionId = (typeof ACTIONS)[number]["id"];

export const ACTION_BY_ID: ReadonlyMap<string, ActionDef> = new Map(
  ACTIONS.map((a) => [a.id, a]),
);

// --- chord parsing & matching ------------------------------------------------

export interface ParsedChord {
  meta: boolean;
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  /** Normalized key token; "Arrow" is the four-arrow wildcard. */
  key: string;
}

/** The concrete modifiers `Mod` stands for under a modifier setting. */
export function resolveMod(setting: ModifierSetting): Pick<ParsedChord, "meta" | "ctrl" | "alt" | "shift"> {
  switch (setting) {
    case "cmd":
      return { meta: true, ctrl: false, alt: false, shift: false };
    case "ctrl-shift":
      return { meta: false, ctrl: true, alt: false, shift: true };
    case "alt":
      return { meta: false, ctrl: false, alt: true, shift: false };
    case "auto":
      return isMac
        ? { meta: true, ctrl: false, alt: false, shift: false }
        : { meta: false, ctrl: true, alt: false, shift: true };
  }
}

const NAMED_KEYS = new Set([
  "Enter",
  "Backspace",
  "Delete",
  "Escape",
  "Space",
  "Tab",
  "Home",
  "End",
  "PageUp",
  "PageDown",
  "Arrow",
  "ArrowLeft",
  "ArrowRight",
  "ArrowUp",
  "ArrowDown",
]);

function isKeyToken(t: string): boolean {
  if (/^[a-z0-9[\],.;'`/\\=-]$/.test(t)) return true;
  if (/^F([1-9]|1[0-2])$/.test(t)) return true;
  return NAMED_KEYS.has(t);
}

/**
 * Parse a chord string under a modifier setting. Returns null when the
 * string is not a valid chord (settings sanitize rejects those). A chord
 * needs at least one of Meta/Ctrl/Alt — Shift alone would collide with
 * typing — except for F-keys, which may bind bare.
 */
export function parseChord(chord: string, setting: ModifierSetting): ParsedChord | null {
  if (chord === "") return null;
  const parts = chord.split("+");
  const key = parts.pop();
  if (key === undefined || !isKeyToken(key)) return null;
  const out: ParsedChord = { meta: false, ctrl: false, alt: false, shift: false, key };
  for (const part of parts) {
    switch (part) {
      case "Mod": {
        const m = resolveMod(setting);
        out.meta ||= m.meta;
        out.ctrl ||= m.ctrl;
        out.alt ||= m.alt;
        out.shift ||= m.shift;
        break;
      }
      case "Mod2": {
        const m = resolveMod(setting);
        out.meta ||= m.meta;
        out.ctrl ||= m.ctrl;
        out.alt ||= m.alt;
        out.shift ||= m.shift;
        // The second layer: Shift, or Alt when the base already spends it.
        if (m.shift) out.alt = true;
        else out.shift = true;
        break;
      }
      case "Meta":
        out.meta = true;
        break;
      case "Ctrl":
        out.ctrl = true;
        break;
      case "Alt":
        out.alt = true;
        break;
      case "Shift":
        out.shift = true;
        break;
      default:
        return null;
    }
  }
  const fkey = /^F([1-9]|1[0-2])$/.test(out.key);
  if (!out.meta && !out.ctrl && !out.alt && !fkey) return null;
  return out;
}

/**
 * The event's key, normalized to a chord key token via the PHYSICAL key
 * where layouts shift things around (letters, digits, brackets), so a
 * binding made on QWERTY still lands elsewhere. Null for modifier-only
 * events and keys the chord grammar doesn't cover.
 */
export function eventKeyToken(e: KeyboardEvent): string | null {
  const code = e.code;
  let m = /^Key([A-Z])$/.exec(code);
  if (m !== null) return m[1].toLowerCase();
  m = /^Digit([0-9])$/.exec(code);
  if (m !== null) return m[1];
  const byCode: Record<string, string> = {
    BracketLeft: "[",
    BracketRight: "]",
    Comma: ",",
    Period: ".",
    Semicolon: ";",
    Quote: "'",
    Backquote: "`",
    Slash: "/",
    Backslash: "\\",
    Minus: "-",
    Equal: "=",
  };
  if (code in byCode) return byCode[code];
  if (e.key === " ") return "Space";
  if (/^F([1-9]|1[0-2])$/.test(e.key)) return e.key;
  if (NAMED_KEYS.has(e.key)) return e.key;
  return null;
}

export type ArrowDir = "left" | "right" | "up" | "down";

/**
 * Match an event against a parsed chord. Modifier state must match exactly
 * (a chord with Shift never fires without it, and vice versa). Returns the
 * arrow direction for wildcard-Arrow chords, "hit" otherwise, null on miss.
 */
export function matchChord(e: KeyboardEvent, c: ParsedChord): ArrowDir | "hit" | null {
  if (e.metaKey !== c.meta || e.ctrlKey !== c.ctrl || e.altKey !== c.alt || e.shiftKey !== c.shift) {
    return null;
  }
  const token = eventKeyToken(e);
  if (token === null) return null;
  if (c.key === "Arrow") {
    const dirs: Record<string, ArrowDir> = {
      ArrowLeft: "left",
      ArrowRight: "right",
      ArrowUp: "up",
      ArrowDown: "down",
    };
    return dirs[token] ?? null;
  }
  return token === c.key ? "hit" : null;
}

/**
 * A captured chord string from a keydown in the rebinding UI, concrete
 * modifiers in canonical order. Null when the event can't be a chord
 * (modifier-only, unsupported key, or no non-Shift modifier on a non-F key).
 * `arrowSet` collapses any arrow into the "Arrow" wildcard.
 */
export function captureChord(e: KeyboardEvent, arrowSet: boolean): string | null {
  let token = eventKeyToken(e);
  if (token === null) return null;
  if (arrowSet && token.startsWith("Arrow")) token = "Arrow";
  else if (!arrowSet && token === "Arrow") return null;
  const mods: string[] = [];
  if (e.ctrlKey) mods.push("Ctrl");
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey) mods.push("Shift");
  if (e.metaKey) mods.push("Meta");
  const fkey = /^F([1-9]|1[0-2])$/.test(token);
  if (!e.metaKey && !e.ctrlKey && !e.altKey && !fkey) return null;
  return [...mods, token].join("+");
}

// --- display -----------------------------------------------------------------

const MAC_KEY_GLYPHS: Record<string, string> = {
  Enter: "↩",
  Backspace: "⌫",
  Delete: "⌦",
  Escape: "⎋",
  Space: "␣",
  Tab: "⇥",
  Home: "↖",
  End: "↘",
  PageUp: "⇞",
  PageDown: "⇟",
  Arrow: "←↑↓→",
  ArrowLeft: "←",
  ArrowRight: "→",
  ArrowUp: "↑",
  ArrowDown: "↓",
};

const OTHER_KEY_NAMES: Record<string, string> = {
  Arrow: "←↑↓→",
  ArrowLeft: "←",
  ArrowRight: "→",
  ArrowUp: "↑",
  ArrowDown: "↓",
};

/** Human chord label — mac symbols (⌃⌥⇧⌘E) or Ctrl+Shift+E style. */
export function displayChord(chord: string, setting: ModifierSetting): string {
  const parsed = parseChord(chord, setting);
  if (parsed === null) return chord;
  const keyLabel = parsed.key.length === 1 ? parsed.key.toUpperCase() : parsed.key;
  if (isMac) {
    return (
      (parsed.ctrl ? "⌃" : "") +
      (parsed.alt ? "⌥" : "") +
      (parsed.shift ? "⇧" : "") +
      (parsed.meta ? "⌘" : "") +
      (MAC_KEY_GLYPHS[parsed.key] ?? keyLabel)
    );
  }
  const mods = [
    parsed.ctrl ? "Ctrl" : null,
    parsed.alt ? "Alt" : null,
    parsed.shift ? "Shift" : null,
    parsed.meta ? "Meta" : null,
  ].filter((x): x is string => x !== null);
  return [...mods, OTHER_KEY_NAMES[parsed.key] ?? keyLabel].join("+");
}

/** Inline label for the base modifier ("⌘" / "Ctrl+Shift+"). */
export function modLabel(setting: ModifierSetting): string {
  const m = resolveMod(setting);
  if (isMac) {
    return (m.ctrl ? "⌃" : "") + (m.alt ? "⌥" : "") + (m.shift ? "⇧" : "") + (m.meta ? "⌘" : "");
  }
  const mods = [
    m.ctrl ? "Ctrl" : null,
    m.alt ? "Alt" : null,
    m.shift ? "Shift" : null,
    m.meta ? "Meta" : null,
  ].filter((x): x is string => x !== null);
  return mods.join("+") + "+";
}

/**
 * Chords the browser reserves for itself — a page never sees them, so a
 * binding on one only fires inside the native shell. (⌘/Ctrl W T N Q, plus
 * their Shift layers.)
 */
export function isBrowserReserved(chord: string, setting: ModifierSetting): boolean {
  const p = parseChord(chord, setting);
  if (p === null) return false;
  const primary = isMac ? p.meta && !p.ctrl && !p.alt : p.ctrl && !p.alt && !p.meta;
  return primary && ["w", "t", "n", "q"].includes(p.key);
}

// --- spec-pinned chords (not rebindable) --------------------------------------

/**
 * Display strings for the pinned chords. The reference chord is pinned to
 * Cmd/Ctrl+Shift+R; per-pane font size is pinned to Cmd/Ctrl +/−/0 (they
 * shadow browser zoom, and only fire on font-sizable panes).
 */
export const PINNED = {
  reference: isMac ? "⇧⌘R" : "Ctrl+Shift+R",
  fontPlus: isMac ? "⌘+" : "Ctrl++",
  fontMinus: isMac ? "⌘−" : "Ctrl+-",
  fontReset: isMac ? "⌘0" : "Ctrl+0",
} as const;

/**
 * The native app's menu-bar accelerators (⌘ on macOS, Ctrl elsewhere — fixed by
 * the menu bar, independent of the base-modifier setting). These fire only
 * inside the chimaera app, because a browser reserves ⌘W/⌘T/⌘N for itself; the
 * shell "finally owns" them (see crates/chimaera-app/src/menu.rs). ⌘W/⌘T/⇧⌘T
 * are a second, browser-reserved way to reach a rebindable action above (Close
 * View / New Terminal / New Agent). Listed so the settings surface can show the
 * full map in one place — keep in sync with menu.rs.
 */
export const APP_MENU = {
  closeView: isMac ? "⌘W" : "Ctrl+W",
  newTerminal: isMac ? "⌘T" : "Ctrl+T",
  newAgent: isMac ? "⇧⌘T" : "Ctrl+Shift+T",
  newWindow: isMac ? "⇧⌘N" : "Ctrl+Shift+N",
} as const;

/** The pinned reference chord as a parsed matcher. */
export const REFERENCE_CHORD: ParsedChord = {
  meta: isMac,
  ctrl: !isMac,
  alt: false,
  shift: true,
  key: "r",
};

/**
 * Font-size chord action for `e`, when it is one: Cmd on macOS / Ctrl
 * elsewhere with +/−/0 (spec-pinned — these shadow browser zoom, so callers
 * must intercept them ONLY while a terminal pane is focused; everywhere else
 * browser zoom keeps working). "+1"/-"1" = step, 0 = reset.
 */
export function fontChord(e: KeyboardEvent): 1 | -1 | 0 | null {
  const mod = isMac ? e.metaKey && !e.ctrlKey && !e.altKey : e.ctrlKey && !e.metaKey && !e.altKey;
  if (!mod) return null;
  switch (e.code) {
    case "Equal": // + is Shift+Equal on most layouts; both step up
    case "NumpadAdd":
      return 1;
    case "Minus":
    case "NumpadSubtract":
      return -1;
    case "Digit0":
    case "Numpad0":
      return 0;
    default:
      return null;
  }
}

/**
 * Digit 1..9 when the event carries exactly the base modifier (openN is
 * pinned to Mod+1–9; the digit comes from the physical key so Shift-digit
 * symbol layouts don't break it). Null otherwise.
 */
export function chordDigit(e: KeyboardEvent, setting: ModifierSetting): number | null {
  const m = resolveMod(setting);
  if (e.metaKey !== m.meta || e.ctrlKey !== m.ctrl || e.altKey !== m.alt || e.shiftKey !== m.shift) {
    return null;
  }
  const match = /^Digit([1-9])$/.exec(e.code);
  return match === null ? null : Number.parseInt(match[1], 10);
}
