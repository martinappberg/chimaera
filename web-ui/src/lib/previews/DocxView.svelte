<script lang="ts">
  /**
   * A Word document (.docx) drawn in the browser by docx-preview: pages at
   * their set size on the workbench's desk, zoom steps, fit width, a page
   * indicator, and the browser's own find (the text is real text). Read-only.
   *
   * The document is untrusted: it renders into detached nodes, which
   * `officeSafety.sanitizeRendered` strips of anything that could load or run
   * (remote images, CSS urls smuggled through font names, alt-chunk HTML,
   * handlers) before they are attached — inside a shadow root, so the
   * document's stylesheet can't restyle the app. Links route through the
   * app: `#bookmarks` scroll, web links open like any other, nothing else
   * navigates. A disk change re-renders in place, keeping the reading spot.
   */
  import { untrack } from "svelte";
  import { basename, fsDownload } from "./files";
  import { retain, release, type FileEntry } from "./fileStore.svelte";
  import { fetchRawBytes, TooLargeError } from "./rawBytes";
  import { sanitizeRendered } from "./officeSafety";
  import { activateUrl } from "../shared/urlOpen";
  import { isRemoteHost } from "../net/api";
  import Spinner from "./Spinner.svelte";

  interface Props {
    path: string;
  }

  let { path }: Props = $props();

  /** Largest document opened (the whole file is parsed in the page). */
  const MAX_DOCX_BYTES = 50 * 1024 * 1024;
  const ZOOMS = [0.5, 0.67, 0.75, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2, 2.5, 3];
  /** Horizontal room kept around the page when fitting its width (px). */
  const FIT_GUTTER = 40;

  interface Rendered {
    nodes: Node[];
    blocked: number;
  }

  let entry = $state<FileEntry | null>(null);
  $effect(() => {
    const e = retain(path);
    entry = e;
    void e.ensureMtime();
    return () => release(path);
  });

  let rendered = $state.raw<Rendered | null>(null);
  let error = $state<string | null>(null);
  let tooLarge = $state(false);

  let docx: Promise<typeof import("docx-preview")> | null = null;
  async function build(p: string): Promise<Rendered> {
    const bytes = await fetchRawBytes(p, MAX_DOCX_BYTES);
    docx ??= import("docx-preview");
    const lib = await docx;
    let nodes: Node[];
    try {
      const doc = await lib.parseAsync(bytes, OPTIONS);
      nodes = await lib.renderDocument(doc, OPTIONS);
    } catch {
      throw new Error("this file couldn't be read as a Word document (legacy .doc files aren't supported)");
    }
    const blocked = sanitizeRendered(nodes);
    unsymbolBullets(nodes);
    return { nodes, blocked };
  }

  /** Word's default bullets are private-use characters in the Symbol and
   *  Wingdings fonts, which most systems (and every Linux) lack: the list
   *  would show no bullets at all. Draw them as the Unicode characters they
   *  stand for, in the paragraph's own font. */
  const SYMBOL_BULLETS: Record<string, string> = {
    "": "•",
    "": "▪",
    "": "➢",
    "": "❖",
    "": "✔",
    "": "□",
    "": "■",
  };
  function unsymbolBullets(nodes: Node[]): void {
    for (const n of nodes) {
      if (!(n instanceof HTMLStyleElement)) continue;
      const css = n.textContent ?? "";
      if (!/[-]/.test(css)) continue;
      n.textContent = css.replace(/(::?before\s*\{[^}]*)/g, (rule) => {
        if (!/[-]/.test(rule)) return rule;
        return rule
          .replace(/[-]/g, (c) => SYMBOL_BULLETS[c] ?? "•")
          .replace(/font-family:\s*(?:"?(?:Symbol|Wingdings[^;"]*)"?)\s*;?/gi, "");
      });
    }
  }

  const OPTIONS = {
    className: "docx",
    inWrapper: true,
    breakPages: true,
    // Word records where its own layout broke pages; honoring them gives the
    // page count a reader expects.
    ignoreLastRenderedPageBreak: false,
    experimental: false,
    renderHeaders: true,
    renderFooters: true,
    renderFootnotes: true,
    renderEndnotes: true,
    renderComments: false,
    renderChanges: false,
    // An alt chunk is arbitrary HTML the library would put in an iframe.
    renderAltChunks: false,
    useBase64URL: false,
  };

  // (Re)render on open and on every on-disk version; the last render stays
  // up until the next is ready.
  let seen: string | null | undefined;
  let gen = 0;
  $effect(() => {
    const m = entry?.mtime ?? null;
    if (seen !== undefined && (m === null || m === seen || seen === null)) {
      if (m !== null) seen = m;
      return;
    }
    seen = m;
    const mine = ++gen;
    void build(path).then(
      (r) => {
        if (mine !== gen) return;
        rendered = r;
        error = null;
        tooLarge = false;
      },
      (e: unknown) => {
        if (mine !== gen) return;
        tooLarge = e instanceof TooLargeError;
        error = e instanceof Error ? e.message : "the document could not be opened";
      },
    );
  });

  // --- the page desk ------------------------------------------------------------

  let scroller = $state<HTMLDivElement | null>(null);
  let hostEl = $state<HTMLDivElement | null>(null);
  let shadow: ShadowRoot | null = null;
  let content: HTMLDivElement | null = null;
  let viewW = $state(0);
  /** The widest page at 100%, measured after each render. */
  let pageW = $state(0);
  let pageCount = $state(0);
  let current = $state(0);
  /** "auto" = fit the width but never past 100%; "width" = fit the width. */
  let zoom = $state<"auto" | "width" | number>("auto");

  const fitWidth = $derived(pageW > 0 && viewW > 0 ? Math.max(0.25, (viewW - FIT_GUTTER) / pageW) : 1);
  const scale = $derived(zoom === "auto" ? Math.min(1, fitWidth) : zoom === "width" ? Math.min(3, fitWidth) : zoom);

  $effect(() => {
    const el = scroller;
    if (el === null) return;
    const ro = new ResizeObserver(() => (viewW = el.clientWidth));
    ro.observe(el);
    viewW = el.clientWidth;
    return () => ro.disconnect();
  });

  // Attach the rendered nodes into the shadow root (once per render),
  // keeping the reader's place across a re-render.
  $effect(() => {
    const host = hostEl;
    const r = rendered;
    if (host === null || r === null) return;
    untrack(() => {
      if (shadow === null) {
        shadow = host.attachShadow({ mode: "open" });
        const style = document.createElement("style");
        style.textContent = DESK_CSS;
        content = document.createElement("div");
        content.className = "desk";
        shadow.append(style, content);
        shadow.addEventListener("click", onClick);
      }
      const sc = scroller;
      const ratio = sc !== null && sc.scrollHeight > sc.clientHeight ? sc.scrollTop / sc.scrollHeight : 0;
      content!.replaceChildren(...r.nodes);
      // Zoom goes on the page column, not the desk: the desk stays the
      // pane's width, so a zoomed-out column stays centered in it.
      zoomEl = content!.querySelector<HTMLElement>(".docx-wrapper") ?? content;
      const sections = pages();
      pageCount = sections.length;
      pageW = Math.max(0, ...sections.map((s) => s.getBoundingClientRect().width));
      zoomEl!.style.zoom = String(scale);
      if (sc !== null) requestAnimationFrame(() => (sc.scrollTop = ratio * sc.scrollHeight));
      updateCurrent();
    });
  });

  let zoomEl: HTMLElement | null = null;

  // Zoom keeps the point at the top of the view where it was.
  $effect(() => {
    const z = scale;
    const el = zoomEl;
    const sc = untrack(() => scroller);
    if (el === null || untrack(() => rendered) === null) return;
    const prev = Number(el.style.zoom || "1");
    if (prev === z) return;
    const top = sc?.scrollTop ?? 0;
    el.style.zoom = String(z);
    if (sc !== null) sc.scrollTop = (top * z) / prev;
    updateCurrent();
  });

  function pages(): HTMLElement[] {
    return content === null ? [] : Array.from(content.querySelectorAll<HTMLElement>(".docx-wrapper > section.docx"));
  }

  /** The page under a line a third of the way down the view. */
  function updateCurrent(): void {
    const sc = scroller;
    if (sc === null) return;
    const line = sc.getBoundingClientRect().top + sc.clientHeight / 3;
    const all = pages();
    let lo = 0;
    let hi = all.length - 1;
    let found = 0;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (all[mid].getBoundingClientRect().top <= line) {
        found = mid;
        lo = mid + 1;
      } else hi = mid - 1;
    }
    current = found;
  }

  let frame = 0;
  function onScroll(): void {
    if (frame !== 0) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      updateCurrent();
    });
  }
  $effect(() => () => cancelAnimationFrame(frame));

  function step(dir: 1 | -1): void {
    const cur = scale;
    const next =
      dir > 0 ? ZOOMS.find((z) => z > cur + 0.01) : [...ZOOMS].reverse().find((z) => z < cur - 0.01);
    zoom = next ?? (dir > 0 ? ZOOMS[ZOOMS.length - 1] : ZOOMS[0]);
  }

  function onClick(e: Event): void {
    const a = e
      .composedPath()
      .find((n): n is HTMLAnchorElement => n instanceof HTMLAnchorElement);
    if (a === undefined) return;
    const href = a.getAttribute("href") ?? "";
    const me = e as MouseEvent;
    if (href.startsWith("#")) {
      e.preventDefault();
      const id = decodeURIComponent(href.slice(1));
      const target = shadow?.getElementById(id) ?? shadow?.querySelector(`[name="${CSS.escape(id)}"]`);
      target?.scrollIntoView({ block: "start" });
      return;
    }
    if (/^https?:/i.test(href)) {
      e.preventDefault();
      activateUrl(href, me.metaKey || me.ctrlKey);
      return;
    }
    if (!/^mailto:/i.test(href)) e.preventDefault();
  }

  const remote = isRemoteHost();
  let downloadError = $state<string | null>(null);
  async function download(): Promise<void> {
    downloadError = null;
    try {
      await fsDownload(path);
    } catch (e) {
      downloadError = e instanceof Error ? e.message : "download failed";
    }
  }

  function onKey(e: KeyboardEvent): void {
    if (!(e.metaKey || e.ctrlKey) || e.altKey) return;
    if (e.key === "=" || e.key === "+") step(1);
    else if (e.key === "-") step(-1);
    else if (e.key === "0") zoom = "auto";
    else return;
    e.preventDefault();
  }

  // The desk inside the shadow root: theme tokens inherit across the
  // boundary; pages stay paper-white in both themes, as Word draws them.
  const DESK_CSS = `
    :host { display: block; color: #000; color-scheme: light; font: initial; line-height: normal; }
    .desk { display: flex; flex-direction: column; align-items: center; width: max-content; min-width: 100%; }
    .desk .docx-wrapper {
      background: transparent;
      padding: 20px 20px 4px;
    }
    .desk .docx-wrapper > section.docx {
      margin-bottom: 20px;
      box-shadow:
        0 0 0 1px color-mix(in srgb, var(--fg) 10%, transparent),
        0 2px 12px color-mix(in srgb, var(--fg) 14%, transparent);
    }
    .desk a[href] { cursor: pointer; }
  `;
</script>

<div class="docx-view">
  <div class="docx-bar">
    {#if rendered !== null}
      <button class="bbtn icon" onclick={() => step(-1)} title="zoom out (⌘−)" aria-label="zoom out">
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="M3.5 8h9" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" /></svg>
      </button>
      <button class="bbtn zoom" class:on={zoom === "auto"} onclick={() => (zoom = "auto")} title="reset: fit the pane, at most 100% (⌘0)">
        {Math.round(scale * 100)}%
      </button>
      <button class="bbtn icon" onclick={() => step(1)} title="zoom in (⌘+)" aria-label="zoom in">
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="M3.5 8h9M8 3.5v9" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" /></svg>
      </button>
      <button class="bbtn" class:on={zoom === "width"} onclick={() => (zoom = "width")} title="fit the page width to the pane">fit width</button>
      {#if pageCount > 0}
        <span class="pages" title="pages as the document's saved page breaks set them; the layout is the browser's, so lines may break differently than in Word"
          >{current + 1} / {pageCount}</span
        >
      {/if}
      {#if rendered.blocked > 0}
        <span class="note" title="links to images, media or styles outside the file are never fetched">
          {rendered.blocked} external {rendered.blocked === 1 ? "resource" : "resources"} not loaded
        </span>
      {/if}
    {/if}
    {#if downloadError !== null}<span class="bar-err">{downloadError}</span>{/if}
    <span class="spacer"></span>
    {#if remote}
      <button class="bbtn" onclick={() => void download()} title="download {basename(path)} to open it in Word">download</button>
    {/if}
  </div>

  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    class="docx-scroll"
    bind:this={scroller}
    onscroll={onScroll}
    onkeydown={onKey}
    tabindex="0"
    role="document"
    aria-label={basename(path)}
  >
    <div class="host" bind:this={hostEl} class:hidden={rendered === null}></div>
    {#if error !== null && rendered === null}
      <div class="docx-msg">
        <span>{error}</span>
        {#if tooLarge && remote}
          <button class="opt" onclick={() => void download()}>download</button>
        {:else if tooLarge}
          <span class="hint">open it with Word from its folder</span>
        {/if}
      </div>
    {:else if rendered === null}
      <Spinner label="laying out the document" />
    {/if}
    {#if error !== null && rendered !== null}
      <div class="stale-note" role="status">{error} — showing the last good render</div>
    {/if}
  </div>
</div>

<style>
  .docx-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  .docx-bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.3rem;
    height: 26px;
    padding: 0 0.5rem 0 0.4rem;
    border-bottom: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
    min-width: 0;
  }

  .pages {
    margin-left: 0.5rem;
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
    color: var(--fg);
    white-space: nowrap;
  }

  .note,
  .bar-err {
    margin-left: 0.5rem;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .bar-err {
    color: var(--err);
  }

  .spacer {
    flex: 1;
  }

  .bbtn,
  .opt {
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
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .bbtn.icon {
    display: inline-flex;
    align-items: center;
    padding: 0.2rem 0.3rem;
  }

  .bbtn.zoom {
    min-width: 5ch;
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
  }

  .bbtn:hover,
  .opt:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .bbtn.on {
    color: var(--fg);
  }

  .docx-scroll {
    position: relative;
    flex: 1;
    min-height: 0;
    overflow: auto;
    scrollbar-width: thin;
    background: color-mix(in srgb, var(--fg) 4%, var(--term-bg));
    outline: none;
  }

  .docx-scroll:focus-visible {
    box-shadow: inset 0 0 0 1px var(--focus-ring);
  }

  .host.hidden {
    display: none;
  }

  .docx-msg {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.5rem;
    padding: 1rem;
    color: var(--muted);
    font-size: var(--text-md);
    text-align: center;
  }

  .opt {
    border: 1px solid var(--edge);
    padding: 0.2rem 0.7rem;
  }

  .hint {
    font-size: var(--text-xs);
    opacity: 0.85;
  }

  .stale-note {
    position: sticky;
    bottom: 10px;
    margin: 0 auto;
    width: fit-content;
    max-width: calc(100% - 20px);
    padding: 0.25rem 0.6rem;
    border: 1px solid color-mix(in srgb, var(--warn) 40%, var(--edge));
    border-radius: 6px;
    background: var(--overlay-bg);
    color: var(--warn);
    font-size: var(--text-xs);
  }

  @media (prefers-reduced-motion: reduce) {
    .bbtn,
    .opt {
      transition: none;
    }
  }
</style>
