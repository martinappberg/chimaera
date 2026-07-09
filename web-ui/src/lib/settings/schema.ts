/**
 * The settings ground truth. Every user-tunable value in chimaera is declared
 * HERE, once — the settings UI, the settings.json editor (validation +
 * completion), the reactive store, and the generated JSON Schema all derive
 * from this registry. Adding a setting = adding one entry here and reading it
 * at the consumption site; nothing else to keep in sync.
 *
 * Values live as a flat map of dotted keys in `~/.config/chimaera/settings.json`
 * on the daemon host (only explicitly-set values are written, VS Code style).
 * The daemon serves and persists the map (GET/PUT /api/v1/settings) and
 * broadcasts changes over /ws/events, so every attached window — and the
 * daemon's own consumers (PTY scrollback, quick-open walker) — converge on
 * the same file.
 */

import { ACTIONS, parseChord } from "../keys";
import { themesOfKind, type ThemeDef } from "./themes";

/** Value types a setting can carry (mirrors the JSON representation). */
export type SettingValue = boolean | number | string | string[];

export type SettingType =
  | "boolean"
  | "number"
  | "integer"
  | "string"
  | "enum"
  | "color"
  | "string-list"
  | "keybinding";

export interface EnumOption {
  value: string;
  label: string;
  /**
   * Preview colors for card-style enum rendering (theme pickers):
   * [pane bg, rail bg, text, accent].
   */
  swatch?: readonly string[];
}

export interface SettingDef {
  /** Dotted id, e.g. "terminal.fontSize" — the settings.json key. */
  id: string;
  /** Short title shown in the settings UI ("Font Size"). */
  title: string;
  /** Category = UI section ("Terminal"); ordered by first appearance. */
  category: string;
  /** One- or two-sentence plain description (UI row + JSON hover). */
  description: string;
  type: SettingType;
  default: SettingValue;
  /** Numeric bounds/step (number/integer types). */
  min?: number;
  max?: number;
  step?: number;
  /** Choices (enum type). */
  options?: readonly EnumOption[];
  /** Input placeholder for string types whose "" default means "built-in". */
  placeholder?: string;
  /** Where the value is consumed; "daemon" values act on the server side. */
  scope: "client" | "daemon";
  /** Extra caveat rendered small in the UI ("Applies to new sessions."). */
  note?: string;
  /** Control override: "theme-cards" renders enum options as mini theme
   *  previews (requires options with `swatch`). */
  control?: "theme-cards";
}

/** `keys.<action>` ids, generated from the keys.ts registry. */
type KeyBindingId = `keys.${(typeof ACTIONS)[number]["id"]}`;

/**
 * Typed view of the settings map: ids -> value types. Kept adjacent to DEFS
 * (same file, same order) — the `satisfies` check below guarantees every
 * statically-declared key has a def and every default matches its declared
 * type; the keybinding rows come from the keys.ts registry.
 */
export type SettingsMap = {
  "appearance.theme": "system" | "light" | "dark";
  "appearance.lightTheme": string;
  "appearance.darkTheme": string;
  "appearance.accentColor": string;
  "agents.defaultView": "chat" | "terminal";
  "terminal.fontSize": number;
  "terminal.fontFamily": string;
  "terminal.lineHeight": number;
  "terminal.cursorStyle": "block" | "bar" | "underline";
  "terminal.cursorBlink": boolean;
  "terminal.scrollback": number;
  "terminal.minimumContrastRatio": number;
  "terminal.copyOnSelect": boolean;
  "terminal.macOptionIsMeta": boolean;
  "editor.fontSize": number;
  "editor.lineHeight": number;
  "editor.lineNumbers": boolean;
  "editor.wordWrap": boolean;
  "editor.tabSize": number;
  "files.showHidden": boolean;
  "files.tableRowsPerPage": number;
  "quickOpen.maxResults": number;
  "quickOpen.ignoreDirs": string[];
  "git.path": string;
  "agents.claude.path": string;
  "agents.codex.path": string;
  "agents.gemini.path": string;
  "agents.agy.path": string;
  "daemon.scrollbackLines": number;
  "daemon.restoreSessions": boolean;
  "update.autoCheck": boolean;
  "keys.modifier": "auto" | "cmd" | "ctrl-shift" | "alt";
} & Record<KeyBindingId, string>;

export type SettingId = keyof SettingsMap;

/** Theme-picker options with card-preview swatches, from the registry. */
function themeOptions(kind: "light" | "dark"): EnumOption[] {
  return themesOfKind(kind).map((t: ThemeDef) => ({
    value: t.id,
    label: t.label,
    swatch: [t.tokens["--term-bg"], t.tokens["--rail-bg"], t.tokens["--fg"], t.tokens["--accent"]],
  }));
}

const DEFS = {
  // --- Appearance -----------------------------------------------------------
  "appearance.theme": {
    title: "Mode",
    category: "Appearance",
    description:
      "Light or dark for the whole window. System follows the OS preference live; the palette comes from the Light Theme / Dark Theme choices below.",
    type: "enum",
    default: "system",
    options: [
      { value: "system", label: "System" },
      { value: "light", label: "Light" },
      { value: "dark", label: "Dark" },
    ],
    scope: "client",
  },
  "appearance.lightTheme": {
    title: "Light Theme",
    category: "Appearance",
    description: "Palette used while the window is light.",
    type: "enum",
    default: "chimaera-light",
    options: themeOptions("light"),
    control: "theme-cards",
    scope: "client",
  },
  "appearance.darkTheme": {
    title: "Dark Theme",
    category: "Appearance",
    description: "Palette used while the window is dark.",
    type: "enum",
    default: "chimaera-dark",
    options: themeOptions("dark"),
    control: "theme-cards",
    scope: "client",
  },
  "appearance.accentColor": {
    title: "Accent Color",
    category: "Appearance",
    description:
      "Accent used for focus, selection, live-session dots, and primary actions. Empty uses the selected theme's own accent.",
    type: "color",
    default: "",
    scope: "client",
  },

  // --- Agents ----------------------------------------------------------------
  "agents.defaultView": {
    title: "New Agent Sessions",
    category: "Agents",
    description:
      "How new Claude sessions open: the structured chat view, or the agent's own terminal UI. Every session can be switched either way from the pane bar; if the chat protocol ever fails to start, the session falls back to a terminal on its own.",
    type: "enum",
    default: "chat",
    options: [
      { value: "chat", label: "Chat" },
      { value: "terminal", label: "Terminal (TUI)" },
    ],
    scope: "client",
  },

  // --- Terminal --------------------------------------------------------------
  "terminal.fontSize": {
    title: "Font Size",
    category: "Terminal",
    description:
      "Default terminal text size in pixels. Individual panes can still override it with ⌘+/⌘− (persisted per pane).",
    type: "number",
    default: 13.5,
    min: 9,
    max: 28,
    step: 0.5,
    scope: "client",
  },
  "terminal.fontFamily": {
    title: "Font Family",
    category: "Terminal",
    description:
      "Terminal font stack. Empty uses the bundled JetBrains Mono (shipped inside the daemon — works air-gapped). A custom font must be installed on the machine running the browser.",
    type: "string",
    default: "",
    placeholder: "JetBrains Mono",
    scope: "client",
  },
  "terminal.lineHeight": {
    title: "Line Height",
    category: "Terminal",
    description:
      "Line height multiplier on the font's natural line box. 1.25 ≈ 1.65× font size for JetBrains Mono.",
    type: "number",
    default: 1.25,
    min: 1,
    max: 2,
    step: 0.05,
    scope: "client",
  },
  "terminal.cursorStyle": {
    title: "Cursor Style",
    category: "Terminal",
    description: "Shape of the terminal cursor.",
    type: "enum",
    default: "block",
    options: [
      { value: "block", label: "Block" },
      { value: "bar", label: "Bar" },
      { value: "underline", label: "Underline" },
    ],
    scope: "client",
  },
  "terminal.cursorBlink": {
    title: "Cursor Blink",
    category: "Terminal",
    description: "Blink the terminal cursor.",
    type: "boolean",
    default: false,
    scope: "client",
  },
  "terminal.scrollback": {
    title: "Scrollback (browser)",
    category: "Terminal",
    description:
      "Lines of scrollback kept in the browser-side terminal. The daemon keeps its own history for reattach — see Daemon: Scrollback Lines.",
    type: "integer",
    default: 5000,
    min: 200,
    max: 100000,
    step: 100,
    scope: "client",
  },
  "terminal.minimumContrastRatio": {
    title: "Minimum Contrast Ratio",
    category: "Terminal",
    description:
      "WCAG contrast floor the terminal enforces by nudging text colors. 1 disables it; 3 lifts illegible dim text without repainting intended colors; 4.5 recolors aggressively.",
    type: "number",
    default: 3,
    min: 1,
    max: 21,
    step: 0.5,
    scope: "client",
  },
  "terminal.copyOnSelect": {
    title: "Copy on Select",
    category: "Terminal",
    description: "Copy terminal selections to the clipboard as you make them.",
    type: "boolean",
    default: false,
    scope: "client",
  },
  "terminal.macOptionIsMeta": {
    title: "Option as Meta (macOS)",
    category: "Terminal",
    description:
      "Treat the Option key as Meta in the terminal (Emacs/readline Alt-bindings) instead of typing special characters.",
    type: "boolean",
    default: false,
    scope: "client",
  },

  // --- Editor ----------------------------------------------------------------
  "editor.fontSize": {
    title: "Font Size",
    category: "Editor",
    description: "Code and text editor font size in pixels.",
    type: "number",
    default: 12.5,
    min: 9,
    max: 28,
    step: 0.5,
    scope: "client",
  },
  "editor.lineHeight": {
    title: "Line Height",
    category: "Editor",
    description: "Editor line height multiplier.",
    type: "number",
    default: 1.55,
    min: 1,
    max: 2.4,
    step: 0.05,
    scope: "client",
  },
  "editor.lineNumbers": {
    title: "Line Numbers",
    category: "Editor",
    description: "Show the line-number gutter.",
    type: "boolean",
    default: true,
    scope: "client",
  },
  "editor.wordWrap": {
    title: "Word Wrap",
    category: "Editor",
    description: "Wrap long lines instead of scrolling horizontally.",
    type: "boolean",
    default: false,
    scope: "client",
  },
  "editor.tabSize": {
    title: "Tab Size",
    category: "Editor",
    description: "Columns a tab character renders as (and the indent unit when editing).",
    type: "integer",
    default: 4,
    min: 1,
    max: 8,
    step: 1,
    scope: "client",
  },

  // --- Files -----------------------------------------------------------------
  "files.showHidden": {
    title: "Show Hidden Files",
    category: "Files",
    description: "Show dotfiles in the FILES tree.",
    type: "boolean",
    default: false,
    scope: "client",
  },
  "files.tableRowsPerPage": {
    title: "Table Rows per Page",
    category: "Files",
    description:
      "Rows fetched per page when previewing CSV/TSV tables (more rows per fetch, fewer round-trips; the daemon caps a page at 1000).",
    type: "integer",
    default: 200,
    min: 50,
    max: 1000,
    step: 50,
    scope: "client",
  },

  // --- Quick Open ------------------------------------------------------------
  "quickOpen.maxResults": {
    title: "Max Results",
    category: "Quick Open",
    description: "Result rows the ⌘P palette shows.",
    type: "integer",
    default: 50,
    min: 10,
    max: 200,
    step: 10,
    scope: "client",
  },
  "quickOpen.ignoreDirs": {
    title: "Ignored Directories",
    category: "Quick Open",
    description:
      "Directory names the quick-open index skips at any depth. Replaces the built-in list (.git, node_modules, target, dist, __pycache__, .venv, venv, .snakemake, work).",
    type: "string-list",
    default: [],
    placeholder: "node_modules",
    scope: "daemon",
    note: "Consumed by the daemon's file walker; leave empty for the built-in list.",
  },

  // --- Git -------------------------------------------------------------------
  "git.path": {
    title: "Git Binary Path",
    category: "Git",
    description:
      "Absolute path to the git the source-control features use (status, diffs, worktrees). Empty resolves git from your login shell, then PATH. Set this when the host's default git is too old — e.g. an HPC login node still on git 1.8: run `module load git` (or similar), then paste `command -v git` here. chimaera needs git ≥ 2.15.",
    type: "string",
    default: "",
    placeholder: "resolve from login shell / PATH",
    scope: "daemon",
    note: "Resolved by the daemon; takes effect on the next status refresh.",
  },

  // --- Agents ----------------------------------------------------------------
  // Per-agent binary overrides. The key uses the agent's CLI id, so
  // Antigravity is `agents.agy.path` (matching the daemon's `agents.<id>.path`).
  // Empty resolves from the login shell, then a chimaera-managed install.
  "agents.claude.path": {
    title: "Claude Code Binary",
    category: "Agents",
    description:
      "Absolute path to the `claude` binary chimaera runs — for launched agents and when you type `claude` in a chimaera terminal. Empty resolves it from your login shell, then a chimaera-managed install. Set this when your install lives somewhere your login shell doesn't surface (e.g. ~/.npm-global/bin on an HPC node).",
    type: "string",
    default: "",
    placeholder: "resolve from login shell / PATH",
    scope: "daemon",
    note: "Resolved by the daemon; used for spawns and terminal shims.",
  },
  "agents.codex.path": {
    title: "Codex Binary",
    category: "Agents",
    description:
      "Absolute path to the `codex` binary chimaera runs. Empty resolves it from your login shell, then a chimaera-managed install.",
    type: "string",
    default: "",
    placeholder: "resolve from login shell / PATH",
    scope: "daemon",
    note: "Resolved by the daemon; used for spawns and terminal shims.",
  },
  "agents.gemini.path": {
    title: "Gemini CLI Binary",
    category: "Agents",
    description:
      "Absolute path to the `gemini` binary chimaera runs. Empty resolves it from your login shell, then a chimaera-managed install.",
    type: "string",
    default: "",
    placeholder: "resolve from login shell / PATH",
    scope: "daemon",
    note: "Resolved by the daemon; used for spawns and terminal shims.",
  },
  "agents.agy.path": {
    title: "Antigravity CLI Binary",
    category: "Agents",
    description:
      "Absolute path to the `agy` (Antigravity CLI) binary chimaera runs. Empty resolves it from your login shell, then a chimaera-managed install. Point this at the standalone CLI, not the Antigravity IDE's app launcher.",
    type: "string",
    default: "",
    placeholder: "resolve from login shell / PATH",
    scope: "daemon",
    note: "Resolved by the daemon; used for spawns and terminal shims.",
  },

  // --- Daemon ----------------------------------------------------------------
  "daemon.scrollbackLines": {
    title: "Scrollback Lines (daemon)",
    category: "Daemon",
    description:
      "Terminal history the daemon keeps per session — this is what survives disconnects and feeds reattach snapshots. Long pipeline logs want more; each line costs daemon memory.",
    type: "integer",
    default: 10000,
    min: 1000,
    max: 200000,
    step: 1000,
    scope: "daemon",
    note: "Applies to sessions started after the change.",
  },
  "daemon.restoreSessions": {
    title: "Restore Sessions on Restart",
    category: "Daemon",
    description:
      "When the daemon restarts (update, crash, machine reboot), bring sessions back where they were: shells respawn at their last directory, Claude conversations resume, other agents land in Recents. Off: nothing respawns, but agent conversations still retire into Recents.",
    type: "boolean",
    default: true,
    scope: "daemon",
  },

  // --- Updates ----------------------------------------------------------------
  "update.autoCheck": {
    title: "Check for Updates",
    category: "Updates",
    description:
      "Let the daemon check GitHub a few times a day for newer chimaera releases and offer them in the UI. Only the public releases feed is read; nothing installs without a click.",
    type: "boolean",
    default: true,
    scope: "daemon",
  },

  // --- Keyboard ----------------------------------------------------------------
  "keys.modifier": {
    title: "Base Modifier",
    category: "Keyboard",
    description:
      "The modifier every Mod-based chord builds on. Auto uses ⌘ on macOS and Ctrl+Shift elsewhere. The terminal always owns bare Ctrl, so there is no Ctrl-alone option.",
    type: "enum",
    default: "auto",
    options: [
      { value: "auto", label: "Auto" },
      { value: "cmd", label: "⌘ / Meta" },
      { value: "ctrl-shift", label: "Ctrl+Shift" },
      { value: "alt", label: "Alt" },
    ],
    scope: "client",
  },
} as const satisfies Record<Exclude<SettingId, KeyBindingId>, Omit<SettingDef, "id">>;

/**
 * The keybinding rows, straight from the keys.ts action registry — the
 * registry is the single source for labels, descriptions, and defaults.
 */
const KEY_DEFS: SettingDef[] = ACTIONS.map((a) => ({
  id: `keys.${a.id}`,
  title: a.label,
  category: "Keyboard",
  description: a.description,
  type: "keybinding",
  default: a.def,
  scope: "client",
}));

/** All settings, registry order (drives category and row order in the UI). */
export const SETTINGS: readonly SettingDef[] = [
  ...(Object.entries(DEFS).map(([id, def]) => ({ id, ...def })) as SettingDef[]),
  ...KEY_DEFS,
];

const BY_ID = new Map<string, SettingDef>(SETTINGS.map((d) => [d.id, d]));

export function settingDef(id: string): SettingDef | undefined {
  return BY_ID.get(id);
}

export function defaultValue<K extends SettingId>(id: K): SettingsMap[K] {
  return BY_ID.get(id)?.default as SettingsMap[K];
}

/** Categories in registry order. */
export const CATEGORIES: readonly string[] = [...new Set(SETTINGS.map((d) => d.category))];

/**
 * Validate `value` against `def`. Returns the value to USE (clamped for
 * out-of-range numbers, filtered for lists) or null when the value is
 * unusable and the default should apply. Never throws.
 */
export function sanitize(def: SettingDef, value: unknown): SettingValue | null {
  switch (def.type) {
    case "boolean":
      return typeof value === "boolean" ? value : null;
    case "number":
    case "integer": {
      if (typeof value !== "number" || !Number.isFinite(value)) return null;
      let v = value;
      if (def.type === "integer") v = Math.round(v);
      if (def.min !== undefined) v = Math.max(v, def.min);
      if (def.max !== undefined) v = Math.min(v, def.max);
      return v;
    }
    case "string":
      return typeof value === "string" ? value : null;
    case "color":
      return typeof value === "string" && (value === "" || /^#[0-9a-fA-F]{6}$/.test(value))
        ? value
        : null;
    case "enum":
      return typeof value === "string" && (def.options ?? []).some((o) => o.value === value)
        ? value
        : null;
    case "string-list":
      return Array.isArray(value)
        ? value.filter((x): x is string => typeof x === "string" && x.length > 0)
        : null;
    case "keybinding":
      // "" = binding disabled; validation is structural, so Mod tokens are
      // checked without caring which modifier setting is active.
      return typeof value === "string" && (value === "" || parseChord(value, "auto") !== null)
        ? value
        : null;
  }
}

/**
 * Human description of a def's expected type ("one of: system, light, dark";
 * "number 9–28") for validation messages in the JSON editor.
 */
export function expectedType(def: SettingDef): string {
  switch (def.type) {
    case "boolean":
      return "true or false";
    case "number":
    case "integer": {
      const kind = def.type === "integer" ? "integer" : "number";
      return def.min !== undefined && def.max !== undefined
        ? `${kind} ${def.min}–${def.max}`
        : kind;
    }
    case "string":
      return "string";
    case "color":
      return 'hex color like "#2e9e6b" (or "" for the default)';
    case "enum":
      return `one of: ${(def.options ?? []).map((o) => JSON.stringify(o.value)).join(", ")}`;
    case "string-list":
      return "array of strings";
    case "keybinding":
      return 'chord like "Mod+e" or "Meta+Shift+d" (or "" to disable)';
  }
}
