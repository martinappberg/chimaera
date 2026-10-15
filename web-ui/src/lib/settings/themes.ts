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
 * github-light, one-light, one-dark, tokyo-night, rose-pine-dawn and
 * catppuccin-* start from the canonical published palettes, with surface/edge
 * tones adapted to chimaera's rail/pane/stage layering (2026-10-15: nord,
 * gruvbox and solarized-light left — editor palettes that never sat well on a
 * layered workbench). terracotta-* and graphite-* are chimaera's own: a warm
 * ivory/charcoal pair and a true-neutral pair.
 *
 * Legibility contract (readability pass 2026-07-07): every color a theme uses
 * AS TEXT holds WCAG >= 4.5:1 against its sheet (>= 3.5:1 for bright ANSI
 * variants, >= 3.8:1 for comments, which must stay visibly dimmer than code).
 * Where a canonical tone missed the floor, its OKLab lightness was moved just
 * far enough to pass with the hue and chroma untouched, so each theme still
 * reads unmistakably as itself. themes.contrast.test.ts enforces the floors.
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
    "--accent": "#2e9e6b", // 3.4:1 on white: never text as-is — links mix it toward --fg
    "--err": "#cc4444",
    "--warn": "#9d6d0e", // renders as note TEXT, not just a dot
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
    "--fg": "#dcdce0", // one step off white: 15:1 glared next to every other dark theme (12–13)
    "--muted": "#9a9aa6",
    "--accent": "#3fbf85",
    "--err": "#d96b6b",
    "--warn": "#d0a355",
    "--rate": "#a58fd6",
    "--rail-bg": "#1c1c23",
    "--row-hover": "#25252d",
    "--row-active": "#2d2d36",
    "--term-selection": "#dcdce033",
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
    "--syn-def": "#dcdce0",
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
    "--fg": "#3d3f58", // text deepened a step: canonical #4c4f69 sat within 15% of muted, so nothing receded
    "--muted": "#555870", // subtext1, floored for selections
    "--accent": "#1b63f1", // blue
    "--err": "#cf0837", // red, floored
    "--warn": "#a55b07", // yellow, floored
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
    "--syn-func": "#1b63f1", // blue
    "--syn-def": "#3d3f58",
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

// --- GitHub Light (Primer's light palette) ---------------------------------
// The most-seen light theme in developer tools: neutral white, blue accent,
// the familiar red/blue/purple syntax. Surfaces follow Primer's canvas →
// canvas.subtle ladder; borders are Primer's border.default.

const githubLight: ThemeDef = {
  id: "github-light",
  label: "GitHub Light",
  kind: "light",
  tokens: {
    "--bg": "#fafbfc",
    "--fg": "#1f2328", // fg.default
    "--muted": "#656d76", // fg.muted
    "--accent": "#0969da", // accent.fg
    "--err": "#cf222e", // danger.fg
    "--warn": "#9a6700", // attention.fg
    "--rate": "#8250df", // done.fg
    "--rail-bg": "#f6f8fa", // canvas.subtle
    "--row-hover": "#eef1f4",
    "--row-active": "#e4e8ec",
    "--term-selection": "#1f232826",
    "--term-bg": "#ffffff", // canvas.default — panes get the paper
    "--scrim": "rgba(0, 0, 0, 0.25)",
    "--overlay-bg": "#ffffff",
    "--edge": "#d0d7de", // border.default
    "--syn-keyword": "#cf222e",
    "--syn-string": "#0a3069",
    "--syn-comment": "#6e7781",
    "--syn-number": "#0550ae",
    "--syn-type": "#953800",
    "--syn-func": "#6639ba",
    "--syn-def": "#1f2328",
    "--syn-prop": "#0550ae",
    "--ficon-generic": "#6e7781",
    "--ficon-lang": "#0969da",
    "--ficon-data": "#1a7f37",
    "--ficon-doc": "#656d76",
    "--ficon-media": "#bc4c00",
    "--ficon-archive": "#9a6700",
    "--ficon-config": "#57606a",
    "--ficon-bio": "#8250df",
    "--ficon-vcs": "#cf222e",
  },
  // Primer's own terminal palette.
  ansi: {
    black: "#24292f",
    red: "#cf222e",
    green: "#116329",
    yellow: "#4d2d00",
    blue: "#0969da",
    magenta: "#8250df",
    cyan: "#1b7c83",
    white: "#6e7781",
    brightBlack: "#57606a",
    brightRed: "#a40e26",
    brightGreen: "#1a7f37",
    brightYellow: "#633c01",
    brightBlue: "#1d88fc",
    brightMagenta: "#9e6ef2",
    brightCyan: "#3192aa",
    brightWhite: "#8c959f",
  },
};

// --- One Light (Atom's One Light) -------------------------------------------
// Soft cool-grey paper with the One syntax set; the light half of the most
// installed editor theme. Surfaces step from the editor's #fafafa.

const oneLight: ThemeDef = {
  id: "one-light",
  label: "One Light",
  kind: "light",
  tokens: {
    "--bg": "#f4f4f5",
    "--fg": "#383a42",
    "--muted": "#666973",
    "--accent": "#4078f2", // blue
    "--err": "#ce4237", // red
    "--warn": "#9b6a02", // yellow
    "--rate": "#a626a4", // purple
    "--rail-bg": "#eeeef0",
    "--row-hover": "#e5e5e6",
    "--row-active": "#dcdcde",
    "--term-selection": "#383a4226",
    "--term-bg": "#fafafa", // the editor sheet
    "--scrim": "rgba(0, 0, 0, 0.25)",
    "--overlay-bg": "#ffffff",
    "--edge": "#dbdbdc",
    "--syn-keyword": "#a626a4",
    "--syn-string": "#308331",
    "--syn-comment": "#7e7f85",
    "--syn-number": "#986801",
    "--syn-type": "#9b6a02",
    "--syn-func": "#356ce4",
    "--syn-def": "#383a42",
    "--syn-prop": "#ce4237",
    "--ficon-generic": "#8a8b91",
    "--ficon-lang": "#4078f2",
    "--ficon-data": "#0184bc",
    "--ficon-doc": "#696c77",
    "--ficon-media": "#986801",
    "--ficon-archive": "#c18401",
    "--ficon-config": "#0184bc",
    "--ficon-bio": "#a626a4",
    "--ficon-vcs": "#e45649",
  },
  ansi: {
    black: "#383a42",
    red: "#ce4237",
    green: "#308331",
    yellow: "#9b6a02",
    blue: "#356ce4",
    magenta: "#a626a4",
    cyan: "#0f7bae",
    white: "#a0a1a7",
    brightBlack: "#4f525e",
    brightRed: "#e45649",
    brightGreen: "#459645",
    brightYellow: "#b27b0f",
    brightBlue: "#4078f2",
    brightMagenta: "#a626a4",
    brightCyan: "#0184bc",
    brightWhite: "#e5e5e6",
  },
};

// --- Tokyo Night (enkia's night variant) -------------------------------------
// Deep blue-black with cool lavender text and the blue/green/magenta syntax
// set; the modern dark theme. Surfaces step up from the editor's #1a1b26.

const tokyoNight: ThemeDef = {
  id: "tokyo-night",
  label: "Tokyo Night",
  kind: "dark",
  tokens: {
    "--bg": "#1f2030",
    "--fg": "#c0caf5",
    "--muted": "#9aa5ce",
    "--accent": "#7aa2f7", // blue
    "--err": "#f7768e", // red
    "--warn": "#e0af68", // yellow
    "--rate": "#bb9af7", // magenta
    "--rail-bg": "#24263a",
    "--row-hover": "#2b2e45",
    "--row-active": "#343852",
    "--term-selection": "#c0caf530",
    "--term-bg": "#1a1b26", // the editor well
    "--scrim": "rgba(0, 0, 0, 0.5)",
    "--overlay-bg": "#262a40",
    "--edge": "#363b54",
    "--syn-keyword": "#bb9af7",
    "--syn-string": "#9ece6a",
    "--syn-comment": "#6b75a0",
    "--syn-number": "#ff9e64",
    "--syn-type": "#2ac3de",
    "--syn-func": "#7aa2f7",
    "--syn-def": "#c0caf5",
    "--syn-prop": "#7dcfff",
    "--ficon-generic": "#8f9bc7",
    "--ficon-lang": "#7aa2f7",
    "--ficon-data": "#73daca",
    "--ficon-doc": "#a9b1d6",
    "--ficon-media": "#ff9e64",
    "--ficon-archive": "#e0af68",
    "--ficon-config": "#7dcfff",
    "--ficon-bio": "#bb9af7",
    "--ficon-vcs": "#f7768e",
  },
  ansi: {
    black: "#414868",
    red: "#f7768e",
    green: "#9ece6a",
    yellow: "#e0af68",
    blue: "#7aa2f7",
    magenta: "#bb9af7",
    cyan: "#7dcfff",
    white: "#a9b1d6",
    brightBlack: "#666f9b",
    brightRed: "#f7768e",
    brightGreen: "#9ece6a",
    brightYellow: "#e0af68",
    brightBlue: "#7aa2f7",
    brightMagenta: "#bb9af7",
    brightCyan: "#7dcfff",
    brightWhite: "#c0caf5",
  },
};

// --- One Dark Pro (Atom's One Dark) ------------------------------------------
// The de-facto dark editor theme: slate grey, soft grey text, the One
// syntax set. Surfaces step up from the editor's #282c34.

const oneDark: ThemeDef = {
  id: "one-dark",
  label: "One Dark Pro",
  kind: "dark",
  tokens: {
    "--bg": "#2c3139",
    "--fg": "#abb2bf",
    "--muted": "#9fa7b3",
    "--accent": "#61afef", // blue
    "--err": "#e36e77", // red
    "--warn": "#e5c07b", // yellow
    "--rate": "#c678dd", // purple
    "--rail-bg": "#30353f",
    "--row-hover": "#383e4a",
    "--row-active": "#3e4451", // selection
    "--term-selection": "#abb2bf30",
    "--term-bg": "#282c34", // the editor well
    "--scrim": "rgba(0, 0, 0, 0.5)",
    "--overlay-bg": "#2f343e",
    "--edge": "#3b4048",
    "--syn-keyword": "#c678dd",
    "--syn-string": "#98c379",
    "--syn-comment": "#7f8694",
    "--syn-number": "#d19a66",
    "--syn-type": "#e5c07b",
    "--syn-func": "#61afef",
    "--syn-def": "#abb2bf",
    "--syn-prop": "#e36e77",
    "--ficon-generic": "#8a919d",
    "--ficon-lang": "#61afef",
    "--ficon-data": "#56b6c2",
    "--ficon-doc": "#abb2bf",
    "--ficon-media": "#d19a66",
    "--ficon-archive": "#e5c07b",
    "--ficon-config": "#56b6c2",
    "--ficon-bio": "#c678dd",
    "--ficon-vcs": "#e06c75",
  },
  ansi: {
    black: "#3f4451",
    red: "#e36e77",
    green: "#98c379",
    yellow: "#e5c07b",
    blue: "#61afef",
    magenta: "#c678dd",
    cyan: "#56b6c2",
    white: "#abb2bf",
    brightBlack: "#79808e",
    brightRed: "#e06c75",
    brightGreen: "#98c379",
    brightYellow: "#e5c07b",
    brightBlue: "#61afef",
    brightMagenta: "#c678dd",
    brightCyan: "#56b6c2",
    brightWhite: "#dcdfe4",
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
    "--fg": "#474268", // text deepened a step: canonical #575279 sat within 15% of muted, so nothing receded
    "--muted": "#5f5a78", // subtle deepened to hold 4.5 on selected rows
    "--accent": "#286983", // pine
    "--err": "#a7586f", // love
    "--warn": "#a86304", // gold
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
    "--syn-def": "#474268",
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

// --- Terracotta (ivory and charcoal, a clay accent) --------------------------
// Warm paper tones: ivory sheets in the light half, charcoal in the dark, one
// terracotta accent, syntax in rust/olive/ochre. Chimaera's own warm pair.

const terracottaLight: ThemeDef = {
  id: "terracotta-light",
  label: "Terracotta Light",
  kind: "light",
  tokens: {
    "--bg": "#f4f2eb",
    "--fg": "#141413",
    "--muted": "#6b6a63",
    "--accent": "#c9623f",
    "--err": "#b3261e",
    "--warn": "#946300",
    "--rate": "#7c5cbf",
    "--rail-bg": "#efede5",
    "--row-hover": "#e8e6dd",
    "--row-active": "#e0ded4",
    "--term-selection": "#14141326",
    "--term-bg": "#faf9f5", // ivory — panes get the paper
    "--scrim": "rgba(0, 0, 0, 0.25)",
    "--overlay-bg": "#ffffff",
    "--edge": "#dcd9cf",
    "--syn-keyword": "#9b4d2f",
    "--syn-string": "#3f7a4a",
    "--syn-comment": "#807e75",
    "--syn-number": "#a4631a",
    "--syn-type": "#2f6f8f",
    "--syn-func": "#3a5fa8",
    "--syn-def": "#141413",
    "--syn-prop": "#6a4fb0",
    "--ficon-generic": "#85837a",
    "--ficon-lang": "#3a5fa8",
    "--ficon-data": "#3f7a4a",
    "--ficon-doc": "#6b6a63",
    "--ficon-media": "#c9623f",
    "--ficon-archive": "#a7651d",
    "--ficon-config": "#2f6f8f",
    "--ficon-bio": "#7c5cbf",
    "--ficon-vcs": "#b3261e",
  },
  ansi: {
    black: "#3d3c38",
    red: "#b3261e",
    green: "#3f7a4a",
    yellow: "#946300",
    blue: "#3a5fa8",
    magenta: "#8a4d9b",
    cyan: "#2c7d88",
    white: "#b8b6ad",
    brightBlack: "#75736b",
    brightRed: "#b3261e",
    brightGreen: "#3f7a4a",
    brightYellow: "#946300",
    brightBlue: "#3a5fa8",
    brightMagenta: "#8a4d9b",
    brightCyan: "#2f7f8a",
    brightWhite: "#e2e0d6",
  },
};

const terracottaDark: ThemeDef = {
  id: "terracotta-dark",
  label: "Terracotta Dark",
  kind: "dark",
  tokens: {
    "--bg": "#262624",
    "--fg": "#ece9e2",
    "--muted": "#a8a69c",
    "--accent": "#d97757",
    "--err": "#e8766a",
    "--warn": "#d9a74d",
    "--rate": "#b39ddb",
    "--rail-bg": "#2b2a27",
    "--row-hover": "#33322e",
    "--row-active": "#3b3a35",
    "--term-selection": "#ece9e233",
    "--term-bg": "#1f1e1b", // charcoal — panes get the deepest well
    "--scrim": "rgba(0, 0, 0, 0.5)",
    "--overlay-bg": "#2e2d29",
    "--edge": "#3f3e39",
    "--syn-keyword": "#e09a7c",
    "--syn-string": "#9ecf8c",
    "--syn-comment": "#8a887f",
    "--syn-number": "#e6b36a",
    "--syn-type": "#7fc3d8",
    "--syn-func": "#8fb5ea",
    "--syn-def": "#ece9e2",
    "--syn-prop": "#c5b4ea",
    "--ficon-generic": "#8a887f",
    "--ficon-lang": "#8fb5ea",
    "--ficon-data": "#9ecf8c",
    "--ficon-doc": "#a8a69c",
    "--ficon-media": "#d97757",
    "--ficon-archive": "#e6b36a",
    "--ficon-config": "#7fc3d8",
    "--ficon-bio": "#b39ddb",
    "--ficon-vcs": "#e8766a",
  },
  ansi: {
    black: "#45443f",
    red: "#e8766a",
    green: "#9ecf8c",
    yellow: "#d9a74d",
    blue: "#8fb5ea",
    magenta: "#d19ad8",
    cyan: "#7fc3d8",
    white: "#c9c6bd",
    brightBlack: "#8a887f",
    brightRed: "#e8766a",
    brightGreen: "#9ecf8c",
    brightYellow: "#d9a74d",
    brightBlue: "#8fb5ea",
    brightMagenta: "#d19ad8",
    brightCyan: "#7fc3d8",
    brightWhite: "#ece9e2",
  },
};

// --- Graphite (true neutrals, a soft green accent) ---------------------------
// No tint at all: white and near-black sheets, grey chrome, one green accent,
// syntax in clear primaries. The quiet pair.

const graphiteLight: ThemeDef = {
  id: "graphite-light",
  label: "Graphite Light",
  kind: "light",
  tokens: {
    "--bg": "#f9f9f9",
    "--fg": "#0d0d0d",
    "--muted": "#676773",
    "--accent": "#0f8f74",
    "--err": "#d02f2f",
    "--warn": "#9a6500",
    "--rate": "#7a5af5",
    "--rail-bg": "#f4f4f5",
    "--row-hover": "#ececee",
    "--row-active": "#e3e3e6",
    "--term-selection": "#0d0d0d26",
    "--term-bg": "#ffffff",
    "--scrim": "rgba(0, 0, 0, 0.25)",
    "--overlay-bg": "#ffffff",
    "--edge": "#e1e1e4",
    "--syn-keyword": "#8b3fc9",
    "--syn-string": "#1f7a4d",
    "--syn-comment": "#808089",
    "--syn-number": "#b25a12",
    "--syn-type": "#1d7aa8",
    "--syn-func": "#2e5fd3",
    "--syn-def": "#0d0d0d",
    "--syn-prop": "#5a4fd0",
    "--ficon-generic": "#808089",
    "--ficon-lang": "#2e5fd3",
    "--ficon-data": "#1f7a4d",
    "--ficon-doc": "#676773",
    "--ficon-media": "#b25a12",
    "--ficon-archive": "#9a6500",
    "--ficon-config": "#1d7aa8",
    "--ficon-bio": "#7a5af5",
    "--ficon-vcs": "#d02f2f",
  },
  ansi: {
    black: "#2b2b30",
    red: "#d02f2f",
    green: "#1f7a4d",
    yellow: "#8c6300",
    blue: "#2e5fd3",
    magenta: "#8b3fc9",
    cyan: "#1d7a8a",
    white: "#b4b4ba",
    brightBlack: "#6e6e78",
    brightRed: "#d02f2f",
    brightGreen: "#1f7a4d",
    brightYellow: "#8c6300",
    brightBlue: "#2e5fd3",
    brightMagenta: "#8b3fc9",
    brightCyan: "#1d7a8a",
    brightWhite: "#e6e6ea",
  },
};

const graphiteDark: ThemeDef = {
  id: "graphite-dark",
  label: "Graphite Dark",
  kind: "dark",
  tokens: {
    "--bg": "#1e1e1e",
    "--fg": "#ececec",
    "--muted": "#a0a0a8",
    "--accent": "#19c37d",
    "--err": "#ef6b6b",
    "--warn": "#e0b25a",
    "--rate": "#a99cf0",
    "--rail-bg": "#212121",
    "--row-hover": "#2a2a2a",
    "--row-active": "#333333",
    "--term-selection": "#ececec33",
    "--term-bg": "#171717",
    "--scrim": "rgba(0, 0, 0, 0.5)",
    "--overlay-bg": "#262626",
    "--edge": "#383838",
    "--syn-keyword": "#c792ea",
    "--syn-string": "#8fd3a0",
    "--syn-comment": "#7f7f88",
    "--syn-number": "#e8a95b",
    "--syn-type": "#6fc3e0",
    "--syn-func": "#82aaff",
    "--syn-def": "#ececec",
    "--syn-prop": "#b4a7f5",
    "--ficon-generic": "#8a8a90",
    "--ficon-lang": "#82aaff",
    "--ficon-data": "#8fd3a0",
    "--ficon-doc": "#a0a0a8",
    "--ficon-media": "#e8a95b",
    "--ficon-archive": "#e0b25a",
    "--ficon-config": "#6fc3e0",
    "--ficon-bio": "#c792ea",
    "--ficon-vcs": "#ef6b6b",
  },
  ansi: {
    black: "#3a3a3a",
    red: "#ef6b6b",
    green: "#8fd3a0",
    yellow: "#e0b25a",
    blue: "#82aaff",
    magenta: "#c792ea",
    cyan: "#6fc3e0",
    white: "#c4c4c4",
    brightBlack: "#8a8a90",
    brightRed: "#ef6b6b",
    brightGreen: "#8fd3a0",
    brightYellow: "#e0b25a",
    brightBlue: "#82aaff",
    brightMagenta: "#c792ea",
    brightCyan: "#6fc3e0",
    brightWhite: "#ececec",
  },
};

/** All themes, picker order (defaults first). */
export const THEMES: readonly ThemeDef[] = [
  chimaeraLight,
  githubLight,
  oneLight,
  catppuccinLatte,
  rosePineDawn,
  terracottaLight,
  graphiteLight,
  chimaeraDark,
  tokyoNight,
  oneDark,
  catppuccinMocha,
  terracottaDark,
  graphiteDark,
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
