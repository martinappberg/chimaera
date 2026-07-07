/**
 * The theme registry: every selectable color theme, fully specified — the
 * complete UI token set (the CSS custom properties app.css declares) AND the
 * terminal's 16-color ANSI palette, together, so a theme can never leave the
 * terminal and the chrome disagreeing.
 *
 * The settings store resolves appearance.theme (system|light|dark) to a mode,
 * picks the theme id from appearance.lightTheme / appearance.darkTheme, and
 * applies `tokens` inline on <html> (the app.css blocks are the no-JS
 * fallback). The schema derives the theme-picker options (and their preview
 * swatches) from this list — adding a theme here is the whole job.
 *
 * Palette provenance: chimaera-* are the original hand-tuned palettes
 * (WCAG-annotated, see the ansi comments). nord, gruvbox-dark, solarized-
 * light, and rose-pine-dawn use the canonical published palettes, with
 * surface/edge tones adapted to chimaera's rail/pane/stage layering.
 */

/** Every themable CSS custom property; each theme must define them all. */
const TOKEN_NAMES = [
  "--bg",
  "--fg",
  "--muted",
  "--accent",
  "--err",
  "--warn",
  "--rate",
  "--rail-bg",
  "--row-hover",
  "--row-active",
  "--term-selection",
  "--term-bg",
  "--scrim",
  "--overlay-bg",
  "--edge",
  "--syn-keyword",
  "--syn-string",
  "--syn-comment",
  "--syn-number",
  "--syn-type",
  "--syn-func",
  "--syn-def",
  "--syn-prop",
  "--ficon-generic",
  "--ficon-lang",
  "--ficon-data",
  "--ficon-doc",
  "--ficon-media",
  "--ficon-archive",
  "--ficon-config",
  "--ficon-bio",
  "--ficon-vcs",
] as const;

export type TokenName = (typeof TOKEN_NAMES)[number];
export type ThemeTokens = Record<TokenName, string>;

/** xterm's 16-color palette (normal + bright). */
export interface AnsiPalette {
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  brightBlack: string;
  brightRed: string;
  brightGreen: string;
  brightYellow: string;
  brightBlue: string;
  brightMagenta: string;
  brightCyan: string;
  brightWhite: string;
}

export interface ThemeDef {
  /** Stable id stored in settings.json ("nord"). */
  id: string;
  /** Display name ("Nord"). */
  label: string;
  /** Which mode this theme serves; also becomes data-theme (color-scheme). */
  kind: "light" | "dark";
  tokens: ThemeTokens;
  ansi: AnsiPalette;
}

// --- Chimaera Light (original default; values mirror app.css :root) --------

const chimaeraLight: ThemeDef = {
  id: "chimaera-light",
  label: "Chimaera Light",
  kind: "light",
  tokens: {
    "--bg": "#fbfbfc",
    "--fg": "#1d1d20",
    "--muted": "#6c6c74",
    "--accent": "#2e9e6b",
    "--err": "#cc4444",
    "--warn": "#b8862b",
    "--rate": "#8a68c0",
    "--rail-bg": "#f3f3f5",
    "--row-hover": "#ececef",
    "--row-active": "#e4e4e9",
    "--term-selection": "#1d1d2026",
    "--term-bg": "#ffffff",
    "--scrim": "rgba(0, 0, 0, 0.25)",
    "--overlay-bg": "#ffffff",
    "--edge": "#e3e3e7",
    "--syn-keyword": "#9a4fa8",
    "--syn-string": "#2f8a57",
    "--syn-comment": "#8a8a93",
    "--syn-number": "#a05e2a",
    "--syn-type": "#2f7a9b",
    "--syn-func": "#3e6fc0",
    "--syn-def": "#1d1d20",
    "--syn-prop": "#705aa8",
    "--ficon-generic": "#7c7c85",
    "--ficon-lang": "#4a76c4",
    "--ficon-data": "#2f8a70",
    "--ficon-doc": "#6a6a76",
    "--ficon-media": "#b07a2e",
    "--ficon-archive": "#9a7440",
    "--ficon-config": "#78788a",
    "--ficon-bio": "#8a5bb0",
    "--ficon-vcs": "#b0603c",
  },
  // Readability pass 2026-07-06 (measured WCAG ratios against #ffffff):
  // every color a TUI uses AS TEXT holds >= 4.5:1 (normal) or >= 3.5:1
  // (bright variants). white/brightWhite stay near the background by ANSI
  // semantics; the minimumContrastRatio floor catches TUIs typing with them.
  ansi: {
    black: "#3b3b41",
    red: "#bf4d56", // 4.76
    green: "#2d8453", // 4.63
    yellow: "#8c702b", // 4.70
    blue: "#3e6fc0", // 4.95
    magenta: "#95569f", // 5.11
    cyan: "#2b7e8d", // 4.70
    white: "#b9b9c0",
    brightBlack: "#73737d", // 4.69
    brightRed: "#d26873", // 3.52
    brightGreen: "#3e9866", // 3.57
    brightYellow: "#a18542", // 3.53
    brightBlue: "#5b89d5", // 3.51
    brightMagenta: "#ad74b8", // 3.52
    brightCyan: "#4293a1", // 3.55
    brightWhite: "#d9d9df",
  },
};

// --- Chimaera Dark (original default; values mirror app.css dark block) -----

const chimaeraDark: ThemeDef = {
  id: "chimaera-dark",
  label: "Chimaera Dark",
  kind: "dark",
  tokens: {
    "--bg": "#131316",
    "--fg": "#e7e7ea",
    "--muted": "#92929c",
    "--accent": "#3fbf85",
    "--err": "#d96b6b",
    "--warn": "#d0a355",
    "--rate": "#a58fd6",
    "--rail-bg": "#18181c",
    "--row-hover": "#1f1f24",
    "--row-active": "#26262c",
    "--term-selection": "#e7e7ea33",
    "--term-bg": "#0f0f13",
    "--scrim": "rgba(0, 0, 0, 0.5)",
    "--overlay-bg": "#1b1b20",
    "--edge": "#2b2b32",
    "--syn-keyword": "#c586d6",
    "--syn-string": "#7cc99a",
    "--syn-comment": "#7a7a85",
    "--syn-number": "#d0a355",
    "--syn-type": "#6fb3d2",
    "--syn-func": "#82a7e8",
    "--syn-def": "#e7e7ea",
    "--syn-prop": "#a58fd6",
    "--ficon-generic": "#8b8b95",
    "--ficon-lang": "#6f9ae4",
    "--ficon-data": "#58c39c",
    "--ficon-doc": "#9a9aa6",
    "--ficon-media": "#d8a45c",
    "--ficon-archive": "#c39a63",
    "--ficon-config": "#9a9ab0",
    "--ficon-bio": "#b78fdc",
    "--ficon-vcs": "#e0855c",
  },
  // Measured against #0f0f13; brightBlack (claude's secondary text) was the
  // worst offender at 3.03 and now sits at 4.65.
  ansi: {
    black: "#33333a",
    red: "#e2757e", // 6.45
    green: "#5cc48d", // 8.87
    yellow: "#d9b96c", // 10.11
    blue: "#79a5ea", // 7.63
    magenta: "#c795d3", // 7.89
    cyan: "#6cc3d4", // 9.47
    white: "#c9c9d1", // 11.62
    brightBlack: "#7c7c8a", // 4.65
    brightRed: "#ef959c", // 8.61
    brightGreen: "#7fd6a8", // 11.01
    brightYellow: "#e7cd8b", // 12.30
    brightBlue: "#9cbbf1", // 9.83
    brightMagenta: "#d8afe2", // 10.15
    brightCyan: "#8fd6e4", // 11.75
    brightWhite: "#ededf3", // 16.40
  },
};

// --- Nord (arcticicestudio's canonical nord0–nord15) ------------------------

const nord: ThemeDef = {
  id: "nord",
  label: "Nord",
  kind: "dark",
  tokens: {
    "--bg": "#323947", // between panes (nord0) and rail
    "--fg": "#d8dee9", // nord4
    "--muted": "#8792a8", // lightened nord3 for legible secondary text
    "--accent": "#88c0d0", // nord8, the signature frost cyan
    "--err": "#bf616a", // nord11
    "--warn": "#ebcb8b", // nord13
    "--rate": "#b48ead", // nord15
    "--rail-bg": "#373f4f",
    "--row-hover": "#414b5e",
    "--row-active": "#4a566c",
    "--term-selection": "#d8dee930",
    "--term-bg": "#2e3440", // nord0, the canonical editor bg
    "--scrim": "rgba(0, 0, 0, 0.45)",
    "--overlay-bg": "#3b4252", // nord1
    "--edge": "#434c5e", // nord2
    "--syn-keyword": "#81a1c1", // nord9
    "--syn-string": "#a3be8c", // nord14
    "--syn-comment": "#616e88", // the community's brightened comment tone
    "--syn-number": "#b48ead", // nord15
    "--syn-type": "#8fbcbb", // nord7
    "--syn-func": "#88c0d0", // nord8
    "--syn-def": "#d8dee9",
    "--syn-prop": "#81a1c1",
    "--ficon-generic": "#8792a8",
    "--ficon-lang": "#81a1c1",
    "--ficon-data": "#8fbcbb",
    "--ficon-doc": "#9aa5ba",
    "--ficon-media": "#d08770", // nord12
    "--ficon-archive": "#ebcb8b",
    "--ficon-config": "#95a0b5",
    "--ficon-bio": "#b48ead",
    "--ficon-vcs": "#d08770",
  },
  // Canonical Nord terminal mapping; brightBlack lifted to the widely used
  // #616e88 (nord3 at 4.5+:1 on nord0 — the spec's #4c566a is ~2:1 and the
  // contrast floor would repaint it anyway).
  ansi: {
    black: "#3b4252",
    red: "#bf616a",
    green: "#a3be8c",
    yellow: "#ebcb8b",
    blue: "#81a1c1",
    magenta: "#b48ead",
    cyan: "#88c0d0",
    white: "#e5e9f0",
    brightBlack: "#616e88",
    brightRed: "#bf616a",
    brightGreen: "#a3be8c",
    brightYellow: "#ebcb8b",
    brightBlue: "#81a1c1",
    brightMagenta: "#b48ead",
    brightCyan: "#8fbcbb",
    brightWhite: "#eceff4",
  },
};

// --- Gruvbox Dark (morhetz's canonical medium-contrast dark) ---------------

const gruvboxDark: ThemeDef = {
  id: "gruvbox-dark",
  label: "Gruvbox Dark",
  kind: "dark",
  tokens: {
    "--bg": "#282828", // bg0
    "--fg": "#ebdbb2", // fg1
    "--muted": "#a89984", // fg4
    "--accent": "#b8bb26", // bright green — chimaera's alive-green, gruvboxed
    "--err": "#fb4934",
    "--warn": "#fabd2f",
    "--rate": "#d3869b",
    "--rail-bg": "#32302f", // bg0_s
    "--row-hover": "#3c3836", // bg1
    "--row-active": "#504945", // bg2
    "--term-selection": "#ebdbb230",
    "--term-bg": "#1d2021", // bg0_h — panes get the hard-contrast well
    "--scrim": "rgba(0, 0, 0, 0.5)",
    "--overlay-bg": "#32302f",
    "--edge": "#45403d",
    "--syn-keyword": "#fb4934",
    "--syn-string": "#b8bb26",
    "--syn-comment": "#928374",
    "--syn-number": "#d3869b",
    "--syn-type": "#fabd2f",
    "--syn-func": "#8ec07c",
    "--syn-def": "#ebdbb2",
    "--syn-prop": "#83a598",
    "--ficon-generic": "#928374",
    "--ficon-lang": "#83a598",
    "--ficon-data": "#8ec07c",
    "--ficon-doc": "#a89984",
    "--ficon-media": "#fe8019",
    "--ficon-archive": "#d79921",
    "--ficon-config": "#bdae93",
    "--ficon-bio": "#d3869b",
    "--ficon-vcs": "#fe8019",
  },
  // Canonical gruvbox terminal palette (normal = faded, bright = bright).
  ansi: {
    black: "#282828",
    red: "#cc241d",
    green: "#98971a",
    yellow: "#d79921",
    blue: "#458588",
    magenta: "#b16286",
    cyan: "#689d6a",
    white: "#a89984",
    brightBlack: "#928374",
    brightRed: "#fb4934",
    brightGreen: "#b8bb26",
    brightYellow: "#fabd2f",
    brightBlue: "#83a598",
    brightMagenta: "#d3869b",
    brightCyan: "#8ec07c",
    brightWhite: "#ebdbb2",
  },
};

// --- Solarized Light (Ethan Schoonover's canonical base/accent tones) ------

const solarizedLight: ThemeDef = {
  id: "solarized-light",
  label: "Solarized Light",
  kind: "light",
  tokens: {
    "--bg": "#f6efdb", // between base3 panes and base2 rail
    "--fg": "#586e75", // base01, the emphasized content tone
    "--muted": "#839496", // base0
    "--accent": "#859900", // solarized green
    "--err": "#dc322f",
    "--warn": "#b58900",
    "--rate": "#6c71c4", // violet
    "--rail-bg": "#eee8d5", // base2
    "--row-hover": "#e5ddc4",
    "--row-active": "#dcd3b5",
    "--term-selection": "#586e7526",
    "--term-bg": "#fdf6e3", // base3, the canonical paper
    "--scrim": "rgba(0, 0, 0, 0.25)",
    "--overlay-bg": "#fdf6e3",
    "--edge": "#ded5ba",
    "--syn-keyword": "#859900", // green (Statement)
    "--syn-string": "#2aa198", // cyan
    "--syn-comment": "#839496",
    "--syn-number": "#d33682", // magenta
    "--syn-type": "#b58900", // yellow (Type)
    "--syn-func": "#268bd2", // blue (Identifier)
    "--syn-def": "#586e75",
    "--syn-prop": "#6c71c4", // violet
    "--ficon-generic": "#839496",
    "--ficon-lang": "#268bd2",
    "--ficon-data": "#2aa198",
    "--ficon-doc": "#657b83",
    "--ficon-media": "#cb4b16",
    "--ficon-archive": "#b58900",
    "--ficon-config": "#93a1a1",
    "--ficon-bio": "#6c71c4",
    "--ficon-vcs": "#cb4b16",
  },
  // Pragmatic bright mapping (hue-per-slot) instead of the spec's base-tones-
  // on-bright-slots, which turns TUI text invisible on light backgrounds.
  ansi: {
    black: "#073642",
    red: "#dc322f",
    green: "#859900",
    yellow: "#b58900",
    blue: "#268bd2",
    magenta: "#d33682",
    cyan: "#2aa198",
    white: "#eee8d5",
    brightBlack: "#657b83",
    brightRed: "#cb4b16",
    brightGreen: "#859900",
    brightYellow: "#b58900",
    brightBlue: "#268bd2",
    brightMagenta: "#6c71c4",
    brightCyan: "#2aa198",
    brightWhite: "#fdf6e3",
  },
};

// --- Rosé Pine Dawn (the official dawn variant) ------------------------------

const rosePineDawn: ThemeDef = {
  id: "rose-pine-dawn",
  label: "Rosé Pine Dawn",
  kind: "light",
  tokens: {
    "--bg": "#faf4ed", // base
    "--fg": "#575279", // text
    "--muted": "#797593", // subtle
    "--accent": "#286983", // pine
    "--err": "#b4637a", // love
    "--warn": "#ea9d34", // gold
    "--rate": "#907aa9", // iris
    "--rail-bg": "#f2e9e1", // overlay
    "--row-hover": "#eadfd5",
    "--row-active": "#e2d5c8",
    "--term-selection": "#57527926",
    "--term-bg": "#fffaf3", // surface — panes get the brightest sheet
    "--scrim": "rgba(87, 82, 121, 0.25)",
    "--overlay-bg": "#fffaf3",
    "--edge": "#dfdad9", // highlight med
    "--syn-keyword": "#286983", // pine
    "--syn-string": "#ea9d34", // gold
    "--syn-comment": "#9893a5", // muted
    "--syn-number": "#d7827e", // rose
    "--syn-type": "#56949f", // foam
    "--syn-func": "#907aa9", // iris
    "--syn-def": "#575279",
    "--syn-prop": "#286983",
    "--ficon-generic": "#797593",
    "--ficon-lang": "#56949f",
    "--ficon-data": "#286983",
    "--ficon-doc": "#8e89a0",
    "--ficon-media": "#ea9d34",
    "--ficon-archive": "#d7827e",
    "--ficon-config": "#8b86a3",
    "--ficon-bio": "#907aa9",
    "--ficon-vcs": "#b4637a",
  },
  // Official rose-pine/terminal dawn palette.
  ansi: {
    black: "#f2e9e1",
    red: "#b4637a",
    green: "#286983",
    yellow: "#ea9d34",
    blue: "#56949f",
    magenta: "#907aa9",
    cyan: "#d7827e",
    white: "#575279",
    brightBlack: "#9893a5",
    brightRed: "#b4637a",
    brightGreen: "#286983",
    brightYellow: "#ea9d34",
    brightBlue: "#56949f",
    brightMagenta: "#907aa9",
    brightCyan: "#d7827e",
    brightWhite: "#575279",
  },
};

/** All themes, picker order (defaults first). */
export const THEMES: readonly ThemeDef[] = [
  chimaeraLight,
  solarizedLight,
  rosePineDawn,
  chimaeraDark,
  nord,
  gruvboxDark,
];

const BY_ID = new Map(THEMES.map((t) => [t.id, t]));

export function themeById(id: string): ThemeDef | undefined {
  return BY_ID.get(id);
}

export function themesOfKind(kind: "light" | "dark"): ThemeDef[] {
  return THEMES.filter((t) => t.kind === kind);
}

/** The built-in default for a mode (used when a stored id is unknown). */
export function defaultThemeFor(kind: "light" | "dark"): ThemeDef {
  return kind === "dark" ? chimaeraDark : chimaeraLight;
}
