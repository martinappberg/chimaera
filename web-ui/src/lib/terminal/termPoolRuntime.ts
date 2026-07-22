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
import { registerPathLinks } from "./links";
import { registerUrlLinks } from "./urlLinks";

function composeDispose(...disposers: Array<() => void>): () => void {
  return () => {
    for (const d of disposers) d();
  };
}
import type { PoolHandlers } from "./termPool";
import { BASE_FONT_SIZE, baseFontSize, fontFamily } from "./terminalMetrics";
import { activeTheme, getSetting, onSettingsChange } from "../settings/store.svelte";
import { isMac } from "../shared/keys";
import { copyText } from "../shared/clipboard";

const POOL_CAP = 12;
const REFIT_DEBOUNCE_MS = 80;
/** A PTY controls OSC 52 payloads. Bound decode + clipboard IPC memory so a
 * hostile process cannot turn one escape sequence into an unbounded client
 * allocation (roughly 1 MiB decoded text after base64 overhead). */
const OSC52_MAX_BASE64_BYTES = 1_400_000;
/**
 * Built-in default terminal font size (the terminal.fontSize schema default;
 * baseFontSize() is the live value). Readability pass 2026-07-06: 13 was
 * measurably small for dense TUI output; 13.5 won the screenshot comparison
 * against 14. At 1x displays both land in the same 8px cell (xterm rounds
 * the advance), but 14's true advance is 8.4px — glyphs get cramped — while
 * 13.5's 8.11px fits cleanly; it also keeps ~4% more columns. JetBrains
 * Mono's tall x-height reads crisply at 13.5 on both 1x and 2x.
 */
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

function themeFromTokens() {
  const cs = getComputedStyle(document.documentElement);
  const v = (name: string) => cs.getPropertyValue(name).trim();
  // The settings store applies the theme's tokens (and records the active
  // ThemeDef) before notifying subscribers, so both are already current.
  // Each theme carries its own hand-tuned ANSI palette (settings/themes.ts)
  // — xterm's defaults are the single loudest "unstyled demo" signal in a
  // terminal app, and a UI theme without its terminal palette is half a
  // theme.
  return {
    background: v("--term-bg"),
    foreground: v("--fg"),
    cursor: v("--fg"),
    cursorAccent: v("--term-bg"),
    selectionBackground: v("--term-selection"),
    ...activeTheme().ansi,
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

/**
 * Wire clipboard writes through the terminal — the same in a local PTY and a
 * remote one, since both are just bytes on the wire from the browser's view.
 *
 * OSC 52 is how a program running INSIDE the terminal (most often a remote
 * agent, whose "copy" has no other way back to the Mac clipboard) sets the
 * system clipboard, exactly as it would under iTerm2 or Terminal.app. xterm
 * has no built-in handler, so those copies were silently dropped. Clipboard
 * *reads* (`OSC 52 ; c ; ?`) are ignored on purpose: a program that can emit
 * escape codes must not be able to exfiltrate the clipboard back over the PTY.
 *
 * Cmd+C (Ctrl+Shift+C off macOS — bare Ctrl stays SIGINT) copies the
 * terminal's OWN selection: xterm keeps its selection off the DOM, so the
 * browser's native copy grabs nothing. copyOnSelect is the separate
 * as-you-select convenience; this is the explicit-chord path.
 *
 * All writes go through shared/clipboard's `copyText` — native-shell first
 * (WKWebView rejects non-gesture `navigator.clipboard` writes), browser
 * fallback second.
 */
function registerTerminalClipboard(term: Terminal): void {
  term.parser.registerOscHandler(52, (data) => {
    const semi = data.indexOf(";");
    if (semi === -1) return false; // malformed; let xterm's default run
    const payload = data.slice(semi + 1);
    if (payload === "" || payload === "?") return true; // read/clear: swallow, never leak
    if (payload.length > OSC52_MAX_BASE64_BYTES) return true;
    let text: string;
    try {
      text = new TextDecoder().decode(Uint8Array.from(atob(payload), (c) => c.charCodeAt(0)));
    } catch {
      return true; // not valid base64: swallow, like a native terminal
    }
    void copyText(text);
    return true;
  });

  term.attachCustomKeyEventHandler((e) => {
    if (e.type !== "keydown" || (e.key !== "c" && e.key !== "C")) return true;
    const copyChord = isMac
      ? e.metaKey && !e.ctrlKey && !e.altKey
      : e.ctrlKey && e.shiftKey && !e.metaKey && !e.altKey;
    if (!copyChord) return true;
    const selection = term.getSelection();
    if (selection === "") return true; // nothing selected: let the chord fall through
    void copyText(selection);
    e.preventDefault();
    e.stopPropagation();
    return false;
  });
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

  registerTerminalClipboard(term);

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
    // Clickable paths work in EVERY session — agents and shells alike. So do
    // proxyable URLs (the browser pane's front door); both providers share
    // one dispose.
    disposeLinks: composeDispose(
      registerPathLinks(term, id, {
        context: (sid) =>
          handlers?.linkContext(sid) ?? { cwd: null, root: null, workspaceId: null },
        open: (sid, path, kind, newSplit) => handlers?.onOpenPath(sid, path, kind, newSplit),
      }),
      registerUrlLinks(term, id, {
        open: (sid, target, newSplit) => handlers?.onOpenUrl(sid, target, newSplit),
      }),
    ),
  };
  fitEntry(entry);

  // Connect only after the terminal is open, visible, and fitted, so the
  // snapshot frame lands in a fully initialized terminal.
  entry.socket = new SessionSocket(id, {
    onBinary: (data) => term.write(data),
    onReset: (cols, rows) => {
      // The incoming snapshot was rendered at (cols, rows); adopt that grid
      // before replaying or every soft-wrapped row re-wraps at the wrong
      // column. The onResize echo this fires is a server-side no-op.
      if (cols !== undefined && rows !== undefined && (term.cols !== cols || term.rows !== rows)) {
        term.resize(cols, rows);
      }
      term.reset();
    },
    dims: () => ({ cols: term.cols, rows: term.rows }),
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
      void copyText(text);
    }
  });
  // Copy provenance: surface pastes so agent composers can be source-tagged.
  // Listener on xterm's own textarea; xterm still handles the paste itself.
  term.textarea?.addEventListener("paste", (e) => {
    const text = e.clipboardData?.getData("text");
    if (text !== undefined && text !== "") handlers?.onPaste(id, text);
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
    // A visible terminal outlives its session: a fast-dying agent's pane
    // must keep showing the process's last words (the missing-API-key
    // message IS the product here). Disposal happens once the tab closes
    // and the instance parks (or on LRU eviction).
    if (!live.has(entry.id) && !isVisible(entry)) disposeEntry(entry);
  }
}

/**
 * Force-dispose one session's pooled terminal, visible or not. The
 * chat⇄terminal toggle uses this: the PTY died on purpose, and a stale
 * warm instance would replay the dead socket's exited screen into the
 * session's next terminal view.
 */
export function disposeSession(id: string): void {
  const entry = pool.get(id);
  if (entry !== undefined) disposeEntry(entry);
}

/** The current grid size of a pooled session's terminal, if it is attached. */
export function getSize(id: string): { cols: number; rows: number } | null {
  const entry = pool.get(id);
  if (entry === undefined || !isVisible(entry)) return null;
  return { cols: entry.term.cols, rows: entry.term.rows };
}
