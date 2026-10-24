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

import { ACTIONS, parseChord } from "../shared/keys";
import { themesOfKind, type ThemeDef } from "./themes";
import { DICTATION_LANGUAGES } from "../chat/voiceLanguages";

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
  /** Unit suffix shown beside a numeric field ("px", "ms"). Display only. */
  unit?: string;
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
  "appearance.interfaceFontSize": number;
  "appearance.interfaceFontFamily": string;
  "agents.defaultView": "chat" | "terminal";
  "agents.communication.enabled": boolean;
  "agents.communication.wakes": "never" | "ask" | "auto" | "always";
  "agents.communication.freeSubagents": boolean;
  "dashboard.landing": "auto" | "never";
  "dashboard.cardDensity": "auto" | "comfortable" | "compact";
  "dashboard.roster": "line" | "cards";
  "notifications.turnFinished": boolean;
  "notifications.needsYou": boolean;
  "notifications.agentMessages": boolean;
  "notifications.sound": boolean;
  "notifications.whileFocused": boolean;
  "notifications.dockBadge": boolean;
  "chat.fontSize": number;
  "chat.fontFamily": string;
  "chat.lineHeight": number;
  "chat.contentWidth": number;
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
  "editor.fontFamily": string;
  "editor.lineHeight": number;
  "editor.markdownFontSize": number;
  "editor.markdownLineHeight": number;
  "editor.markdownDefaultMode": "reading" | "live" | "source";
  "editor.lineNumbers": boolean;
  "editor.wordWrap": boolean;
  "editor.tabSize": number;
  "editor.autosave": "off" | "afterDelay";
  "editor.autosaveDelay": number;
  "files.showHidden": boolean;
  "files.tableRowsPerPage": number;
  "quickOpen.maxResults": number;
  "quickOpen.ignoreDirs": string[];
  "git.path": string;
  "agents.claude.path": string;
  "agents.codex.path": string;
  "agents.gemini.path": string;
  "agents.agy.path": string;
  "agents.agy.chatPath": string;
  "agents.grok.path": string;
  "agents.cursor.path": string;
  "daemon.scrollbackLines": number;
  "daemon.restoreSessions": boolean;
  "chat.remoteControlAtStart": boolean;
  "chat.toolSummaries": boolean;
  "chat.codexSteering": "agent" | "next_step" | "immediate";
  "chat.newSessionModel": "remember" | "agent";
  "chat.resumeAfterRestart": boolean;
  "chat.voice": boolean;
  "chat.voiceMicrophone": string;
  "chat.voiceLanguage": string;
  "update.autoCheck": boolean;
  "plugins.allowUnverified": boolean;
  "plugins.toolsDir": string;
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
      "System follows the OS; the palettes for each mode are below.",
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
      "Focus, selection, live-session dots and primary actions. Empty uses the theme's own accent.",
    type: "color",
    default: "",
    scope: "client",
  },
  "appearance.interfaceFontSize": {
    title: "Interface Font Size",
    category: "Appearance",
    description:
      "The rail, file tree, tabs, dashboard, dialogs and this page. Chat, Terminal and Editor have their own sizes.",
    type: "number",
    default: 13.5,
    min: 11,
    max: 18,
    step: 0.5,
    unit: "px",
    scope: "client",
  },
  "appearance.interfaceFontFamily": {
    title: "Interface Font Family",
    category: "Appearance",
    description:
      "Empty uses the system font. A custom font must be installed on the machine showing the window.",
    type: "string",
    default: "",
    placeholder: "system interface font",
    scope: "client",
  },

  // --- Agents ----------------------------------------------------------------
  "agents.defaultView": {
    title: "New Agent Sessions",
    category: "Agents",
    description:
      "How the + new agent button opens a session. Any session switches between the two from its pane bar.",
    type: "enum",
    default: "chat",
    options: [
      { value: "chat", label: "Chat" },
      { value: "terminal", label: "Terminal (TUI)" },
    ],
    scope: "client",
  },
  // Agent communication (docs/design/agent-communication-plan.md §7): one global
  // switch, on by default; the daemon reads both from its cached map.
  "agents.communication.enabled": {
    title: "Agents can see and message each other",
    category: "Agents",
    description:
      "Agents can see who else is working in the workspace and exchange messages. Works in Chat for every built-in agent, and in Terminal for Claude Code and Codex. Messages wait in an inbox when they cannot be delivered immediately. Off also turns the Mastermind off.",
    type: "boolean",
    default: true,
    scope: "daemon",
    note: "New sessions get the tools; running ones stop at once and keep them listed until they restart.",
  },
  "agents.communication.wakes": {
    title: "Agents may wake each other",
    category: "Agents",
    description:
      "Whether another agent's message may start a turn in an idle agent — a turn billed to your account. Ask me asks every time, in the agent's chat and in Needs you; a reply to a question it asked still wakes it. Within limits wakes it on its own, up to 20 wakes in a workspace since you last stepped in (a prompt in one of its chats or Claude terminals, a Wake, a hand-over) and 4 rounds of one back-and-forth; past those you're asked once. Always wakes it every time, with no limits. Never keeps messages in its inbox until it looks or you hand them over. Terminal agents are never woken.",
    type: "enum",
    default: "ask",
    options: [
      { value: "never", label: "Never" },
      { value: "ask", label: "Ask me" },
      { value: "auto", label: "Within limits" },
      { value: "always", label: "Always" },
    ],
    scope: "daemon",
  },
  // Off by default: moving a subagent the user's turn waits on changes that
  // turn's flow, and the subagent's result then arrives in a turn of its own.
  "agents.communication.freeSubagents": {
    title: "Messages may move a waiting subagent to the background",
    category: "Agents",
    description:
      "A message from another agent may move this chat's waiting subagent to the background so it is read now; its result then arrives later.",
    type: "boolean",
    default: false,
    scope: "daemon",
    note: "Claude chats. Codex reads a message while it waits on its subagents either way.",
  },

  // --- Dashboard ---------------------------------------------------------------
  "dashboard.landing": {
    title: "Workspace Landing",
    category: "Dashboard",
    description:
      "What an empty workspace window opens on.",
    type: "enum",
    default: "auto",
    options: [
      { value: "auto", label: "Dashboard" },
      { value: "never", label: "First session" },
    ],
    scope: "client",
  },
  "dashboard.cardDensity": {
    title: "Agent Card Density",
    category: "Dashboard",
    description:
      "Auto switches to compact rows once seven or more agents are present.",
    type: "enum",
    default: "auto",
    options: [
      { value: "auto", label: "Auto" },
      { value: "comfortable", label: "Comfortable" },
      { value: "compact", label: "Compact" },
    ],
    scope: "client",
  },
  "dashboard.roster": {
    title: "Now",
    category: "Dashboard",
    description:
      "How the dashboard shows who is running.",
    type: "enum",
    default: "line",
    options: [
      { value: "line", label: "One line" },
      { value: "cards", label: "Cards" },
    ],
    scope: "client",
  },

  // --- Notifications -----------------------------------------------------------
  // Daemon-scoped: the daemon decides which notices exist at all (so every
  // consumer — the native shell, each browser tab — agrees), and hands the
  // presentation keys to the native shell alongside them.
  "notifications.turnFinished": {
    title: "When an Agent Finishes",
    category: "Notifications",
    description:
      "Notify when an agent ends its turn and hands the floor back, quoting how its reply starts. Nothing is sent for sessions in the window you're working in.",
    type: "boolean",
    default: true,
    scope: "daemon",
  },
  "notifications.needsYou": {
    title: "When an Agent Needs You",
    category: "Notifications",
    description:
      "Notify when an agent is blocked on you — a permission to approve, a question to answer, waiting for input, another agent's message waiting for you to wake it — or stops on an error or a usage limit.",
    type: "boolean",
    default: true,
    scope: "daemon",
  },
  "notifications.agentMessages": {
    title: "Messages from Agents",
    category: "Notifications",
    description:
      "Let agents notify you themselves — ask one to \"ping me when the job finishes\" and it can. Agents also see this switch: when it's off, their notify tool tells them so.",
    type: "boolean",
    default: true,
    scope: "daemon",
  },
  "notifications.sound": {
    title: "Play a Sound",
    category: "Notifications",
    description: "Notifications play the system notification sound.",
    type: "boolean",
    default: true,
    scope: "daemon",
  },
  "notifications.whileFocused": {
    title: "Notify While Chimaera Is in Front",
    category: "Notifications",
    description:
      "Also show notifications while you're working in Chimaera, for sessions in your other windows (never the window you're in). Off: only when Chimaera is in the background.",
    type: "boolean",
    default: true,
    scope: "daemon",
  },
  "notifications.dockBadge": {
    title: "Dock Badge",
    category: "Notifications",
    description:
      "Show how many agents are waiting on you on the app icon, and bounce it once when one gets blocked while Chimaera is in the background (native app).",
    type: "boolean",
    default: true,
    scope: "daemon",
  },

  // --- Chat ------------------------------------------------------------------
  "chat.fontSize": {
    title: "Font Size",
    category: "Chat",
    description:
      "The whole chat surface: transcript, composer, tool cards, trays and the dashboard's Mastermind.",
    type: "number",
    default: 14,
    min: 11,
    max: 24,
    step: 0.5,
    unit: "px",
    scope: "client",
  },
  "chat.fontFamily": {
    title: "Font Family",
    category: "Chat",
    description:
      "Empty follows the interface font; code stays in the monospace face.",
    type: "string",
    default: "",
    placeholder: "follow interface font",
    scope: "client",
  },
  "chat.lineHeight": {
    title: "Line Height",
    category: "Chat",
    description: "Line height multiplier for messages and the composer.",
    type: "number",
    default: 1.57,
    min: 1.2,
    max: 2.2,
    step: 0.01,
    scope: "client",
  },
  // Keyed `chat.*` from when only the transcript read it; it now sets every
  // reading column, so it sits with the window-wide Appearance choices. The
  // key stays: stored values and the wire never see the category.
  "chat.contentWidth": {
    title: "Reading Width",
    category: "Appearance",
    description:
      "Width of the reading column in chat transcripts (composer included) and rendered Markdown. Narrower panes fit their space.",
    type: "integer",
    // The Claude desktop app's reading column; ~97 characters of 14px text.
    default: 680,
    min: 480,
    max: 1400,
    step: 8,
    unit: "px",
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
    unit: "px",
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
    unit: "lines",
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
    unit: "px",
    scope: "client",
  },
  "editor.fontFamily": {
    title: "Font Family",
    category: "Editor",
    description:
      "Font stack for code, text, diffs, and the raw settings editor. Empty uses the bundled JetBrains Mono.",
    type: "string",
    default: "",
    placeholder: "JetBrains Mono",
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
  "editor.markdownFontSize": {
    title: "Markdown Preview Font Size",
    category: "Editor",
    description:
      "Body size for rendered Markdown. Defaults to the Chat size; Pane actions → Smaller / Larger text overrides it per pane.",
    type: "number",
    default: 14,
    min: 10,
    max: 32,
    step: 0.5,
    unit: "px",
    scope: "client",
  },
  "editor.markdownLineHeight": {
    title: "Markdown Preview Line Height",
    category: "Editor",
    description: "Line height multiplier for rendered Markdown previews.",
    type: "number",
    default: 1.65,
    min: 1.2,
    max: 2.4,
    step: 0.05,
    scope: "client",
  },
  "editor.markdownDefaultMode": {
    title: "Markdown Default Mode",
    category: "Editor",
    description:
      "How a Markdown file opens the first time: live (the rendered document; double-click a block to edit it in place — only that block shows as source — and Esc returns to the page), the read-only reading view, or raw source. After that, each file reopens in the mode you last picked.",
    type: "enum",
    default: "live",
    options: [
      { value: "reading", label: "Reading" },
      { value: "live", label: "Live" },
      { value: "source", label: "Source" },
    ],
    note: "Files over 1 MB and binary content always open in reading.",
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
  "editor.autosave": {
    title: "Autosave",
    category: "Editor",
    description:
      "Save edited files on their own: after a pause in typing, and when the editor loses focus or you switch tabs. Agents only see what is saved to disk. Never saves over a file that changed on disk until you resolve it.",
    type: "enum",
    default: "off",
    options: [
      { value: "off", label: "Off" },
      { value: "afterDelay", label: "After a delay" },
    ],
    scope: "client",
  },
  "editor.autosaveDelay": {
    title: "Autosave Delay",
    category: "Editor",
    description: "Milliseconds of idle typing before an autosave (when Autosave is on).",
    type: "integer",
    default: 1000,
    min: 200,
    max: 60000,
    step: 100,
    unit: "ms",
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
    unit: "rows",
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
      "Empty resolves git from your login shell, then PATH. Set it when the host's git is older than 2.15: after `module load git` (or similar), paste the output of `command -v git`.",
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

  "agents.grok.path": {
    title: "Grok Build Binary",
    category: "Agents",
    description: "Optional absolute path to Grok Build. Empty finds your installed copy automatically.",
    type: "string", default: "", placeholder: "resolve from login shell / PATH", scope: "daemon",
  },
  "agents.cursor.path": {
    title: "Cursor CLI Binary",
    category: "Agents",
    description: "Optional absolute path to the Cursor CLI. Empty finds your installed copy automatically.",
    type: "string", default: "", placeholder: "resolve from login shell / PATH", scope: "daemon",
  },
  "agents.agy.chatPath": {
    title: "Antigravity Chat Binary",
    category: "Agents",
    description: "Optional advanced override for Google's chat executable. Normally installed automatically with Antigravity.",
    type: "string", default: "", placeholder: "use the installed chat runtime", scope: "daemon",
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
    unit: "lines",
    scope: "daemon",
    note: "Applies to sessions started after the change.",
  },
  "chat.newSessionModel": {
    title: "New Chat Model",
    category: "Chat",
    description: "Use your last model and effort choices, or follow each agent’s configuration. Existing chats keep their settings.",
    type: "enum",
    default: "remember",
    options: [
      { value: "remember", label: "Last used" },
      { value: "agent", label: "Agent default" },
    ],
    scope: "daemon",
  },
  "chat.codexSteering": {
    title: "Codex Steering Timing",
    category: "Chat",
    description: "When Codex reads messages steered into an active turn, including messages from another agent. Agent default uses your Codex configuration (normally the next step). Immediate lets Codex interrupt its current model response or code-mode call. Messages queued for the next turn still wait.",
    type: "enum",
    default: "agent",
    options: [
      { value: "agent", label: "Agent default" },
      { value: "next_step", label: "Next step" },
      { value: "immediate", label: "Immediate (experimental)" },
    ],
    scope: "daemon",
    note: "Requires Codex 0.159.0 or later. Applies when a chat starts or resumes; running chats keep their current setting.",
  },
  "chat.remoteControlAtStart": {
    title: "Remote Control at Start (Claude)",
    category: "Chat",
    description:
      "Start every new Claude chat with Remote Control on, so the Claude app or claude.ai/code can pick it up. Otherwise turn it on per chat from Chat options. Only where your account allows it.",
    type: "boolean",
    default: false,
    scope: "daemon",
  },
  "chat.toolSummaries": {
    title: "Tool Activity Labels (Claude)",
    category: "Chat",
    description:
      "Label each run of tool calls in a Claude chat with a short summary of what it did (\"Listed files in directory\"), the way the Claude apps do. Each label is one small, fast model call on your account. Off: tool runs show their counts (\"3 commands\").",
    type: "boolean",
    default: true,
    scope: "daemon",
    note: "Applies to chat sessions started after the change.",
  },
  "chat.resumeAfterRestart": {
    title: "Pick Up Interrupted Work After a Restart",
    category: "Chat",
    description:
      "When the daemon restarts while a chat was mid-turn or running background work, tell the resumed agent what stopped so it can finish. This starts a turn on your account. Off: those chats come back idle.",
    type: "boolean",
    default: true,
    scope: "daemon",
  },
  "chat.voice": {
    title: "Voice Dictation",
    category: "Chat",
    description:
      "A mic button in every chat composer (`/voice on|off` flips this too). Speech goes to Claude's speech-to-text with the claude.ai login on the daemon's host, so the button shows only where that login exists; the mic is on only while you dictate.",
    type: "boolean",
    default: true,
    scope: "client",
  },
  "chat.voiceMicrophone": {
    title: "Dictation Microphone",
    category: "Chat",
    description:
      "The input dictation records from, by the name your system gives it. Empty uses the system default input. Right-click the mic button to pick from the microphones this machine has.",
    type: "string",
    default: "",
    placeholder: "system default",
    scope: "client",
  },
  "chat.voiceLanguage": {
    title: "Dictation Language",
    category: "Chat",
    description:
      "The language you dictate in. Match Browser uses this browser's language when Claude's speech service takes it, else English.",
    type: "enum",
    default: "auto",
    options: [
      { value: "auto", label: "Match Browser" },
      ...DICTATION_LANGUAGES.map((l) => ({ value: l.code, label: l.name })),
    ],
    scope: "client",
  },
  "daemon.restoreSessions": {
    title: "Restore Sessions on Restart",
    category: "Daemon",
    description:
      "When the daemon restarts (update, crash, machine reboot), bring sessions back in their saved view: shells restart in their last folder, supported agent conversations resume, and other agents appear in Recents. Off: nothing respawns, but agent conversations still retire into Recents.",
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

  // --- Extensions --------------------------------------------------------------
  "plugins.allowUnverified": {
    title: "Unverified Plugins",
    category: "Extensions",
    description:
      "Allow installing plugins the Chimaera maintainers haven't verified, each only after you trust what it can do. Off: only verified plugins install and run on this host. Whoever manages the machine can also turn this off for everyone.",
    type: "boolean",
    default: true,
    scope: "daemon",
  },
  "plugins.toolsDir": {
    title: "Plugin Tools Folder",
    category: "Extensions",
    description:
      "Where plugins put the tools they download (TeX Live is about 500 MB). Empty uses chimaera's folder under your home; on a small home quota, name a folder with room, such as $SCRATCH/chimaera-tools. `~` and $VARIABLES expand on the daemon's host. Tools already installed stay where they are.",
    type: "string",
    default: "",
    placeholder: "chimaera's own folder (under your home)",
    scope: "daemon",
    note: "Read by the daemon at each tool install; an install checks for room first.",
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
