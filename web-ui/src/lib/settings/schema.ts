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

/** Value types a setting can carry (mirrors the JSON representation). */
export type SettingValue = boolean | number | string | string[];

export type SettingType =
  | "boolean"
  | "number"
  | "integer"
  | "string"
  | "enum"
  | "color"
  | "string-list";

export interface EnumOption {
  value: string;
  label: string;
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
}

/**
 * Typed view of the settings map: ids -> value types. Kept adjacent to DEFS
 * (same file, same order) — the `satisfies` check below guarantees every key
 * has a def and every default matches its declared type.
 */
export interface SettingsMap {
  "appearance.theme": "system" | "light" | "dark";
  "appearance.accentColor": string;
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
  "daemon.scrollbackLines": number;
}

export type SettingId = keyof SettingsMap;

const DEFS = {
  // --- Appearance -----------------------------------------------------------
  "appearance.theme": {
    title: "Theme",
    category: "Appearance",
    description:
      "Color theme for the whole window. System follows the OS light/dark preference live.",
    type: "enum",
    default: "system",
    options: [
      { value: "system", label: "System" },
      { value: "light", label: "Light" },
      { value: "dark", label: "Dark" },
    ],
    scope: "client",
  },
  "appearance.accentColor": {
    title: "Accent Color",
    category: "Appearance",
    description:
      "Accent used for focus, selection, live-session dots, and primary actions. Empty uses the theme's built-in green (tuned per light/dark).",
    type: "color",
    default: "",
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
} as const satisfies Record<SettingId, Omit<SettingDef, "id">>;

/** All settings, registry order (drives category and row order in the UI). */
export const SETTINGS: readonly SettingDef[] = Object.entries(DEFS).map(([id, def]) => ({
  id,
  ...def,
})) as SettingDef[];

const BY_ID = new Map<string, SettingDef>(SETTINGS.map((d) => [d.id, d]));

export function settingDef(id: string): SettingDef | undefined {
  return BY_ID.get(id);
}

export function defaultValue<K extends SettingId>(id: K): SettingsMap[K] {
  return DEFS[id].default as SettingsMap[K];
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
  }
}

/**
 * JSON Schema (draft-07) for settings.json, generated from the registry —
 * usable by external editors and by our own JSON validation. Unknown keys
 * are allowed (forward compatibility; the daemon preserves them verbatim).
 */
export function settingsJsonSchema(): Record<string, unknown> {
  const properties: Record<string, unknown> = {};
  for (const def of SETTINGS) {
    const prop: Record<string, unknown> = {
      description: def.description,
      default: def.default,
    };
    switch (def.type) {
      case "boolean":
        prop.type = "boolean";
        break;
      case "number":
      case "integer":
        prop.type = def.type;
        if (def.min !== undefined) prop.minimum = def.min;
        if (def.max !== undefined) prop.maximum = def.max;
        break;
      case "string":
        prop.type = "string";
        break;
      case "color":
        prop.type = "string";
        prop.pattern = "^$|^#[0-9a-fA-F]{6}$";
        break;
      case "enum":
        prop.enum = (def.options ?? []).map((o) => o.value);
        break;
      case "string-list":
        prop.type = "array";
        prop.items = { type: "string" };
        break;
    }
    properties[def.id] = prop;
  }
  return {
    $schema: "http://json-schema.org/draft-07/schema#",
    title: "Chimaera settings",
    type: "object",
    properties,
    additionalProperties: true,
  };
}
