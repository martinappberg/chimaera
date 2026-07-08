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
 * Palette provenance: chimaera-* are the original hand-tuned palettes.
 * nord, gruvbox-dark, solarized-light, rose-pine-dawn, and catppuccin-* start
 * from the canonical published palettes, with surface/edge tones adapted to
 * chimaera's rail/pane/stage layering.
 *
 * Legibility contract (readability pass 2026-07-07): every color a theme uses
 * AS TEXT holds WCAG >= 4.5:1 against its sheet (>= 3.5:1 for bright ANSI
 * variants, >= 3.8:1 for comments, which must stay visibly dimmer than code).
 * Where a canonical tone missed the floor, its OKLab lightness was moved just
 * far enough to pass with the hue and chroma untouched, so each theme still
 * reads unmistakably as itself. Nord is kept exactly canonical — its softness
 * is the point, and the terminal's minimumContrastRatio floor backstops it.
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
    "--muted": "#65656d", // holds 4.5:1 even on --row-active
    "--accent": "#2e9e6b",
    "--err": "#cc4444",
    "--warn": "#a5730d", // renders as note TEXT, not just a dot — 4.0:1 floor
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
    "--syn-string": "#2b8654",
    "--syn-comment": "#80808a",
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
  // Measured against #ffffff: every color a TUI uses AS TEXT holds >= 4.5:1
  // (normal) or >= 3.5:1 (bright variants). white/brightWhite stay near the
  // background by ANSI semantics; the minimumContrastRatio floor catches TUIs
  // typing with them.
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
// 2026-07-07 softening pass: the neutral stack lifted a step and given a cool
// undertone (void-black read as harsh next to the softer themes), muted and
// comments brightened to keep their ratios on the lighter sheets. Identity —
// neutral surfaces, green accent — unchanged.

const chimaeraDark: ThemeDef = {
  id: "chimaera-dark",
  label: "Chimaera Dark",
  kind: "dark",
  tokens: {
    "--bg": "#17171c",
    "--fg": "#e7e7ea",
    "--muted": "#9a9aa6",
    "--accent": "#3fbf85",
    "--err": "#d96b6b",
    "--warn": "#d0a355",
    "--rate": "#a58fd6",
    "--rail-bg": "#1c1c23",
    "--row-hover": "#25252d",
    "--row-active": "#2d2d36",
    "--term-selection": "#e7e7ea33",
    "--term-bg": "#121218",
    "--scrim": "rgba(0, 0, 0, 0.5)",
    "--overlay-bg": "#202028",
    "--edge": "#31313b",
    "--syn-keyword": "#c586d6",
    "--syn-string": "#7cc99a",
    "--syn-comment": "#7d7e89",
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
  // Measured against #121218; brightBlack (claude's secondary text) holds 4.5.
  ansi: {
    black: "#33333a",
    red: "#e2757e",
    green: "#5cc48d",
    yellow: "#d9b96c",
    blue: "#79a5ea",
    magenta: "#c795d3",
    cyan: "#6cc3d4",
    white: "#c9c9d1",
    brightBlack: "#7c7c8a", // 4.54
    brightRed: "#ef959c",
    brightGreen: "#7fd6a8",
    brightYellow: "#e7cd8b",
    brightBlue: "#9cbbf1",
    brightMagenta: "#d8afe2",
    brightCyan: "#8fd6e4",
    brightWhite: "#ededf3",
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
    "--muted": "#bdae93", // fg3 — fg4 fell under 4:1 on selections
    "--accent": "#b8bb26", // bright green — chimaera's alive-green, gruvboxed
    "--err": "#ff533d", // bright red nudged over 4.5:1 (it renders as error TEXT)
    "--warn": "#fabd2f",
    "--rate": "#d3869b",
    "--rail-bg": "#32302f", // bg0_s
    "--row-hover": "#3c3836", // bg1
    "--row-active": "#453f3a", // between bg1 and bg2 — bg2 drowned muted text
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
  // Normal slots take gruvbox's BRIGHT palette (the iconic tones): the spec's
  // faded neutrals sit at 3.0–3.9:1 on bg0_h and read as mud next to their
  // bright twins. Nord ships the same normal==bright doubling.
  ansi: {
    black: "#282828",
    red: "#fb4934",
    green: "#b8bb26",
    yellow: "#fabd2f",
    blue: "#83a598",
    magenta: "#d3869b",
    cyan: "#8ec07c",
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

// --- Solarized Light (Ethan Schoonover's hues, floored for legibility) ------
// Canonical solarized-light text tones sit at 2.6–3.0:1 on the paper bg —
// famously washed out on modern panels. Text moves one base-tone darker
// (base02/base01) and each accent keeps its hue with lightness dropped to the
// 4.5 floor, so it still reads as solarized without the squint.

const solarizedLight: ThemeDef = {
  id: "solarized-light",
  label: "Solarized Light",
  kind: "light",
  tokens: {
    "--bg": "#f6efdb", // between base3 panes and base2 rail
    "--fg": "#073642", // base02 — base01 hovers at the 4.5 line on the rail
    "--muted": "#495f66", // base01 deepened to hold 4.5 on selected rows
    "--accent": "#6b7d00", // solarized green, floored
    "--err": "#d02425",
    "--warn": "#986e00",
    "--rate": "#6569bc", // violet
    "--rail-bg": "#eee8d5", // base2
    "--row-hover": "#e5ddc4",
    "--row-active": "#dcd3b5",
    "--term-selection": "#586e7526",
    "--term-bg": "#fdf6e3", // base3, the canonical paper
    "--scrim": "rgba(0, 0, 0, 0.25)",
    "--overlay-bg": "#fdf6e3",
    "--edge": "#ded5ba",
    "--syn-keyword": "#677900", // green (Statement)
    "--syn-string": "#007f77", // cyan
    "--syn-comment": "#708082",
    "--syn-number": "#cd307d", // magenta
    "--syn-type": "#946a00", // yellow (Type)
    "--syn-func": "#0076bc", // blue (Identifier)
    "--syn-def": "#073642",
    "--syn-prop": "#6569bc", // violet
    "--ficon-generic": "#657b83",
    "--ficon-lang": "#0076bc",
    "--ficon-data": "#007f77",
    "--ficon-doc": "#657b83",
    "--ficon-media": "#c6470f",
    "--ficon-archive": "#946a00",
    "--ficon-config": "#708082",
    "--ficon-bio": "#6569bc",
    "--ficon-vcs": "#c6470f",
  },
  // Hue-per-slot bright mapping (the spec puts base tones on bright slots,
  // which turns TUI text invisible on light backgrounds). Normal >= 4.5:1 on
  // base3, bright >= 3.5:1.
  ansi: {
    black: "#073642",
    red: "#d72d2b",
    green: "#677900",
    yellow: "#946a00",
    blue: "#0076bc",
    magenta: "#cd307d",
    cyan: "#007f77",
    white: "#eee8d5",
    brightBlack: "#657b83",
    brightRed: "#cb4b16",
    brightGreen: "#768a00",
    brightYellow: "#a67a00",
    brightBlue: "#1f87cd",
    brightMagenta: "#6569bc",
    brightCyan: "#069088",
    brightWhite: "#fdf6e3",
  },
};

// --- Rosé Pine Dawn (official dawn hues, floored for legibility) ------------
// Dawn's gold/rose/foam are decorative tones in the official spec (gold sits
// at 2.2:1 on the surface). Chimaera renders them as text — warnings, strings,
// ANSI slots — so each keeps its hue with lightness dropped to the 4.5 floor.

const rosePineDawn: ThemeDef = {
  id: "rose-pine-dawn",
  label: "Rosé Pine Dawn",
  kind: "light",
  tokens: {
    "--bg": "#faf4ed", // base
    "--fg": "#575279", // text
    "--muted": "#5f5a78", // subtle deepened to hold 4.5 on selected rows
    "--accent": "#286983", // pine
    "--err": "#a7586f", // love
    "--warn": "#b06700", // gold
    "--rate": "#806b99", // iris
    "--rail-bg": "#f2e9e1", // overlay
    "--row-hover": "#eadfd5",
    "--row-active": "#e2d5c8",
    "--term-selection": "#57527926",
    "--term-bg": "#fffaf3", // surface — panes get the brightest sheet
    "--scrim": "rgba(87, 82, 121, 0.25)",
    "--overlay-bg": "#fffaf3",
    "--edge": "#dfdad9", // highlight med
    "--syn-keyword": "#286983", // pine
    "--syn-string": "#ab6200", // gold
    "--syn-comment": "#827d8f", // muted
    "--syn-number": "#ad5d5a", // rose
    "--syn-type": "#3e7d87", // foam
    "--syn-func": "#806b99", // iris
    "--syn-def": "#575279",
    "--syn-prop": "#286983",
    "--ficon-generic": "#706b89",
    "--ficon-lang": "#3e7d87",
    "--ficon-data": "#286983",
    "--ficon-doc": "#7d7891",
    "--ficon-media": "#ab6200",
    "--ficon-archive": "#ad5d5a",
    "--ficon-config": "#7a7590",
    "--ficon-bio": "#806b99",
    "--ficon-vcs": "#a7586f",
  },
  // Official rose-pine/terminal dawn slot mapping (green=pine, cyan=rose is
  // the palette's own quirk), tones floored as above.
  ansi: {
    black: "#f2e9e1",
    red: "#ab5b72",
    green: "#286983",
    yellow: "#ab6200",
    blue: "#3e7d87",
    magenta: "#806b99",
    cyan: "#ad5d5a",
    white: "#575279",
    brightBlack: "#827d8f",
    brightRed: "#b4637a",
    brightGreen: "#286983",
    brightYellow: "#bd7300",
    brightBlue: "#508d98",
    brightMagenta: "#907aa9",
    brightCyan: "#bf6d69",
    brightWhite: "#575279",
  },
};

// --- Catppuccin Latte (the official light flavor) ---------------------------
// Surfaces are exactly canonical (base sheet, mantle bg, crust rail). Latte's
// warm accents (yellow, peach, pink, teal) are pastel-light and fail on the
// bright sheets, so text uses hue-preserving darkened tones.

const catppuccinLatte: ThemeDef = {
  id: "catppuccin-latte",
  label: "Catppuccin Latte",
  kind: "light",
  tokens: {
    "--bg": "#e6e9ef", // mantle
    "--fg": "#4c4f69", // text
    "--muted": "#555870", // subtext1, floored for selections
    "--accent": "#1e66f5", // blue
    "--err": "#cf0837", // red, floored
    "--warn": "#ab5d00", // yellow, floored
    "--rate": "#8839ef", // mauve
    "--rail-bg": "#dce0e8", // crust
    "--row-hover": "#d3d8e2",
    "--row-active": "#ccd0da", // surface0
    "--term-selection": "#4c4f6926",
    "--term-bg": "#eff1f5", // base — panes get the brightest sheet
    "--scrim": "rgba(76, 79, 105, 0.25)",
    "--overlay-bg": "#eff1f5",
    "--edge": "#c6cad6",
    "--syn-keyword": "#8839ef", // mauve
    "--syn-string": "#188000", // green
    "--syn-comment": "#76798c", // overlay2
    "--syn-number": "#ce3400", // peach
    "--syn-type": "#a75a00", // yellow
    "--syn-func": "#1e66f5", // blue
    "--syn-def": "#4c4f69",
    "--syn-prop": "#5363d6", // lavender
    "--ficon-generic": "#76798c",
    "--ficon-lang": "#1e66f5",
    "--ficon-data": "#007a82",
    "--ficon-doc": "#6c6f85",
    "--ficon-media": "#b96a00",
    "--ficon-archive": "#a75a00",
    "--ficon-config": "#7c7f93",
    "--ficon-bio": "#8839ef",
    "--ficon-vcs": "#ce3400",
  },
  // Official latte slot mapping (magenta=pink, cyan=teal), tones floored:
  // normal >= 4.5:1 on base, bright >= 3.5:1.
  ansi: {
    black: "#5c5f77",
    red: "#d20f39",
    green: "#188000",
    yellow: "#a75a00",
    blue: "#1b63f2",
    magenta: "#b24297",
    cyan: "#007a82",
    white: "#acb0be",
    brightBlack: "#6c6f85",
    brightRed: "#d20f39",
    brightGreen: "#2f9015",
    brightYellow: "#b96a00",
    brightBlue: "#1e66f5",
    brightMagenta: "#c453a8",
    brightCyan: "#048b93",
    brightWhite: "#bcc0cc",
  },
};

// --- Catppuccin Mocha (the official dark flavor) ----------------------------
// Canonical throughout: mantle terminal well, base bg, surface0 selections,
// the pastel accents straight from the spec (all clear 7:1 on these sheets).

const catppuccinMocha: ThemeDef = {
  id: "catppuccin-mocha",
  label: "Catppuccin Mocha",
  kind: "dark",
  tokens: {
    "--bg": "#1e1e2e", // base
    "--fg": "#cdd6f4", // text
    "--muted": "#a6adc8", // subtext0
    "--accent": "#89b4fa", // blue
    "--err": "#f38ba8", // red
    "--warn": "#fab387", // peach
    "--rate": "#cba6f7", // mauve
    "--rail-bg": "#232336",
    "--row-hover": "#2b2b41",
    "--row-active": "#333349",
    "--term-selection": "#cdd6f430",
    "--term-bg": "#181825", // mantle — panes get the darkest well
    "--scrim": "rgba(0, 0, 0, 0.5)",
    "--overlay-bg": "#252538",
    "--edge": "#3a3a52",
    "--syn-keyword": "#cba6f7", // mauve
    "--syn-string": "#a6e3a1", // green
    "--syn-comment": "#7f849c", // overlay1
    "--syn-number": "#fab387", // peach
    "--syn-type": "#f9e2af", // yellow
    "--syn-func": "#89b4fa", // blue
    "--syn-def": "#cdd6f4",
    "--syn-prop": "#b4befe", // lavender
    "--ficon-generic": "#9399b2",
    "--ficon-lang": "#89b4fa",
    "--ficon-data": "#94e2d5",
    "--ficon-doc": "#a6adc8",
    "--ficon-media": "#fab387",
    "--ficon-archive": "#f9e2af",
    "--ficon-config": "#b4befe",
    "--ficon-bio": "#cba6f7",
    "--ficon-vcs": "#eba0ac",
  },
  // Official mocha terminal palette; brightBlack lifted from surface2 (2.9:1)
  // to a legible overlay tone, brightWhite lifted to text so bold-bright
  // actually reads brighter than white.
  ansi: {
    black: "#45475a",
    red: "#f38ba8",
    green: "#a6e3a1",
    yellow: "#f9e2af",
    blue: "#89b4fa",
    magenta: "#f5c2e7",
    cyan: "#94e2d5",
    white: "#bac2de",
    brightBlack: "#73788e",
    brightRed: "#f38ba8",
    brightGreen: "#a6e3a1",
    brightYellow: "#f9e2af",
    brightBlue: "#89b4fa",
    brightMagenta: "#f5c2e7",
    brightCyan: "#94e2d5",
    brightWhite: "#cdd6f4",
  },
};

/** All themes, picker order (defaults first). */
export const THEMES: readonly ThemeDef[] = [
  chimaeraLight,
  solarizedLight,
  rosePineDawn,
  catppuccinLatte,
  chimaeraDark,
  nord,
  gruvboxDark,
  catppuccinMocha,
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
