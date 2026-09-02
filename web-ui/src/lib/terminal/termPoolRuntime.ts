/**
 * The shared xterm instance pool: one warm Terminal + SessionSocket per
 * session id, attached into whichever pane container currently shows that
 * session. Panes call show()/release(); detached instances park in a hidden
 * stash (sockets stay open, buffers stay warm) until the LRU cap evicts them.
 *
 * Parked terminals buffer, don't parse: output for a stashed instance queues
 * as raw bytes in a bounded per-entry ParkedBuffer and replays into
 * term.write on adopt, so a busy hidden session costs no steady main-thread
 * escape parsing (one busy TUI parsed hidden measured 10-17% renderer CPU,
 * × up to POOL_CAP). Parking is an explicit lifecycle flag (release() parks,
 * adopt unparks) — never derived from DOM topology, which pane drag-out
 * flows can rearrange without a release. Buffer overflow and foreign resizes
 * discard the stream and latch needs-resync: adopt then forces a clean
 * socket re-attach (the server fully re-snapshots). Nothing depends on
 * parked parsing: titles/cwd/busy state are all daemon-derived, and the link
 * prefetch hooks onRender (inert while hidden). Two bounded hidden parses
 * remain by design: a reset's adjacent snapshot frame is written through
 * immediately (cheaper than discarding it and re-rendering server-side on
 * adopt), and an exit flushes the buffered tail (the last words). Deferred
 * client-parse side effects (OSC 52 clipboard writes) therefore usually land
 * on adopt — but those two paths can still parse, and so still copy, while
 * hidden.
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
import { ParkedBuffer } from "./parkedBuffer";
import { createLocalEcho, type LocalEcho } from "./localEcho";
import { registerPathLinks } from "./links";
import { registerUrlLinks } from "./urlLinks";
import type { PoolHandlers } from "./termPool";
import { BASE_FONT_SIZE, baseFontSize, fontFamily } from "./terminalMetrics";
import { activeTheme, getSetting, onSettingsChange } from "../settings/store.svelte";
import { isMac } from "../shared/keys";
import { copyText } from "../shared/clipboard";

const POOL_CAP = 12;
const REFIT_DEBOUNCE_MS = 80;
/**
 * Cap on bytes buffered for a parked (hidden) terminal. Beyond this the
 * buffer is discarded and the entry latched desynced: replaying a truncated
 * escape stream would corrupt the grid, and a fresh attach re-snapshots the
 * authoritative server state anyway. 512 KiB comfortably holds minutes of
 * ordinary shell output; only a firehose overflows it. (Snapshots don't
 * count against it — they write through; see ParkedBuffer.)
 */
const PARKED_BUFFER_MAX_BYTES = 512 * 1024;
/**
 * WebGL context losses tolerated before an entry latches to the DOM
 * renderer for good. Losses are usually transient (GPU pressure from too
 * many live contexts, a backgrounded tab) and adopt retries acceleration —
 * but unbounded retries across a 12-entry pool can ping-pong context
 * eviction, each loss costing a full glyph-atlas rebuild.
 */
const WEBGL_MAX_LOSSES = 2;

/** Fold several dispose callbacks into one (the path- and URL-link providers
 *  a pooled terminal registers share a single `disposeLinks`). */
function composeDispose(...disposers: Array<() => void>): () => void {
  return () => {
    for (const d of disposers) d();
  };
}
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
  /** Output routing while parked; see parkedBuffer.ts for the states. */
  buf: ParkedBuffer;
  /** Predictive local echo for high-RTT remotes; see localEcho.ts. */
  echo: LocalEcho;
  /** Live WebGL addon while the renderer is accelerated; null after loss. */
  webgl: WebglAddon | null;
  /**
   * WebGL construction threw (unavailable): never retried. Context LOSS
   * clears `webgl` but not this — the next adopt retries acceleration,
   * bounded by WEBGL_MAX_LOSSES.
   */
  webglFailed: boolean;
  /** Context losses so far; at WEBGL_MAX_LOSSES the DOM renderer is final. */
  webglLosses: number;
  /**
   * Set at the top of disposeEntry: socket close and term.dispose cannot
   * stop already-queued events (a late onmessage, a WebGL context-loss
   * callback) from firing into freed resources — this flag does.
   */
  disposed: boolean;
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

/**
 * Resize-before-reset, the grid-adoption ordering for an incoming snapshot:
 * it was rendered at (cols, rows), and replaying at any other width re-wraps
 * every soft-wrapped row at the wrong column. The onResize echo the resize
 * fires is a server-side no-op.
 */
function applyReset(term: Terminal, cols?: number, rows?: number): void {
  if (cols !== undefined && rows !== undefined && (term.cols !== cols || term.rows !== rows)) {
    term.resize(cols, rows);
  }
  term.reset();
}

/**
 * Catch a just-adopted entry up with its session: replay the deferred bytes,
 * or — when the parked stream was discarded — resync the socket so the
 * server re-snapshots from the authoritative grid. Must run synchronously
 * with the reparent into the host: once unparked, live writes bypass the
 * buffer, and nothing may interleave ahead of the flush. The desynced latch
 * clears only when the resync was actually issued — a refused resync
 * (fatal/closed socket) keeps it, and the existing error surface stands.
 */
function adoptParked(entry: PoolEntry): void {
  // show() re-invokes attach() on already-visible entries (live font
  // changes): only a genuinely parked entry may emit protocol traffic —
  // unpark without a preceding park is not a well-formed lifecycle event.
  const wasParked = entry.buf.isParked();
  const directive = entry.buf.adopt();
  if (directive.resync) {
    // A fresh visible connect re-auths with parked:false — the server sends
    // a full snapshot; no unpark needed on the abandoned connection.
    if (entry.socket.resync()) entry.buf.resyncIssued();
    return;
  }
  for (const chunk of directive.flush) entry.term.write(chunk);
  // Resume the server stream: it catches up from its ring (contiguous with
  // the flushed bytes — the server stopped sending exactly where the park
  // frame landed), or repaints when the ring can't cover the gap.
  if (wasParked) entry.socket.sendUnpark();
}

/**
 * Load (or re-load) the WebGL renderer. On context loss the addon disposes
 * itself and xterm's DOM renderer takes over; the next adopt retries —
 * losses are usually transient (GPU pressure from too many live contexts, a
 * backgrounded tab) — until WEBGL_MAX_LOSSES latches the DOM renderer for
 * good. A constructor throw marks WebGL unavailable immediately.
 */
function loadWebgl(entry: PoolEntry): void {
  if (entry.webgl !== null || entry.webglFailed || entry.webglLosses >= WEBGL_MAX_LOSSES) {
    return;
  }
  try {
    const webgl = new WebglAddon();
    webgl.onContextLoss(() => {
      // A loss event queued behind disposeEntry must not double-dispose.
      if (entry.disposed) return;
      webgl.dispose();
      if (entry.webgl === webgl) {
        entry.webgl = null;
        entry.webglLosses += 1;
      }
    });
    entry.term.loadAddon(webgl);
    entry.webgl = webgl;
  } catch {
    entry.webglFailed = true; // WebGL unavailable; the DOM renderer stays.
  }
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
  hoistStyles(el);

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
        menu: (event, url) => handlers?.onUrlMenu(event, url),
      }),
    ),
    buf: new ParkedBuffer(PARKED_BUFFER_MAX_BYTES),
    // Ghosting a keystroke the closed socket silently dropped would show
    // input that was never delivered — the socket gate is non-negotiable.
    echo: createLocalEcho(term, () => entry.socket.isOpen && (handlers?.echoArmed?.(id) ?? false)),
    webgl: null,
    webglFailed: false,
    webglLosses: 0,
    disposed: false,
  };
  // WebGL renderer with DOM fallback (and an adopt-time retry after loss).
  loadWebgl(entry);
  fitEntry(entry);

  // Connect only after the terminal is open, visible, and fitted, so the
  // snapshot frame lands in a fully initialized terminal.
  entry.socket = new SessionSocket(id, {
    onBinary: (data) => {
      // Parked terminals buffer, don't parse (see the module header) — the
      // one exception is the snapshot write-through after a reset.
      if (entry.disposed) return;
      if (entry.buf.binary(data) === "write") {
        if (entry.echo.hasGhosts()) {
          entry.echo.onOutput(data);
          // Re-anchor surviving ghosts only once xterm has PARSED the chunk
          // (its write pipeline is async) — a rAF can fire mid-parse and
          // re-draw ghosts at the stale cursor over already-echoed text.
          term.write(data, () => {
            if (!entry.disposed) entry.echo.rerender();
          });
        } else {
          term.write(data);
        }
      }
    },
    onReset: (cols, rows) => {
      if (entry.disposed) return;
      // The adjacent snapshot supersedes everything buffered; resize+reset
      // are cheap enough to apply even while parked, and the snapshot then
      // writes through (see ParkedBuffer).
      entry.buf.reset();
      entry.echo.clear();
      applyReset(term, cols, rows);
    },
    dims: () => ({ cols: term.cols, rows: term.rows }),
    onTitle: (title) => handlers?.onTitle(id, title),
    onResized: (cols, rows) => {
      if (entry.disposed) return;
      // While parked this discards old-width bytes and latches desynced —
      // they are unreplayable in the reflowed grid (the server's debounced
      // foreign-resize resync, or adopt, repaints). A reflow is a grid
      // discontinuity: pending ghosts hold stale pixel coordinates.
      entry.buf.resized();
      entry.echo.clear();
      if (term.cols !== cols || term.rows !== rows) term.resize(cols, rows);
    },
    onExited: (status) => {
      if (entry.disposed) return;
      // A parked terminal's buffered tail is its last words — parse it now
      // (one-time, bounded) so the final screen isn't lost. A parked exit
      // also latches desynced (the buffer can't be trusted to be complete
      // when a park-aware server withheld output): adopt reconnects into
      // the server's last-words replay for the authoritative final screen.
      for (const chunk of entry.buf.exited()) term.write(chunk);
      entry.echo.clear();
      term.write("\r\n\x1b[2m[exited]\x1b[0m\r\n");
      handlers?.onExited(id, status);
    },
    onError: (message) => {
      // Never write protocol errors into the PTY scrollback; route them to
      // the app (which shows the re-auth overlay on "unauthorized").
      handlers?.onSocketError(id, message);
    },
    parked: () => entry.buf.isParked(),
    onParkedReady: () => {
      // A parked (re)connect carries no snapshot, and pre-drop buffered
      // bytes predate an output gap: desync so adopt resyncs into a fresh
      // visible attach — one snapshot for the terminal actually shown,
      // instead of one per parked socket at reconnect time.
      if (!entry.disposed) entry.buf.desync();
    },
    onDrop: () => {
      // The output gap starts at the drop, so desync NOW: an adopt that
      // races the reconnect handshake must resync, never flush pre-gap
      // bytes into a visible grid. No-op for a visible entry (its own
      // reconnect ready repaints it).
      if (!entry.disposed) entry.buf.desync();
    },
  });

  term.onData((data) => {
    // Send first: the ghost's DOM work (layout read + style writes) must
    // never sit between the keystroke and the wire — both run in the same
    // task, so the prediction still paints in the same frame.
    entry.socket.sendInput(data);
    entry.echo.onInput(data);
  });
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
  entry.disposed = true;
  pool.delete(entry.id);
  if (entry.fitTimer !== null) clearTimeout(entry.fitTimer);
  entry.echo.dispose();
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

/**
 * Move any `<style>` xterm planted inside the terminal element up to
 * `<head>`. xterm 6 keeps its scrollbar theme sheet in `.xterm-screen` (and
 * the DOM renderer, when WebGL is unavailable, its theme + dimension sheets),
 * so re-parenting the element between a pane and the stash removed and
 * re-inserted a stylesheet on every tab switch. WebKit answers a stylesheet
 * change by rebuilding the document's style resolver and re-resolving EVERY
 * element — a quarter-second per move behind a long transcript, twice per
 * switch, and a multi-second freeze in a busy workspace. The node itself
 * moves (xterm keeps its reference: theme updates and dispose still reach
 * it); its selectors were document-global already.
 */
function hoistStyles(el: HTMLElement): void {
  for (const st of Array.from(el.getElementsByTagName("style"))) document.head.appendChild(st);
}

function attach(id: string, host: HTMLElement, fontOverride: number | undefined): void {
  ensureStash();
  let entry = pool.get(id);
  if (entry === undefined) {
    entry = createEntry(id, host, fontOverride);
  } else {
    if (entry.el.parentElement !== host) {
      hoistStyles(entry.el);
      host.appendChild(entry.el);
    }
    // Now visible: replay what parking deferred (or resync after a discard)
    // — synchronously, before any live write can land — and give a WebGL
    // renderer lost to a context loss a shot at coming back.
    adoptParked(entry);
    loadWebgl(entry);
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
    // Metrics-affecting options (font, line height) change the cell grid —
    // a grid discontinuity for any pending ghost's pixel coordinates.
    e.echo.clear();
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
    // Park is the explicit lifecycle signal that flips output into the
    // ParkedBuffer — set before the move so no write races the stash.
    entry.buf.park();
    entry.echo.clear();
    // Tell the server too: it stops forwarding output entirely (its ring
    // buffers the stream), so a hidden terminal costs the wire ~nothing —
    // the difference between local and tunneled remotes. An old server
    // ignores the frame; the ParkedBuffer above still absorbs its stream.
    entry.socket.sendPark();
    hoistStyles(entry.el);
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
