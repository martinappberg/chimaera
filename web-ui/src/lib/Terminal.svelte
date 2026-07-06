<script lang="ts">
  import { onMount } from "svelte";
  import { Terminal } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import { WebglAddon } from "@xterm/addon-webgl";
  import "@xterm/xterm/css/xterm.css";
  import { SessionSocket } from "./ws";

  interface Props {
    /** Session shown in the main area; others stay warm but hidden. */
    activeId: string | null;
    /** All known session ids; pool entries for vanished ids are disposed. */
    sessionIds: string[];
    onTitle: (id: string, title: string) => void;
    onExited: (id: string, status: number | null) => void;
  }

  let { activeId, sessionIds, onTitle, onExited }: Props = $props();

  const POOL_CAP = 8;
  const FONT_FAMILY =
    'ui-monospace, "SF Mono", SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace';

  interface PoolEntry {
    id: string;
    term: Terminal;
    fit: FitAddon;
    socket: SessionSocket;
    el: HTMLDivElement;
    ro: ResizeObserver;
    lastUsed: number;
  }

  let host = $state<HTMLDivElement | null>(null);

  // Plain non-reactive pool: xterm instances must never be wrapped in $state.
  const pool = new Map<string, PoolEntry>();
  let clock = 0;

  function themeFromTokens() {
    const cs = getComputedStyle(document.documentElement);
    const v = (name: string) => cs.getPropertyValue(name).trim();
    return {
      background: v("--bg"),
      foreground: v("--fg"),
      cursor: v("--fg"),
      cursorAccent: v("--bg"),
      selectionBackground: v("--term-selection"),
    };
  }

  function fitEntry(entry: PoolEntry): void {
    if (entry.el.style.display === "none") return;
    // Never resize to degenerate dimensions (hidden or mid-layout element):
    // a tiny resize destroys buffer content client- and server-side.
    const dims = entry.fit.proposeDimensions();
    if (!dims || !isFinite(dims.cols) || !isFinite(dims.rows) || dims.cols < 2 || dims.rows < 2) {
      return;
    }
    // fit() resizes the terminal; term.onResize then sends the resize frame.
    entry.fit.fit();
  }

  function createEntry(id: string, parent: HTMLDivElement): PoolEntry {
    const el = document.createElement("div");
    el.className = "term-slot";
    // The element must be visible and laid out BEFORE term.open(): opening in
    // a display:none element leaves xterm unmeasured, and the attach snapshot
    // written into that state is lost. Entries are only created on
    // activation, so visible-first is correct; the activation effect hides
    // the others afterwards.
    parent.appendChild(el);

    const term = new Terminal({
      fontFamily: FONT_FAMILY,
      fontSize: 13,
      scrollback: 5000,
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

    const entryForFit: Pick<PoolEntry, "el" | "fit"> = { el, fit };
    fitEntry(entryForFit as PoolEntry);

    // Connect only after the terminal is open, visible, and fitted, so the
    // snapshot frame lands in a fully initialized terminal.
    const socket = new SessionSocket(id, {
      onBinary: (data) => term.write(data),
      onReset: () => term.reset(),
      onTitle: (title) => onTitle(id, title),
      onResized: (cols, rows) => {
        if (term.cols !== cols || term.rows !== rows) term.resize(cols, rows);
      },
      onExited: (status) => {
        term.write("\r\n\x1b[2m[exited]\x1b[0m\r\n");
        onExited(id, status);
      },
      onError: (message) => {
        term.write(`\r\n\x1b[2m[${message}]\x1b[0m\r\n`);
      },
    });

    term.onData((data) => socket.sendInput(data));
    term.onResize(({ cols, rows }) => socket.sendResize(cols, rows));

    const ro = new ResizeObserver(() => {
      if (el.style.display !== "none") fit.fit();
    });
    ro.observe(el);

    const entry: PoolEntry = { id, term, fit, socket, el, ro, lastUsed: ++clock };
    pool.set(id, entry);
    return entry;
  }

  function disposeEntry(entry: PoolEntry): void {
    pool.delete(entry.id);
    entry.socket.close();
    entry.ro.disconnect();
    entry.term.dispose();
    entry.el.remove();
  }

  function evictLru(keepId: string): void {
    while (pool.size > POOL_CAP) {
      let oldest: PoolEntry | null = null;
      for (const e of pool.values()) {
        if (e.id !== keepId && (oldest === null || e.lastUsed < oldest.lastUsed)) {
          oldest = e;
        }
      }
      if (oldest === null) break;
      disposeEntry(oldest);
    }
  }

  // Show/create the active session's terminal, hide the rest.
  $effect(() => {
    const id = activeId;
    const parent = host;
    if (parent === null) return;
    let entry = id !== null ? pool.get(id) : undefined;
    if (id !== null && entry === undefined) {
      entry = createEntry(id, parent);
    }
    for (const e of pool.values()) {
      e.el.style.display = e.id === id ? "" : "none";
    }
    if (entry !== undefined) {
      entry.lastUsed = ++clock;
      evictLru(entry.id);
      const active = entry;
      requestAnimationFrame(() => {
        fitEntry(active);
        active.term.focus();
      });
    }
  });

  // Dispose pool entries for sessions that no longer exist.
  $effect(() => {
    const live = new Set(sessionIds);
    for (const entry of [...pool.values()]) {
      if (!live.has(entry.id)) disposeEntry(entry);
    }
  });

  onMount(() => {
    const mq = matchMedia("(prefers-color-scheme: dark)");
    const onSchemeChange = () => {
      const theme = themeFromTokens();
      for (const e of pool.values()) e.term.options.theme = theme;
    };
    mq.addEventListener("change", onSchemeChange);
    return () => {
      mq.removeEventListener("change", onSchemeChange);
      for (const entry of [...pool.values()]) disposeEntry(entry);
    };
  });
</script>

<div class="term-host" bind:this={host}></div>

<style>
  .term-host {
    position: absolute;
    inset: 0;
  }

  .term-host :global(.term-slot) {
    position: absolute;
    inset: 10px 4px 10px 14px;
  }

  /* Let xterm's own viewport fill the slot. */
  .term-host :global(.xterm) {
    height: 100%;
  }
</style>
