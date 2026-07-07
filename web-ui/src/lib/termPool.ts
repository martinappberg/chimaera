/**
 * The shared xterm instance pool: one warm Terminal + SessionSocket per
 * session id, attached into whichever pane container currently shows that
 * session. Panes call show()/release(); detached instances park in a hidden
 * stash (sockets stay open, buffers stay warm) until the LRU cap evicts them.
 *
 * Refits are per-container (each entry owns a ResizeObserver on its slot),
 * debounced 80ms, and suppressed entirely while a divider drag is active —
 * setDragging(false) flushes the deferred fits once the drag ends.
 */

import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import { SessionSocket } from "./ws";
import { registerPathLinks, type LinkContext, type PathKind } from "./links";
import { getSetting, onSettingsChange } from "./settings/store.svelte";

const POOL_CAP = 12;
const REFIT_DEBOUNCE_MS = 80;
/**
 * Built-in default terminal font size (the terminal.fontSize schema default;
 * baseFontSize() is the live value). Readability pass 2026-07-06: 13 was
 * measurably small for dense TUI output; 13.5 won the screenshot comparison
 * against 14. At 1x displays both land in the same 8px cell (xterm rounds
 * the advance), but 14's true advance is 8.4px — glyphs get cramped — while
 * 13.5's 8.11px fits cleanly; it also keeps ~4% more columns. JetBrains
 * Mono's tall x-height reads crisply at 13.5 on both 1x and 2x.
 */
export const BASE_FONT_SIZE = 13.5;
/** Horizontal/vertical padding on .xterm (see app.css) — fit subtracts it. */
const PAD_X = 22;
const PAD_Y = 24;

export const FONT_FAMILY =
  '"JetBrains Mono", ui-monospace, "SF Mono", SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace';

/** The settings-resolved default font size (per-pane overrides layer on top). */
export function baseFontSize(): number {
  return getSetting("terminal.fontSize");
}

/** The settings-resolved font stack (a custom face falls back to bundled). */
function fontFamily(): string {
  const custom = getSetting("terminal.fontFamily").trim();
  return custom === "" ? FONT_FAMILY : `"${custom.replaceAll('"', "")}", ${FONT_FAMILY}`;
}

/**
 * Terminal options derived from settings. Line-height default 1.25: xterm
 * multiplies the face's NATURAL line box (~1.32 × font size for JetBrains
 * Mono), so 1.25 ≈ 1.65 × font size — already generous; 1.35 was screenshot-
 * compared and rejected. Contrast default 3.0: the 16-color palette below is
 * hand-tuned to >=4.5:1, but TUIs also emit 256-color grays measured at
 * 1.6–3.0:1 on our backgrounds — 3.0 lifts only those illegible cases while
 * 4.5 visibly recolors intended secondary text.
 */
function settingsOptions() {
  return {
    fontFamily: fontFamily(),
    lineHeight: getSetting("terminal.lineHeight"),
    cursorStyle: getSetting("terminal.cursorStyle"),
    cursorBlink: getSetting("terminal.cursorBlink"),
    scrollback: getSetting("terminal.scrollback"),
    minimumContrastRatio: getSetting("terminal.minimumContrastRatio"),
    macOptionIsMeta: getSetting("terminal.macOptionIsMeta"),
  };
}

export interface PoolHandlers {
  onTitle(id: string, title: string): void;
  onExited(id: string, status: number | null): void;
  /**
   * Fatal socket error for a session (auth rejection, session gone).
   * Protocol errors never reach the terminal scrollback — the app decides
   * how to surface them.
   */
  onSocketError(id: string, message: string): void;
  /**
   * The terminal's selection changed (context bridge). `text` is the raw
   * selection, empty when the selection was cleared.
   */
  onSelection(id: string, text: string): void;
  /** Resolution context for the session's path link provider. */
  linkContext(id: string): LinkContext;
  /**
   * A confirmed path link was clicked: open the file in an adjacent pane
   * (`newSplit` = Cmd/Ctrl held), or reveal a dir in the file tree.
   */
  onOpenPath(id: string, path: string, kind: PathKind, newSplit: boolean): void;
}

interface PoolEntry {
  id: string;
  term: Terminal;
  fit: FitAddon;
  socket: SessionSocket;
  el: HTMLDivElement;
  ro: ResizeObserver;
  lastUsed: number;
  fitTimer: ReturnType<typeof setTimeout> | null;
  pendingFit: boolean;
  /** Pane font-size override (px); undefined = follow terminal.fontSize. */
  fontOverride: number | undefined;
  /** Dispose the path link provider + its viewport prefetch. */
  disposeLinks: () => void;
}

// Plain non-reactive module state: xterm instances must never be $state.
const pool = new Map<string, PoolEntry>();
/** Which session each pane container currently wants (survives async gaps). */
const assignments = new Map<HTMLElement, string>();
let clock = 0;
let handlers: PoolHandlers | null = null;
let stash: HTMLDivElement | null = null;
let dragging = false;
let pendingFocusId: string | null = null;

// Ensure the terminal never opens before the bundled face is available —
// xterm measures glyph metrics once at open, and a fallback-font measure
// would leave every grid slightly wrong.
const fontsReady: Promise<void> =
  typeof document !== "undefined" && "fonts" in document
    ? Promise.allSettled([
        document.fonts.load(`400 ${BASE_FONT_SIZE}px "JetBrains Mono"`),
        document.fonts.load(`600 ${BASE_FONT_SIZE}px "JetBrains Mono"`),
      ]).then(() => undefined)
    : Promise.resolve();

// Muted ANSI palettes tuned to the app's neutrals; xterm's defaults are
// the single loudest "unstyled demo" signal in a terminal app.
//
// Readability pass 2026-07-06 (measured WCAG ratios against both --term-bg
// values): every color a TUI uses AS TEXT holds >= 4.5:1 (normal) or >= 3.5:1
// (bright variants, light scheme); dark brightBlack — claude's secondary text
// — was the worst offender at 3.03 and now sits at 4.65. `black` (dark) and
// `white`/`brightWhite` (light) stay near their backgrounds by ANSI semantics;
// the MIN_CONTRAST floor catches any TUI that types with them.
const LIGHT_ANSI = {
  black: "#3b3b41",
  red: "#bf4d56", // 4.76 on #fff
  green: "#2d8453", // 4.63 (was #2f8a57, 4.29)
  yellow: "#8c702b", // 4.70 (was #9a7b2f, 4.00)
  blue: "#3e6fc0", // 4.95
  magenta: "#95569f", // 5.11
  cyan: "#2b7e8d", // 4.70 (was #2f8a9b, 4.02)
  white: "#b9b9c0",
  brightBlack: "#73737d", // 4.69
  brightRed: "#d26873", // 3.52 (was 3.28)
  brightGreen: "#3e9866", // 3.57 (was 3.07)
  brightYellow: "#a18542", // 3.53 (was 2.83)
  brightBlue: "#5b89d5", // 3.51 (was 3.38)
  brightMagenta: "#ad74b8", // 3.52
  brightCyan: "#4293a1", // 3.55 (was 2.86)
  brightWhite: "#d9d9df",
};
const DARK_ANSI = {
  black: "#33333a",
  red: "#e2757e", // 6.45 on #0f0f13
  green: "#5cc48d", // 8.87
  yellow: "#d9b96c", // 10.11
  blue: "#79a5ea", // 7.63
  magenta: "#c795d3", // 7.89
  cyan: "#6cc3d4", // 9.47
  white: "#c9c9d1", // 11.62
  brightBlack: "#7c7c8a", // 4.65 (was #5f5f6a, 3.03 — claude's secondary text)
  brightRed: "#ef959c", // 8.61
  brightGreen: "#7fd6a8", // 11.01
  brightYellow: "#e7cd8b", // 12.30
  brightBlue: "#9cbbf1", // 9.83
  brightMagenta: "#d8afe2", // 10.15
  brightCyan: "#8fd6e4", // 11.75
  brightWhite: "#ededf3", // 16.40
};

function themeFromTokens() {
  const cs = getComputedStyle(document.documentElement);
  const v = (name: string) => cs.getPropertyValue(name).trim();
  // The settings store pins the resolved theme on <html> before notifying
  // subscribers, so the attribute (and every CSS var) is already current.
  const dark = document.documentElement.dataset.theme === "dark";
  return {
    background: v("--term-bg"),
    foreground: v("--fg"),
    cursor: v("--fg"),
    cursorAccent: v("--term-bg"),
    selectionBackground: v("--term-selection"),
    ...(dark ? DARK_ANSI : LIGHT_ANSI),
  };
}

function ensureStash(): HTMLDivElement {
  if (stash === null) {
    stash = document.createElement("div");
    stash.style.display = "none";
    stash.setAttribute("aria-hidden", "true");
    document.body.appendChild(stash);
  }
  return stash;
}

function isVisible(entry: PoolEntry): boolean {
  return entry.el.isConnected && entry.el.parentElement !== stash;
}

function fitEntry(entry: PoolEntry): void {
  if (!isVisible(entry)) return;
  // Never resize to degenerate dimensions (hidden or mid-layout element):
  // a tiny resize destroys buffer content client- and server-side.
  const dims = entry.fit.proposeDimensions();
  if (!dims || !isFinite(dims.cols) || !isFinite(dims.rows) || dims.cols < 2 || dims.rows < 2) {
    return;
  }
  // fit() resizes the terminal; term.onResize then sends the resize frame.
  entry.fit.fit();
}

function scheduleFit(entry: PoolEntry): void {
  if (dragging) {
    // Mid-drag refits cause visible reflow jank at 60fps; defer to drag end.
    entry.pendingFit = true;
    return;
  }
  if (entry.fitTimer !== null) clearTimeout(entry.fitTimer);
  entry.fitTimer = setTimeout(() => {
    entry.fitTimer = null;
    fitEntry(entry);
  }, REFIT_DEBOUNCE_MS);
}

function createEntry(id: string, parent: HTMLElement, fontOverride: number | undefined): PoolEntry {
  const fontSize = fontOverride ?? baseFontSize();
  const el = document.createElement("div");
  el.className = "term-slot";
  // The element must be visible and laid out BEFORE term.open(): opening in
  // a display:none element leaves xterm unmeasured, and the attach snapshot
  // written into that state is lost. Entries are only created on attach into
  // a live pane container, so visible-first holds.
  parent.appendChild(el);

  const term = new Terminal({
    ...settingsOptions(),
    fontSize,
    fontWeight: "400",
    fontWeightBold: "600",
    drawBoldTextInBrightColors: false,
    theme: themeFromTokens(),
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(el);

  // WebGL renderer with DOM fallback: on construction failure or context
  // loss, dispose the addon and let the DOM renderer take over.
  try {
    const webgl = new WebglAddon();
    webgl.onContextLoss(() => webgl.dispose());
    term.loadAddon(webgl);
  } catch {
    // WebGL unavailable; DOM renderer is already active.
  }

  const entry: PoolEntry = {
    id,
    term,
    fit,
    // placeholder; assigned right below (socket handlers close over `term`)
    socket: null as unknown as SessionSocket,
    el,
    ro: null as unknown as ResizeObserver,
    lastUsed: ++clock,
    fitTimer: null,
    pendingFit: false,
    fontOverride,
    // Clickable paths work in EVERY session — agents and shells alike.
    disposeLinks: registerPathLinks(term, id, {
      context: (sid) => handlers?.linkContext(sid) ?? { cwd: null, root: null },
      open: (sid, path, kind, newSplit) => handlers?.onOpenPath(sid, path, kind, newSplit),
    }),
  };
  fitEntry(entry);

  // Connect only after the terminal is open, visible, and fitted, so the
  // snapshot frame lands in a fully initialized terminal.
  entry.socket = new SessionSocket(id, {
    onBinary: (data) => term.write(data),
    onReset: () => term.reset(),
    onTitle: (title) => handlers?.onTitle(id, title),
    onResized: (cols, rows) => {
      if (term.cols !== cols || term.rows !== rows) term.resize(cols, rows);
    },
    onExited: (status) => {
      term.write("\r\n\x1b[2m[exited]\x1b[0m\r\n");
      handlers?.onExited(id, status);
    },
    onError: (message) => {
      // Never write protocol errors into the PTY scrollback; route them to
      // the app (which shows the re-auth overlay on "unauthorized").
      handlers?.onSocketError(id, message);
    },
  });

  term.onData((data) => entry.socket.sendInput(data));
  term.onResize(({ cols, rows }) => entry.socket.sendResize(cols, rows));
  term.onSelectionChange(() => {
    const text = term.getSelection();
    handlers?.onSelection(id, text);
    if (text.length > 0 && getSetting("terminal.copyOnSelect")) {
      void navigator.clipboard?.writeText(text).catch(() => {
        // clipboard permission denied; selection still works normally
      });
    }
  });

  entry.ro = new ResizeObserver(() => scheduleFit(entry));
  entry.ro.observe(el);

  pool.set(id, entry);
  return entry;
}

function disposeEntry(entry: PoolEntry): void {
  pool.delete(entry.id);
  if (entry.fitTimer !== null) clearTimeout(entry.fitTimer);
  entry.disposeLinks();
  entry.socket.close();
  entry.ro.disconnect();
  entry.term.dispose();
  entry.el.remove();
}

/** LRU-evict past the cap; only parked (non-visible) instances are disposable. */
function evictLru(): void {
  while (pool.size > POOL_CAP) {
    let oldest: PoolEntry | null = null;
    for (const e of pool.values()) {
      if (!isVisible(e) && (oldest === null || e.lastUsed < oldest.lastUsed)) {
        oldest = e;
      }
    }
    if (oldest === null) break;
    disposeEntry(oldest);
  }
}

function attach(id: string, host: HTMLElement, fontOverride: number | undefined): void {
  ensureStash();
  let entry = pool.get(id);
  if (entry === undefined) {
    entry = createEntry(id, host, fontOverride);
  } else {
    if (entry.el.parentElement !== host) {
      host.appendChild(entry.el);
    }
    // The destination pane's font size wins (override, else the settings
    // default); changing it re-measures the glyph atlas, so refit.
    entry.fontOverride = fontOverride;
    const fontSize = fontOverride ?? baseFontSize();
    if (entry.term.options.fontSize !== fontSize) {
      entry.term.options.fontSize = fontSize;
      fitEntry(entry);
    }
  }
  entry.lastUsed = ++clock;
  evictLru();
  const e = entry;
  // Hand focus over synchronously — the element is attached and xterm's
  // textarea exists; waiting for a rAF drops keystrokes typed in the gap
  // (and throttled rAFs can delay it indefinitely).
  if (pendingFocusId === id) {
    pendingFocusId = null;
    e.term.focus();
  }
  requestAnimationFrame(() => {
    if (e.el.parentElement !== host) return;
    fitEntry(e);
  });
}

/** Wire the app-level callbacks and settings tracking. Call once on mount. */
export function initPool(h: PoolHandlers): void {
  handlers = h;
  ensureStash();
  // Settings ground truth: any change (this window, another window, or a
  // hand-edit of settings.json) re-applies live to every warm terminal.
  // System theme flips arrive through the same channel — the store resolves
  // "system" and re-pins data-theme before notifying.
  unsubscribeSettings = onSettingsChange(applySettingsToPool);
}

function applySettingsToPool(): void {
  const opts = settingsOptions();
  const theme = themeFromTokens();
  for (const e of pool.values()) {
    Object.assign(e.term.options, opts, { theme });
    // Panes without a per-pane override follow the default size live.
    const size = e.fontOverride ?? baseFontSize();
    if (e.term.options.fontSize !== size) e.term.options.fontSize = size;
    // Metrics-affecting options (font, line height) change the cell grid.
    scheduleFit(e);
  }
}

let unsubscribeSettings: (() => void) | null = null;

/** Tear the pool down (app unmount). */
export function disposePool(): void {
  unsubscribeSettings?.();
  unsubscribeSettings = null;
  for (const entry of [...pool.values()]) disposeEntry(entry);
  assignments.clear();
  handlers = null;
  stash?.remove();
  stash = null;
}

/**
 * Show `id`'s terminal inside `host` (a pane's content container) at the
 * pane's font size (undefined = the default). Also the path for live font
 * changes: re-invoked with a new size while attached, it just re-measures.
 */
export function show(id: string, host: HTMLElement, fontSize?: number): void {
  assignments.set(host, id);
  void fontsReady.then(() => {
    // The pane may have moved on (tab switch, unmount) while fonts loaded.
    if (assignments.get(host) !== id || handlers === null) return;
    attach(id, host, fontSize);
  });
}

/** Detach `id` from `host` back into the warm stash (never kills the session). */
export function release(id: string, host: HTMLElement): void {
  if (assignments.get(host) === id) assignments.delete(host);
  const entry = pool.get(id);
  if (entry !== undefined && entry.el.parentElement === host) {
    ensureStash().appendChild(entry.el);
  }
}

/** Focus the session's terminal, deferring until it is attached if needed. */
export function focusTerminal(id: string): void {
  const entry = pool.get(id);
  if (entry !== undefined && isVisible(entry)) {
    entry.term.focus();
  } else {
    pendingFocusId = id;
  }
}

/**
 * Type `text` into the session's live socket (context bridge references).
 * Returns false when the session has no pooled entry or its socket is down —
 * the caller falls back to a one-shot socket. Callers guarantee `text`
 * carries no newline (never submits).
 */
export function sendText(id: string, text: string): boolean {
  const entry = pool.get(id);
  if (entry === undefined || !entry.socket.isOpen) return false;
  entry.socket.sendInput(text);
  return true;
}

/** Divider-drag coordination: suppress refits mid-drag, flush at drag end. */
export function setDragging(v: boolean): void {
  if (dragging === v) return;
  dragging = v;
  if (!v) {
    for (const e of pool.values()) {
      if (e.pendingFit) {
        e.pendingFit = false;
        fitEntry(e);
      }
    }
  }
}

/** Dispose entries whose sessions no longer exist on the daemon. */
export function syncSessions(liveIds: readonly string[]): void {
  const live = new Set(liveIds);
  for (const entry of [...pool.values()]) {
    if (!live.has(entry.id)) disposeEntry(entry);
  }
}

/** The current grid size of a pooled session's terminal, if it is attached. */
export function getSize(id: string): { cols: number; rows: number } | null {
  const entry = pool.get(id);
  if (entry === undefined || !isVisible(entry)) return null;
  return { cols: entry.term.cols, rows: entry.term.rows };
}

let cellCache: { key: string; w: number; h: number } | null = null;

/** Measure one terminal cell the way xterm will (DOM probe), at the current
 *  settings-resolved font metrics. */
function cellDims(): { w: number; h: number } {
  const size = baseFontSize();
  const family = fontFamily();
  const lineHeight = getSetting("terminal.lineHeight");
  const key = `${size}|${lineHeight}|${family}`;
  if (cellCache !== null && cellCache.key === key) return cellCache;
  const probe = document.createElement("span");
  probe.textContent = "W";
  probe.style.cssText = `position:absolute;visibility:hidden;white-space:pre;font:400 ${size}px ${family};line-height:${lineHeight};`;
  document.body.appendChild(probe);
  const rect = probe.getBoundingClientRect();
  probe.remove();
  const dims = {
    key,
    w: rect.width > 1 ? rect.width : size * 0.6,
    h: rect.height > 1 ? rect.height : Math.round(size * lineHeight),
  };
  // Only cache once the bundled font has had a chance to load.
  if (document.fonts.check(`400 ${size}px "JetBrains Mono"`)) cellCache = dims;
  return dims;
}

/**
 * Estimate the cols/rows a terminal would get inside `el` (used to spawn
 * sessions at the right size so TUIs never boot at 80x24 and then resync).
 */
export function estimateSize(el: HTMLElement): { cols: number; rows: number } {
  const { w, h } = cellDims();
  const cols = Math.floor((el.clientWidth - PAD_X) / w);
  const rows = Math.floor((el.clientHeight - PAD_Y) / h);
  return {
    cols: Math.min(Math.max(cols, 20), 500),
    rows: Math.min(Math.max(rows, 5), 200),
  };
}
