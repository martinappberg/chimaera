<script lang="ts">
  /**
   * Program output (`.log`, `slurm-*.out`, `*.stderr`, …) read tail-first:
   * the last 256 KB opens at the bottom, "load earlier" pages back, and the
   * view keeps a bounded window (4 MB) sliding over a file of any size. ANSI
   * colors render in the theme's terminal palette, carriage-return progress
   * bars collapse to their final state, and error / warning lines are marked
   * (with a jump between them).
   *
   * Follow mode polls the tail every 2 s — only while this pane and the
   * window are visible — and keeps the view pinned to the bottom. Scrolling
   * up stops it; scrolling back to the bottom resumes it. With follow off, a
   * disk change still appends new lines without moving the view, and a pill
   * offers the jump.
   *
   * Lines are built as DOM directly (text nodes and classed spans, never
   * HTML): a 256 KB page is thousands of lines, well past what a keyed
   * template should diff. Each page is one block with `content-visibility`
   * so off-screen pages cost no layout.
   */
  import { tick } from "svelte";
  import { pageVisible } from "../shared/visibility";
  import { activeTheme, getSetting } from "../settings/store.svelte";
  import { api, ApiError } from "../net/api";
  import { humanSize, looksBinary } from "./files";
  import { retain, release, type FileEntry } from "./fileStore.svelte";
  import { ansiVars, appendRuns, collapseCarriageReturns, parseAnsi, plainStyle, type AnsiStyle } from "./ansi";
  import { lineLevel, splitLines, wholeLines } from "./logText";
  import { watchPaneVisible } from "./paneVisible";
  import Spinner from "./Spinner.svelte";

  interface Props {
    path: string;
    /** The tail turned out to be binary: the host shows its info card. */
    onBinary?: (size: number) => void;
  }

  let { path, onBinary }: Props = $props();

  /** One page: the first read, "load earlier", "load later". */
  const PAGE = 256 * 1024;
  /** The first read's size: a log this small arrives in one request. */
  const PROBE = 64 * 1024;
  /** Most bytes kept on screen; pages fall off the far end past this. */
  const WINDOW = 4 * 1024 * 1024;
  /** Follow cadence, and the most pages one tick catches up by. */
  const FOLLOW_MS = 2000;
  const CATCH_UP_PAGES = 8;
  /** A poll that hangs past this is abandoned (the next tick retries). */
  const READ_TIMEOUT_MS = 15_000;

  interface Block {
    start: number;
    end: number;
    el: HTMLDivElement;
    errors: number;
    warns: number;
  }

  let scrollEl = $state<HTMLDivElement | null>(null);
  let linesEl = $state<HTMLDivElement | null>(null);
  let partialEl = $state<HTMLDivElement | null>(null);

  /** Rendered pages, in file order; [lo, hi) is always whole lines. */
  let blocks: Block[] = [];
  /** The ANSI style left open at `hi`, for the next appended page. */
  let tailStyle: AnsiStyle = plainStyle();

  let lo = $state(0);
  let hi = $state(0);
  let size = $state(0);
  let errorCount = $state(0);
  let warnCount = $state(0);
  let ready = $state(false);
  let loadError = $state<string | null>(null);
  let busy = $state(false);
  let following = $state(true);
  let newBelow = $state(false);
  let wrap = $state(readWrap());
  let paneVisible = $state(true);
  /** Bytes after `hi` up to EOF with no newline yet (a line being written). */
  let partialLen = $state(0);

  const atTail = $derived(hi + partialLen >= size);

  const fontSize = $derived(getSetting("editor.fontSize"));
  const lineHeight = $derived(getSetting("editor.lineHeight"));
  const palette = $derived(ansiVars(activeTheme().ansi));

  function readWrap(): boolean {
    try {
      return localStorage.getItem("chimaera.logWrap") === "1";
    } catch {
      return false;
    }
  }

  function setWrap(on: boolean): void {
    wrap = on;
    try {
      localStorage.setItem("chimaera.logWrap", on ? "1" : "0");
    } catch {
      // storage unavailable: the choice lasts for this view
    }
  }

  // --- reading ------------------------------------------------------------------

  interface Slice {
    bytes: Uint8Array;
    size: number;
    truncated: boolean;
  }

  async function read(offset: number, limit: number): Promise<Slice> {
    const q = new URLSearchParams({ path, offset: String(offset), limit: String(limit) });
    const res = await api(`/fs/file?${q.toString()}`, { signal: AbortSignal.timeout(READ_TIMEOUT_MS) });
    if (!res.ok) {
      let message = `request failed with status ${res.status}`;
      try {
        const body = (await res.json()) as { error?: string };
        if (body.error) message = body.error;
      } catch {
        // not JSON
      }
      throw new ApiError(res.status, message);
    }
    const bytes = new Uint8Array(await res.arrayBuffer());
    const total = Number(res.headers.get("X-File-Size") ?? offset + bytes.length);
    return {
      bytes,
      size: Number.isFinite(total) ? total : offset + bytes.length,
      truncated: res.headers.get("X-Truncated") === "true",
    };
  }

  /** Every operation that reads and edits the window runs one at a time. */
  let chain: Promise<void> = Promise.resolve();
  function serial(work: () => Promise<void>): Promise<void> {
    const run = async () => {
      busy = true;
      try {
        await work();
        loadError = null;
      } catch (e) {
        loadError = e instanceof Error ? e.message : "failed to read the log";
      } finally {
        busy = false;
      }
    };
    chain = chain.then(run, run);
    return chain;
  }

  // --- rendering ----------------------------------------------------------------

  const decoder = new TextDecoder();

  function renderLines(text: string, style: AnsiStyle, into: HTMLElement): { style: AnsiStyle; errors: number; warns: number } {
    let errors = 0;
    let warns = 0;
    const frag = document.createDocumentFragment();
    for (const raw of splitLines(text)) {
      const parsed = parseAnsi(collapseCarriageReturns(raw), style);
      style = parsed.state;
      const div = document.createElement("div");
      div.className = "ln";
      let plain = "";
      for (const r of parsed.runs) plain += r.text;
      const level = lineLevel(plain);
      if (level === "error") {
        div.classList.add("e");
        errors++;
      } else if (level === "warn") {
        div.classList.add("w");
        warns++;
      }
      appendRuns(div, parsed.runs);
      frag.appendChild(div);
    }
    into.appendChild(frag);
    return { style, errors, warns };
  }

  function makeBlock(bytes: Uint8Array, start: number, style: AnsiStyle): { block: Block; style: AnsiStyle } {
    const el = document.createElement("div");
    el.className = "blk";
    const out = renderLines(decoder.decode(bytes), style, el);
    return {
      block: { start, end: start + bytes.length, el, errors: out.errors, warns: out.warns },
      style: out.style,
    };
  }

  /** Pin a page's placeholder size to its real one, so skipping its layout
   *  off-screen (content-visibility) never shifts the lines being read. */
  function settle(el: HTMLElement): void {
    el.style.contentVisibility = "visible";
    const h = el.getBoundingClientRect().height;
    el.style.containIntrinsicSize = `auto ${Math.ceil(h)}px`;
    el.style.contentVisibility = "";
  }

  function setPartial(bytes: Uint8Array): void {
    const el = partialEl;
    partialLen = bytes.length;
    if (el === null) return;
    el.replaceChildren();
    if (bytes.length > 0) renderLines(decoder.decode(bytes), tailStyle, el);
  }

  function recount(): void {
    lo = blocks[0]?.start ?? hi;
    hi = blocks.length > 0 ? blocks[blocks.length - 1].end : hi;
    errorCount = blocks.reduce((n, b) => n + b.errors, 0);
    warnCount = blocks.reduce((n, b) => n + b.warns, 0);
  }

  function clearAll(at: number): void {
    for (const b of blocks) b.el.remove();
    blocks = [];
    lo = at;
    hi = at;
    tailStyle = plainStyle();
    setPartial(new Uint8Array());
  }

  /** Replace the window with the page ending at EOF. */
  async function loadTail(first?: Slice): Promise<void> {
    let s = first;
    let offset = 0;
    if (s === undefined || s.truncated) {
      const total = s?.size ?? (await read(0, 0)).size;
      offset = Math.max(0, total - PAGE);
      s = await read(offset, PAGE);
      if (s.size > total + PAGE) {
        // Grew by more than a page between the two reads: read the tail again.
        offset = Math.max(0, s.size - PAGE);
        s = await read(offset, PAGE);
      }
    }
    if (looksBinary(s.bytes)) {
      onBinary?.(s.size);
      return;
    }
    size = s.size;
    clearAll(offset);
    appendSlice(s.bytes, offset, true);
    lastPullAt = Date.now();
  }

  /** Add bytes read at `offset` (== hi) to the end of the window. */
  function appendSlice(bytes: Uint8Array, offset: number, fromTailRead = false): void {
    const lines = linesEl;
    if (lines === null) return;
    const reachesEof = offset + bytes.length >= size;
    const { start, end: wholeEnd } = wholeLines(bytes, offset === 0 || !fromTailRead, false);
    // At EOF the unterminated remainder is the line still being written: it
    // shows, but `hi` stays at its start so the next read picks it up whole.
    const end = reachesEof ? bytes.lastIndexOf(0x0a) + 1 : wholeEnd;
    if (fromTailRead) {
      lo = offset + start;
      hi = lo;
    }
    if (end > start) {
      const made = makeBlock(bytes.subarray(start, end), offset + start, tailStyle);
      tailStyle = made.style;
      lines.appendChild(made.block.el);
      settle(made.block.el);
      blocks.push(made.block);
      hi = made.block.end;
    }
    setPartial(reachesEof ? bytes.subarray(Math.max(end, start)) : new Uint8Array());
    // Past the window budget, drop pages off the top.
    while (blocks.length > 1 && hi - blocks[0].start > WINDOW) {
      const gone = blocks.shift();
      gone?.el.remove();
    }
    recount();
  }

  async function loadEarlier(): Promise<void> {
    const lines = linesEl;
    const sc = scrollEl;
    if (lines === null || sc === null || lo === 0) return;
    const from = Math.max(0, lo - PAGE);
    const s = await read(from, lo - from);
    size = s.size;
    const { start } = wholeLines(s.bytes, from === 0, true);
    const made = makeBlock(s.bytes.subarray(start), from + start, plainStyle());
    // Keep the lines the reader is looking at where they are (the page and
    // any change to the button above them both land above the view).
    const before = sc.scrollHeight;
    lines.insertBefore(made.block.el, lines.firstChild);
    settle(made.block.el);
    blocks.unshift(made.block);
    // Past the budget, drop pages off the bottom: the view leaves the tail.
    while (blocks.length > 1 && blocks[blocks.length - 1].end - made.block.start > WINDOW) {
      const gone = blocks.pop();
      gone?.el.remove();
      setPartial(new Uint8Array());
      following = false;
    }
    hi = blocks[blocks.length - 1].end;
    recount();
    await tick();
    sc.scrollTop += sc.scrollHeight - before;
    lastSetTop = sc.scrollTop;
  }

  /** When the tail was last read (a fresh open needs no immediate poll). */
  let lastPullAt = 0;

  /** Append whatever the file gained past `hi`, up to `maxPages` reads. */
  async function pullNew(maxPages: number): Promise<void> {
    lastPullAt = Date.now();
    for (let i = 0; i < maxPages; i++) {
      const s = await read(hi, PAGE);
      if (s.size < hi) {
        // Truncated or replaced (a rerun overwrote the log): start over.
        await loadTail();
        return;
      }
      size = s.size;
      if (s.size - hi > WINDOW) {
        // Too far behind to page through: jump to the new tail.
        await loadTail();
        return;
      }
      appendSlice(s.bytes, hi);
      if (!s.truncated) return;
    }
  }

  async function loadLater(): Promise<void> {
    await pullNew(1);
  }

  async function jumpTop(): Promise<void> {
    following = false;
    if (lo > 0) {
      const s = await read(0, PAGE);
      size = s.size;
      clearAll(0);
      appendSlice(s.bytes, 0);
    }
    scrollTo(0);
  }

  async function jumpBottom(): Promise<void> {
    newBelow = false;
    if (!atTail) await loadTail();
    following = true;
    scrollTo(Infinity);
  }

  // --- scrolling ------------------------------------------------------------------

  /** The last scrollTop this view set itself, to tell its own scrolls from
   *  the reader's. */
  let lastSetTop = -1;

  function scrollTo(top: number): void {
    const sc = scrollEl;
    if (sc === null) return;
    sc.scrollTop = top === Infinity ? sc.scrollHeight : top;
    lastSetTop = sc.scrollTop;
  }

  function onScroll(): void {
    const sc = scrollEl;
    if (sc === null) return;
    if (Math.abs(sc.scrollTop - lastSetTop) < 1) return;
    const atBottom = sc.scrollHeight - sc.scrollTop - sc.clientHeight < 24;
    if (atBottom && atTail) {
      newBelow = false;
      following = true;
    } else if (!atBottom) {
      following = false;
    }
  }

  function afterAppend(wasFollowing: boolean): void {
    if (wasFollowing) scrollTo(Infinity);
  }

  // --- lifecycle ------------------------------------------------------------------

  let entry = $state<FileEntry | null>(null);
  $effect(() => {
    const e = retain(path);
    entry = e;
    void e.ensureMtime();
    return () => release(path);
  });

  // Open: one small read; a log that fits arrives whole, a bigger one
  // costs a second read for its tail.
  $effect(() => {
    if (linesEl === null || partialEl === null) return;
    void serial(async () => {
      const first = await read(0, PROBE);
      if (looksBinary(first.bytes)) {
        onBinary?.(first.size);
        return;
      }
      await loadTail(first);
      ready = true;
      requestAnimationFrame(() => scrollTo(Infinity));
    });
    return () => {
      for (const b of blocks) b.el.remove();
      blocks = [];
    };
  });

  // Follow: poll the tail while it is on and anyone can see it. Re-running
  // on a visibility return is the catch-up.
  $effect(() => {
    if (!ready || !following || !paneVisible || !$pageVisible) return;
    const pull = () =>
      void serial(async () => {
        if (!following) return;
        await pullNew(CATCH_UP_PAGES);
        afterAppend(following);
      });
    if (Date.now() - lastPullAt >= FOLLOW_MS) pull();
    const t = setInterval(pull, FOLLOW_MS);
    return () => clearInterval(t);
  });

  // Not following: a disk change (the store's watch) still brings the new
  // lines in, without moving the view.
  $effect(() => {
    const m = entry?.mtime;
    if (m === undefined || m === null || !ready || following) return;
    void serial(async () => {
      if (following || !atTail) return;
      const before = hi + partialLen;
      await pullNew(1);
      if (hi + partialLen > before) newBelow = true;
    });
  });

  $effect(() => {
    const el = scrollEl;
    if (el === null) return;
    return watchPaneVisible(el, (v) => (paneVisible = v));
  });

  /** Scroll to the next marked line below (or above) the top of the view. */
  function jumpMark(kind: "e" | "w", dir: 1 | -1): void {
    const sc = scrollEl;
    const lines = linesEl;
    if (sc === null || lines === null) return;
    const marks = [...lines.querySelectorAll<HTMLElement>(`.ln.${kind}`)];
    if (marks.length === 0) return;
    const top = sc.getBoundingClientRect().top;
    const probe = top + 28;
    let target: HTMLElement | undefined;
    if (dir > 0) target = marks.find((m) => m.getBoundingClientRect().top > probe + 1) ?? marks[0];
    else target = [...marks].reverse().find((m) => m.getBoundingClientRect().top < probe - 1) ?? marks[marks.length - 1];
    following = false;
    const y = target.getBoundingClientRect().top - top + sc.scrollTop - 28;
    scrollTo(Math.max(0, y));
    target.classList.remove("flash");
    void target.offsetWidth;
    target.classList.add("flash");
  }

  const windowLabel = $derived.by(() => {
    const shown = hi + partialLen - lo;
    if (lo === 0 && atTail) return `${humanSize(size)}`;
    if (atTail) return `last ${humanSize(shown)} of ${humanSize(size)}`;
    return `${humanSize(shown)} of ${humanSize(size)}`;
  });
</script>

<div class="log-view" style={palette}>
  <div class="log-bar">
    <span class="range" title="{lo.toLocaleString()}–{(hi + partialLen).toLocaleString()} of {size.toLocaleString()} bytes">
      {ready ? windowLabel : ""}
    </span>
    {#if errorCount > 0}
      <button
        class="chip e"
        onclick={(ev) => jumpMark("e", ev.shiftKey ? -1 : 1)}
        title="jump to the next error line (shift-click: the previous one)"
        >{errorCount} {errorCount === 1 ? "error" : "errors"}</button
      >
    {/if}
    {#if warnCount > 0}
      <button
        class="chip w"
        onclick={(ev) => jumpMark("w", ev.shiftKey ? -1 : 1)}
        title="jump to the next warning line (shift-click: the previous one)"
        >{warnCount} {warnCount === 1 ? "warning" : "warnings"}</button
      >
    {/if}
    {#if loadError !== null}<span class="bar-err" title={loadError}>{loadError}</span>{/if}
    <span class="spacer"></span>
    <button class="bbtn" class:on={wrap} onclick={() => setWrap(!wrap)} title="wrap long lines">wrap</button>
    <button class="bbtn" onclick={() => void serial(jumpTop)} title="jump to the start of the file">top</button>
    <button class="bbtn" onclick={() => void serial(jumpBottom)} title="jump to the end of the file">bottom</button>
    <button
      class="bbtn follow"
      class:on={following}
      onclick={() => (following ? (following = false) : void serial(jumpBottom))}
      title={following ? "following the end of the file — click or scroll up to stop" : "follow the end of the file"}
    >
      <span class="dot" aria-hidden="true"></span>{following ? "following" : "follow"}
    </button>
  </div>

  <div class="log-body">
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div
      class="log-scroll"
      class:wrap
      bind:this={scrollEl}
      onscroll={onScroll}
      tabindex="0"
      role="log"
      aria-label="log output"
      style:font-size="{fontSize}px"
      style:line-height={lineHeight}
    >
      {#if ready && lo > 0}
        <button class="more" disabled={busy} onclick={() => void serial(loadEarlier)}>
          load earlier · {humanSize(Math.min(lo, PAGE))}
        </button>
      {/if}
      <div class="lines" bind:this={linesEl}></div>
      <div class="lines partial" bind:this={partialEl}></div>
      {#if ready && !atTail && !following}
        <button class="more" disabled={busy} onclick={() => void serial(loadLater)}>load later</button>
      {/if}
      {#if ready && lo === 0 && hi === 0 && partialLen === 0}
        <div class="empty">empty file</div>
      {/if}
    </div>
    {#if !ready && loadError === null}
      <Spinner />
    {:else if !ready && loadError !== null}
      <div class="log-error">{loadError}</div>
    {/if}
    {#if newBelow && !following}
      <button class="pill" onclick={() => void serial(jumpBottom)}>new output ↓</button>
    {/if}
  </div>
</div>

<style>
  .log-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  .log-bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.35rem;
    height: 26px;
    padding: 0 0.5rem 0 0.7rem;
    border-bottom: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
    min-width: 0;
  }

  .range {
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    margin-right: 0.3rem;
  }

  .spacer {
    flex: 1;
  }

  .bar-err {
    color: var(--err);
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .chip {
    appearance: none;
    font: inherit;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    padding: 0 0.45rem;
    border-radius: 999px;
    cursor: pointer;
    white-space: nowrap;
    line-height: 1.5;
    border: 1px solid;
    background: transparent;
    transition: background-color 0.12s ease;
  }

  .chip.e {
    color: var(--err);
    border-color: color-mix(in srgb, var(--err) 35%, transparent);
  }

  .chip.w {
    color: var(--warn);
    border-color: color-mix(in srgb, var(--warn) 35%, transparent);
  }

  .chip.e:hover {
    background: color-mix(in srgb, var(--err) 10%, transparent);
  }

  .chip.w:hover {
    background: color-mix(in srgb, var(--warn) 10%, transparent);
  }

  .bbtn {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    padding: 0.1rem 0.45rem;
    border-radius: 4px;
    white-space: nowrap;
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .bbtn:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .bbtn.on {
    color: var(--fg);
  }

  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    border: 1px solid currentColor;
    opacity: 0.7;
  }

  .follow.on .dot {
    border-color: var(--accent);
    background: var(--accent);
    opacity: 1;
  }

  .log-body {
    position: relative;
    flex: 1;
    min-height: 0;
  }

  .log-scroll {
    position: absolute;
    inset: 0;
    overflow: auto;
    overflow-anchor: none;
    scrollbar-width: thin;
    padding: 6px 0 10px;
    font-family: var(--editor-font);
    color: var(--fg);
    outline: none;
    tab-size: 8;
  }

  .lines {
    min-width: max-content;
  }

  .wrap .lines {
    min-width: 0;
  }

  /* One page per block; the browser skips layout for pages out of view. */
  .lines :global(.blk) {
    content-visibility: auto;
    contain-intrinsic-size: auto 2000px;
  }

  .lines :global(.ln) {
    padding: 0 14px;
    min-height: 1lh;
    white-space: pre;
  }

  .wrap .lines :global(.ln) {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .lines :global(.ln.e) {
    background: color-mix(in srgb, var(--err) 9%, transparent);
    box-shadow: inset 2px 0 0 color-mix(in srgb, var(--err) 75%, transparent);
  }

  .lines :global(.ln.w) {
    background: color-mix(in srgb, var(--warn) 8%, transparent);
    box-shadow: inset 2px 0 0 color-mix(in srgb, var(--warn) 70%, transparent);
  }

  .lines :global(.ln.flash) {
    animation: log-flash 1.4s ease-out;
  }

  @keyframes log-flash {
    0%,
    25% {
      background-color: color-mix(in srgb, var(--accent) 24%, transparent);
    }
  }

  /* ANSI runs, in the theme's terminal palette (ansiVars on the root). */
  .lines :global(.af0) { color: var(--ansi-0); }
  .lines :global(.af1) { color: var(--ansi-1); }
  .lines :global(.af2) { color: var(--ansi-2); }
  .lines :global(.af3) { color: var(--ansi-3); }
  .lines :global(.af4) { color: var(--ansi-4); }
  .lines :global(.af5) { color: var(--ansi-5); }
  .lines :global(.af6) { color: var(--ansi-6); }
  .lines :global(.af7) { color: var(--ansi-7); }
  .lines :global(.af8) { color: var(--ansi-8); }
  .lines :global(.af9) { color: var(--ansi-9); }
  .lines :global(.af10) { color: var(--ansi-10); }
  .lines :global(.af11) { color: var(--ansi-11); }
  .lines :global(.af12) { color: var(--ansi-12); }
  .lines :global(.af13) { color: var(--ansi-13); }
  .lines :global(.af14) { color: var(--ansi-14); }
  .lines :global(.af15) { color: var(--ansi-15); }
  .lines :global(.ab0) { background: var(--ansi-0); }
  .lines :global(.ab1) { background: var(--ansi-1); }
  .lines :global(.ab2) { background: var(--ansi-2); }
  .lines :global(.ab3) { background: var(--ansi-3); }
  .lines :global(.ab4) { background: var(--ansi-4); }
  .lines :global(.ab5) { background: var(--ansi-5); }
  .lines :global(.ab6) { background: var(--ansi-6); }
  .lines :global(.ab7) { background: var(--ansi-7); }
  .lines :global(.ab8) { background: var(--ansi-8); }
  .lines :global(.ab9) { background: var(--ansi-9); }
  .lines :global(.ab10) { background: var(--ansi-10); }
  .lines :global(.ab11) { background: var(--ansi-11); }
  .lines :global(.ab12) { background: var(--ansi-12); }
  .lines :global(.ab13) { background: var(--ansi-13); }
  .lines :global(.ab14) { background: var(--ansi-14); }
  .lines :global(.ab15) { background: var(--ansi-15); }
  .lines :global(.affg) { color: var(--fg); }
  .lines :global(.afbg) { color: var(--term-bg); }
  .lines :global(.abfg) { background: var(--fg); }
  .lines :global(.abbg) { background: var(--term-bg); }
  .lines :global(.a-b) { font-weight: 600; }
  .lines :global(.a-d) { opacity: 0.65; }
  .lines :global(.a-i) { font-style: italic; }
  .lines :global(.a-u) { text-decoration: underline; }

  .more {
    display: block;
    margin: 4px 14px 8px;
    appearance: none;
    border: 1px dashed var(--edge);
    border-radius: 6px;
    background: transparent;
    color: var(--muted);
    font: inherit;
    font-family: var(--ui-font);
    font-size: var(--text-xs);
    padding: 0.25rem 0.8rem;
    cursor: pointer;
  }

  .more:hover:not(:disabled) {
    color: var(--fg);
    border-color: color-mix(in srgb, var(--fg) 30%, var(--edge));
  }

  .more:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .empty,
  .log-error {
    color: var(--muted);
    font-family: var(--ui-font);
    font-size: var(--text-md);
    padding: 2rem 1rem;
    text-align: center;
  }

  .log-error {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .pill {
    position: absolute;
    left: 50%;
    bottom: 14px;
    transform: translateX(-50%);
    appearance: none;
    border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--edge));
    border-radius: 999px;
    background: var(--overlay-bg);
    color: var(--accent);
    font: inherit;
    font-size: var(--text-xs);
    padding: 0.25rem 0.8rem;
    cursor: pointer;
    box-shadow: 0 3px 12px color-mix(in srgb, var(--fg) 14%, transparent);
  }

  @media (prefers-reduced-motion: reduce) {
    .bbtn,
    .chip {
      transition: none;
    }
    .lines :global(.ln.flash) {
      animation: none;
    }
  }
</style>
