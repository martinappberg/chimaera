<script lang="ts">
  /**
   * A Jupyter notebook, read-only: code cells highlighted with their
   * execution counts, markdown cells rendered (math included), and outputs —
   * stdout/stderr and tracebacks in ANSI color, images inline, SVG as an
   * image (never live markup), HTML in a script-less sandboxed frame.
   *
   * The daemon pages cells (`fs/notebook`) and caps every output, so a
   * 60 MB notebook opens with its first page; later pages load as the
   * reader nears the end. A disk change (re-run, an agent edit) re-reads
   * the loaded cells in place. `#cell=N` scrolls to and flashes a cell.
   *
   * Markdown cells are author content, treated like agent output: marked,
   * then DOMPurify with chat's profile (no style, http(s) links in a new
   * tab without an opener). Math is typeset after sanitizing, through the
   * shared KaTeX policy, loaded only when a cell has an equation.
   */
  import DOMPurify from "dompurify";
  import { Marked } from "marked";
  import type { Parser } from "@lezer/common";
  import { untrack } from "svelte";
  import { rawTicketUrl, resolveDocPath, safeDecodeUri } from "./files";
  import { retain, release, type FileEntry } from "./fileStore.svelte";
  import {
    NOTEBOOK_PAGE_MAX,
    base64Url,
    fsNotebook,
    headingSlug,
    notebookMath,
    pickMime,
    svgUrl,
    type NotebookCell,
    type NotebookOutput,
  } from "./notebook";
  import { parserFor, renderCode } from "./highlight";
  import { ansiVars, appendRuns, collapseCarriageReturns, parseAnsi, plainStyle } from "./ansi";
  import { loadMath } from "./mathLoad";
  import { followDocHref, showLinkHint } from "./docLinks";
  import { revealRequest, takeReveal } from "../shared/reveal";
  import { copyText } from "../shared/clipboard";
  import { activeTheme, getSetting } from "../settings/store.svelte";
  import { humanSize } from "./files";
  import NotebookHtml from "./NotebookHtml.svelte";
  import Spinner from "./Spinner.svelte";

  interface Props {
    path: string;
    /** Active workspace root — `/docs/x.md` links resolve against it. */
    wsRoot?: string | null;
  }

  let { path, wsRoot = null }: Props = $props();

  /** First page (small: the first paint), then pages as the reader scrolls. */
  const FIRST = 24;
  const MORE = 40;

  let cells = $state.raw<NotebookCell[]>([]);
  let total = $state(0);
  let language = $state<string | null>(null);
  let loadError = $state<string | null>(null);
  let loaded = $state(false);
  let loadingMore = $state(false);
  let parser = $state.raw<Parser | null>(null);
  let scrollEl = $state<HTMLDivElement | null>(null);
  let sentinel = $state<HTMLDivElement | null>(null);

  const codeFont = $derived(getSetting("editor.fontSize"));
  const proseFont = $derived(getSetting("editor.markdownFontSize"));
  const palette = $derived(ansiVars(activeTheme().ansi));

  let entry = $state<FileEntry | null>(null);
  $effect(() => {
    const e = retain(path);
    entry = e;
    void e.ensureMtime();
    return () => release(path);
  });

  /** Read cells [0, count) and adopt them unless a newer read started
   *  meanwhile. The daemon may answer a payload-heavy page short: `fill`
   *  keeps asking (re-reading what was loaded, reaching a revealed cell);
   *  the first open takes one page, so it paints after one parse. */
  let generation = 0;
  async function readFirst(count: number, fill = true): Promise<void> {
    const gen = ++generation;
    const got: NotebookCell[] = [];
    let t = 0;
    let lang: string | null = null;
    while (got.length < count) {
      const page = await fsNotebook(path, got.length, Math.min(NOTEBOOK_PAGE_MAX, count - got.length));
      got.push(...page.cells);
      t = page.total;
      lang = page.language;
      if (!fill || page.cells.length === 0 || got.length >= t) break;
    }
    if (gen !== generation) return;
    cells = got;
    total = t;
    language = lang;
    loadError = null;
    loaded = true;
  }

  // Open, and re-read the loaded cells whenever the file changes on disk
  // (keeping the scroll position: cells are keyed by index). The version
  // token arriving for the first time is not a change.
  let seen: string | null | undefined;
  $effect(() => {
    const m = entry?.mtime ?? null;
    if (seen !== undefined && (m === null || m === seen || seen === null)) {
      if (m !== null) seen = m;
      return;
    }
    seen = m;
    const have = untrack(() => cells.length);
    readFirst(Math.max(FIRST, have), have > 0).catch((e: unknown) => {
      loadError = e instanceof Error ? e.message : "failed to read the notebook";
    });
  });

  /** How far below the view the next page starts loading. */
  const AHEAD_PX = 1200;

  async function loadMore(): Promise<void> {
    if (loadingMore || !loaded || cells.length >= total) return;
    loadingMore = true;
    const gen = generation;
    let added = false;
    try {
      const page = await fsNotebook(path, cells.length, MORE);
      if (gen !== generation) return;
      // A page can come back short (the daemon's payload budget); the next
      // one starts where this ended.
      if (page.cells.length > 0 && page.cells[0].index === cells.length) {
        cells = [...cells, ...page.cells];
        added = true;
      }
      total = page.total;
    } catch (e) {
      loadError = e instanceof Error ? e.message : "failed to read more cells";
    } finally {
      loadingMore = false;
    }
    // Short pages can leave the end still in reach: the observer only
    // reports changes, so keep going while it is.
    if (added) requestAnimationFrame(() => {
      if (nearEnd()) void loadMore();
    });
  }

  function nearEnd(): boolean {
    const root = scrollEl;
    const s = sentinel;
    if (root === null || s === null) return false;
    return s.getBoundingClientRect().top - root.getBoundingClientRect().bottom < AHEAD_PX;
  }

  // Page in as the end comes near.
  $effect(() => {
    const root = scrollEl;
    const s = sentinel;
    if (root === null || s === null) return;
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) void loadMore();
      },
      { root, rootMargin: `0px 0px ${AHEAD_PX}px 0px` },
    );
    io.observe(s);
    return () => io.disconnect();
  });

  $effect(() => {
    const lang = language;
    let stale = false;
    void parserFor(lang ?? "python").then((p) => {
      if (!stale) parser = p;
    });
    return () => {
      stale = true;
    };
  });

  // --- #cell=N ------------------------------------------------------------------

  let pendingCell: number | null = null;
  $effect(() => {
    void $revealRequest;
    const req = takeReveal(path);
    if (req?.cell === undefined) return;
    pendingCell = req.cell - 1;
    void revealPending();
  });
  $effect(() => {
    void cells;
    if (pendingCell !== null) void revealPending();
  });

  async function revealPending(): Promise<void> {
    const i = pendingCell;
    if (i === null || !loaded) return;
    if (i >= cells.length && cells.length < total) {
      // Load up to the target first (bounded by the notebook itself).
      await readFirst(Math.min(total, i + MORE));
    }
    pendingCell = null;
    requestAnimationFrame(() => {
      const el = scrollEl?.querySelector<HTMLElement>(`[data-cell="${Math.min(i, cells.length - 1)}"]`);
      if (el === null || el === undefined) return;
      el.scrollIntoView({ block: "start" });
      el.classList.remove("flash");
      void el.offsetWidth;
      el.classList.add("flash");
    });
  }

  // --- markdown -----------------------------------------------------------------

  const marked = new Marked({ gfm: true, breaks: false, async: false });
  marked.use(notebookMath);
  // A private sanitizer instance: its hook (web links → new tab, no opener)
  // never leaks into, or depends on, chat's global one.
  const purify = DOMPurify(window);
  purify.addHook("afterSanitizeAttributes", (node) => {
    if (node instanceof Element && node.tagName === "A" && /^https?:/i.test(node.getAttribute("href") ?? "")) {
      node.setAttribute("target", "_blank");
      node.setAttribute("rel", "noopener noreferrer");
    }
  });

  /** Cell attachments survive the sanitizer as an in-page href: DOMPurify
   *  drops the unknown `attachment:` scheme. */
  const ATTACH = "#nb-attachment=";

  function renderMarkdown(node: HTMLElement, source: string, cell: NotebookCell | null): void {
    const md = source.replace(/\]\(\s*<?attachment:/g, `](${ATTACH}`);
    const frag = purify.sanitize(marked.parse(md) as string, {
      FORBID_TAGS: ["style"],
      FORBID_ATTR: ["style"],
      RETURN_DOM_FRAGMENT: true,
    });
    // Image sources are fixed up while still detached (nothing loads from a
    // fragment), so a relative path never hits the app origin first.
    for (const img of frag.querySelectorAll("img")) {
      const src = img.getAttribute("src") ?? "";
      if (src.startsWith(ATTACH)) {
        const name = decodeURIComponent(src.slice(ATTACH.length).replace(/>$/, ""));
        const att = cell?.attachments?.[name];
        const mime = att === undefined ? null : pickMime(att.data);
        if (att !== undefined && mime !== null && mime.startsWith("image/")) {
          img.setAttribute("src", mime === "image/svg+xml" ? svgUrl(att.data[mime]) : base64Url(mime, att.data[mime]));
        } else img.removeAttribute("src");
      } else if (src !== "" && !/^[a-z][a-z0-9+.-]*:/i.test(src) && !src.startsWith("//")) {
        img.removeAttribute("src");
        const target = resolveDocPath(path, safeDecodeUri(src.split("#")[0]));
        void rawTicketUrl(target).then(
          (url) => img.setAttribute("src", url),
          () => img.setAttribute("alt", `${img.alt || src} (not found)`),
        );
      }
    }
    node.replaceChildren(frag);
    typeset(node);
  }

  function typeset(root: HTMLElement): void {
    const spans = root.querySelectorAll<HTMLElement>("span.nb-math");
    if (spans.length === 0) return;
    void loadMath().then((m) => {
      for (const s of spans) {
        if (!s.isConnected || s.dataset.done === "1") continue;
        s.innerHTML = m.safeMathHtml(s.textContent ?? "", s.dataset.display === "1");
        s.dataset.done = "1";
      }
    });
  }

  function markdown(node: HTMLElement, arg: { source: string; cell: NotebookCell | null }) {
    renderMarkdown(node, arg.source, arg.cell);
    return {
      update(next: { source: string; cell: NotebookCell | null }) {
        renderMarkdown(node, next.source, next.cell);
      },
    };
  }

  /** `text/latex` output: display math (outer `$$` stripped). */
  function latex(node: HTMLElement, src: string) {
    const draw = (s: string) => {
      const body = s.trim().replace(/^\$\$?([\s\S]*?)\$\$?$/, "$1");
      node.textContent = body;
      void loadMath().then((m) => {
        if (node.isConnected) node.innerHTML = m.safeMathHtml(body, true);
      });
    };
    draw(src);
    return { update: draw };
  }

  // --- code and text ------------------------------------------------------------

  function code(node: HTMLElement, arg: { source: string; parser: Parser | null }) {
    renderCode(node, arg.source, arg.parser);
    return {
      update(next: { source: string; parser: Parser | null }) {
        renderCode(node, next.source, next.parser);
      },
    };
  }

  /** Program text with ANSI color, carriage returns resolved per line. */
  function ansi(node: HTMLElement, text: string) {
    const draw = (t: string) => {
      const frag = document.createDocumentFragment();
      let style = plainStyle();
      const lines = t.split("\n");
      lines.forEach((line, i) => {
        const parsed = parseAnsi(collapseCarriageReturns(line), style);
        style = parsed.state;
        appendRuns(frag, parsed.runs);
        if (i < lines.length - 1) frag.appendChild(document.createTextNode("\n"));
      });
      node.replaceChildren(frag);
    };
    draw(text);
    return { update: draw };
  }

  // --- links ----------------------------------------------------------------------

  function onClick(e: MouseEvent): void {
    const anchor = (e.target as Element | null)?.closest?.("a[href]");
    if (anchor === null || anchor === undefined || scrollEl?.contains(anchor) !== true) return;
    const href = anchor.getAttribute("href") ?? "";
    if (/^(mailto|tel):/i.test(href)) return;
    // Never a native navigation: that would replace the workbench.
    e.preventDefault();
    const x = e.clientX;
    const y = e.clientY;
    void followDocHref(href, e.metaKey || e.ctrlKey, {
      docPath: path,
      wsRoot,
      workspaceId: null,
      toAnchor: (anchorId) => scrollToHeading(anchorId),
      toLines: () => {},
      hint: (text) => {
        if (scrollEl !== null) showLinkHint(scrollEl, x, y, text);
      },
    });
  }

  function scrollToHeading(anchorId: string): boolean {
    const want = anchorId.toLowerCase();
    const heads = scrollEl?.querySelectorAll<HTMLElement>(".nb-md :is(h1, h2, h3, h4, h5, h6)") ?? [];
    for (const h of heads) {
      if (headingSlug(h.textContent ?? "").toLowerCase() === want || h.id.toLowerCase() === want) {
        h.scrollIntoView({ block: "start", behavior: "smooth" });
        return true;
      }
    }
    return false;
  }

  // --- small bits -------------------------------------------------------------------

  let copied = $state<number | null>(null);
  let copiedTimer: ReturnType<typeof setTimeout> | null = null;
  function copyCell(cell: NotebookCell): void {
    void copyText(cell.source).then((ok) => {
      if (!ok) return;
      copied = cell.index;
      if (copiedTimer !== null) clearTimeout(copiedTimer);
      copiedTimer = setTimeout(() => (copied = null), 1400);
    });
  }
  $effect(() => () => {
    if (copiedTimer !== null) clearTimeout(copiedTimer);
  });

  /** Collapsed output areas, by cell index (per view, like Jupyter's). */
  let collapsed = $state<Set<number>>(new Set());
  function toggleOutputs(i: number): void {
    const next = new Set(collapsed);
    if (next.has(i)) next.delete(i);
    else next.add(i);
    collapsed = next;
  }

  function omittedNote(out: NotebookOutput): string | null {
    const entries = Object.entries(out.omitted ?? {});
    if (entries.length === 0) return null;
    return entries.map(([mime, bytes]) => `${mime} output not shown — ${humanSize(bytes)}, over the preview cap`).join(" · ");
  }

  const langLabel = $derived(
    language === null ? null : language.charAt(0).toUpperCase() + language.slice(1),
  );
</script>

<div class="nb-view" style={palette}>
  <div class="nb-bar">
    <span class="facts">
      {#if loaded}
        {total} {total === 1 ? "cell" : "cells"}{#if langLabel !== null}<span class="sep">·</span>{langLabel}{/if}
      {/if}
    </span>
    {#if loadError !== null && loaded}<span class="bar-err" title={loadError}>{loadError}</span>{/if}
    <span class="spacer"></span>
    {#if loaded && cells.length < total}
      <span class="progress">{cells.length} of {total} loaded</span>
    {/if}
  </div>

  <!-- Delegated link handling: rendered markdown's own anchors are
       focusable and fire click on Enter, which bubbles here. -->
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div
    class="nb-scroll"
    bind:this={scrollEl}
    onclick={onClick}
    tabindex="0"
    role="region"
    aria-label="notebook"
  >
    {#if !loaded && loadError !== null}
      <div class="nb-error">{loadError}</div>
    {:else if !loaded}
      <Spinner />
    {:else if total === 0}
      <div class="nb-error">this notebook has no cells</div>
    {:else}
      <div class="nb-doc" style:--nb-code-font="{codeFont}px" style:--nb-prose-font="{proseFont}px">
        {#each cells as cell (cell.index)}
          <section class="cell {cell.cell_type}" data-cell={cell.index}>
            {#if cell.cell_type === "code"}
              <div class="gutter" aria-label="execution count">
                [{cell.execution_count ?? " "}]
              </div>
              <div class="body">
                <div class="src-box">
                  <pre class="src" use:code={{ source: cell.source, parser }}></pre>
                  <button
                    class="copy"
                    class:done={copied === cell.index}
                    onclick={() => copyCell(cell)}
                    title="copy cell source"
                    aria-label="copy cell source"
                  >
                    {#if copied === cell.index}
                      <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="M3.5 8.5 6.5 11.5 12.5 4.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
                    {:else}
                      <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><rect x="5.5" y="5.5" width="7.5" height="7.5" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.3" /><path d="M10.5 3.5v-.5A1.5 1.5 0 0 0 9 1.5H4A1.5 1.5 0 0 0 2.5 3v5A1.5 1.5 0 0 0 4 9.5h.5" fill="none" stroke="currentColor" stroke-width="1.3" /></svg>
                    {/if}
                  </button>
                </div>
                {#if cell.truncated}
                  <div class="note">source cut at 1 MB</div>
                {/if}
                {#if (cell.outputs?.length ?? 0) > 0}
                  <div class="outputs" class:collapsed={collapsed.has(cell.index)}>
                    <button
                      class="out-rail"
                      onclick={() => toggleOutputs(cell.index)}
                      title={collapsed.has(cell.index) ? "show outputs" : "collapse outputs"}
                      aria-label={collapsed.has(cell.index) ? "show outputs" : "collapse outputs"}
                      aria-expanded={!collapsed.has(cell.index)}
                    ></button>
                    {#if collapsed.has(cell.index)}
                      <button class="out-folded" onclick={() => toggleOutputs(cell.index)}>
                        {cell.outputs?.length} {cell.outputs?.length === 1 ? "output" : "outputs"} hidden
                      </button>
                    {:else}
                      {#each cell.outputs ?? [] as out, oi (oi)}
                        {@render output(out)}
                      {/each}
                    {/if}
                  </div>
                {/if}
              </div>
            {:else if cell.cell_type === "markdown"}
              <div class="gutter"></div>
              <div class="body">
                <div class="nb-md md-body" use:markdown={{ source: cell.source, cell }}></div>
              </div>
            {:else}
              <div class="gutter"></div>
              <div class="body"><pre class="raw">{cell.source}</pre></div>
            {/if}
          </section>
        {/each}
        <div class="sentinel" bind:this={sentinel}>
          {#if cells.length < total}
            <button class="more" disabled={loadingMore} onclick={() => void loadMore()}>
              {loadingMore ? "loading cells…" : `load more cells (${total - cells.length} left)`}
            </button>
          {/if}
        </div>
      </div>
    {/if}
  </div>
</div>

{#snippet output(out: NotebookOutput)}
  {#if out.output_type === "stream"}
    <pre class="out-text" class:stderr={out.name === "stderr"} use:ansi={out.text ?? ""}></pre>
    {#if out.truncated}<div class="note">output cut at 200 KB</div>{/if}
  {:else if out.output_type === "error"}
    <pre class="out-text out-error" use:ansi={out.traceback || `${out.ename ?? "Error"}: ${out.evalue ?? ""}`}></pre>
    {#if out.truncated}<div class="note">traceback cut at 200 KB</div>{/if}
  {:else if out.data !== undefined}
    {@const mime = pickMime(out.data)}
    {@const payload = mime === null ? "" : out.data[mime]}
    {@const note = omittedNote(out)}
    {#if mime === "image/png" || mime === "image/jpeg" || mime === "image/gif"}
      <img class="out-img" src={base64Url(mime, payload)} alt={out.data["text/plain"] ?? "output image"} />
    {:else if mime === "image/svg+xml"}
      <img class="out-img" src={svgUrl(payload)} alt={out.data["text/plain"] ?? "output image"} />
    {:else if mime === "text/html"}
      <NotebookHtml html={payload} />
    {:else if mime === "text/markdown"}
      <div class="nb-md md-body out-md" use:markdown={{ source: payload, cell: null }}></div>
    {:else if mime === "text/latex"}
      <div class="out-latex" use:latex={payload}></div>
    {:else if mime === "text/plain"}
      <pre class="out-text" use:ansi={payload}></pre>
    {/if}
    {#if note !== null}<div class="note omitted">{note}</div>{/if}
  {:else if out.output_type !== "display_data" && out.output_type !== "execute_result"}
    <div class="note">{out.output_type} output</div>
  {/if}
{/snippet}

<style>
  .nb-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  .nb-bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    height: 26px;
    padding: 0 0.7rem;
    border-bottom: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
    min-width: 0;
  }

  .facts,
  .progress {
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }

  .sep {
    margin: 0 0.4em;
    opacity: 0.6;
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

  .nb-scroll {
    position: relative;
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    overflow-x: hidden;
    scrollbar-width: thin;
    outline: none;
  }

  .nb-doc {
    max-width: 1040px;
    margin: 0 auto;
    padding: 18px 22px 48px 10px;
  }

  .nb-error {
    padding: 3rem 1rem;
    color: var(--muted);
    font-size: var(--text-md);
    text-align: center;
  }

  .cell {
    display: grid;
    grid-template-columns: 6ch minmax(0, 1fr);
    column-gap: 10px;
    margin: 0 0 14px;
    border-radius: 6px;
    scroll-margin-top: 12px;
  }

  .cell.markdown {
    margin-bottom: 10px;
  }

  .cell:global(.flash) {
    animation: nb-flash 1.5s ease-out;
  }

  @keyframes nb-flash {
    0%,
    20% {
      background-color: color-mix(in srgb, var(--accent) 12%, transparent);
    }
  }

  .gutter {
    padding-top: 0.55em;
    text-align: right;
    font-family: var(--editor-font);
    font-size: calc(var(--nb-code-font) - 1px);
    color: var(--muted);
    opacity: 0.8;
    white-space: pre;
    user-select: none;
    font-variant-numeric: tabular-nums;
  }

  .body {
    min-width: 0;
  }

  .src-box {
    position: relative;
  }

  .src,
  .raw {
    margin: 0;
    padding: 0.55em 0.9em;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: color-mix(in srgb, var(--fg) 3.5%, var(--term-bg));
    font-family: var(--editor-font);
    font-size: var(--nb-code-font);
    line-height: 1.5;
    color: var(--fg);
    overflow-x: auto;
    scrollbar-width: thin;
    white-space: pre;
    tab-size: 4;
  }

  .raw {
    color: var(--muted);
    background: transparent;
    border-style: dashed;
  }

  .copy {
    position: absolute;
    top: 5px;
    right: 5px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 4px;
    border: 1px solid var(--edge);
    border-radius: 5px;
    background: color-mix(in srgb, var(--term-bg) 85%, transparent);
    color: var(--muted);
    cursor: pointer;
    opacity: 0;
    transition:
      opacity 0.12s ease,
      color 0.12s ease;
  }

  .src-box:hover .copy,
  .copy:focus-visible,
  .copy.done {
    opacity: 1;
  }

  .copy:hover,
  .copy.done {
    color: var(--accent);
  }

  /* Syntax classes from highlight.ts, on the editor's --syn-* tokens. */
  .src :global(.hl-keyword) { color: var(--syn-keyword); }
  .src :global(.hl-string) { color: var(--syn-string); }
  .src :global(.hl-comment) { color: var(--syn-comment); font-style: italic; }
  .src :global(.hl-number) { color: var(--syn-number); }
  .src :global(.hl-type) { color: var(--syn-type); }
  .src :global(.hl-func) { color: var(--syn-func); }
  .src :global(.hl-def) { color: var(--syn-def); }
  .src :global(.hl-prop) { color: var(--syn-prop); }
  .src :global(.hl-invalid) { color: var(--err); }

  .outputs {
    position: relative;
    margin-top: 6px;
    padding-left: 12px;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 6px;
  }

  .outputs > :global(*) {
    max-width: 100%;
  }

  /* The fold rail: a hairline beside the outputs, a handle on hover. */
  .out-rail {
    position: absolute;
    left: 0;
    top: 2px;
    bottom: 2px;
    width: 8px;
    padding: 0;
    border: 0;
    border-left: 2px solid color-mix(in srgb, var(--fg) 8%, transparent);
    background: none;
    cursor: pointer;
    transition: border-color 0.12s ease;
  }

  .out-rail:hover,
  .out-rail:focus-visible {
    border-left-color: color-mix(in srgb, var(--accent) 70%, transparent);
    outline: none;
  }

  .out-folded {
    appearance: none;
    border: none;
    background: none;
    padding: 0.15rem 0;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
  }

  .out-folded:hover {
    color: var(--fg);
  }

  .out-text {
    margin: 0;
    width: 100%;
    font-family: var(--editor-font);
    font-size: calc(var(--nb-code-font) - 0.5px);
    line-height: 1.45;
    color: var(--fg);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 32em;
    overflow-y: auto;
    scrollbar-width: thin;
  }

  .out-text.stderr {
    padding: 0.35em 0.6em;
    border-radius: 5px;
    background: color-mix(in srgb, var(--err) 7%, transparent);
  }

  .out-error {
    padding: 0.5em 0.7em;
    border-radius: 5px;
    border-left: 2px solid color-mix(in srgb, var(--err) 70%, transparent);
    background: color-mix(in srgb, var(--err) 6%, transparent);
  }

  .out-img {
    display: block;
    max-width: 100%;
    height: auto;
    border-radius: 3px;
  }

  .out-latex {
    overflow-x: auto;
    scrollbar-width: thin;
    padding: 0.2em 0;
  }

  .note {
    font-size: var(--text-xs);
    color: var(--muted);
    font-style: italic;
  }

  .note.omitted {
    padding: 0.4rem 0.6rem;
    border: 1px dashed var(--edge);
    border-radius: 5px;
    font-style: normal;
  }

  .sentinel {
    min-height: 1px;
    padding-left: calc(6ch + 10px);
  }

  .more {
    appearance: none;
    border: 1px dashed var(--edge);
    border-radius: 6px;
    background: transparent;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    padding: 0.3rem 0.9rem;
    cursor: pointer;
  }

  .more:hover:not(:disabled) {
    color: var(--fg);
  }

  /* ANSI runs in the theme's terminal palette (ansiVars on the root). */
  .out-text :global(.af0) { color: var(--ansi-0); }
  .out-text :global(.af1) { color: var(--ansi-1); }
  .out-text :global(.af2) { color: var(--ansi-2); }
  .out-text :global(.af3) { color: var(--ansi-3); }
  .out-text :global(.af4) { color: var(--ansi-4); }
  .out-text :global(.af5) { color: var(--ansi-5); }
  .out-text :global(.af6) { color: var(--ansi-6); }
  .out-text :global(.af7) { color: var(--ansi-7); }
  .out-text :global(.af8) { color: var(--ansi-8); }
  .out-text :global(.af9) { color: var(--ansi-9); }
  .out-text :global(.af10) { color: var(--ansi-10); }
  .out-text :global(.af11) { color: var(--ansi-11); }
  .out-text :global(.af12) { color: var(--ansi-12); }
  .out-text :global(.af13) { color: var(--ansi-13); }
  .out-text :global(.af14) { color: var(--ansi-14); }
  .out-text :global(.af15) { color: var(--ansi-15); }
  .out-text :global(.ab0) { background: var(--ansi-0); }
  .out-text :global(.ab1) { background: var(--ansi-1); }
  .out-text :global(.ab2) { background: var(--ansi-2); }
  .out-text :global(.ab3) { background: var(--ansi-3); }
  .out-text :global(.ab4) { background: var(--ansi-4); }
  .out-text :global(.ab5) { background: var(--ansi-5); }
  .out-text :global(.ab6) { background: var(--ansi-6); }
  .out-text :global(.ab7) { background: var(--ansi-7); }
  .out-text :global(.ab8) { background: var(--ansi-8); }
  .out-text :global(.ab9) { background: var(--ansi-9); }
  .out-text :global(.ab10) { background: var(--ansi-10); }
  .out-text :global(.ab11) { background: var(--ansi-11); }
  .out-text :global(.ab12) { background: var(--ansi-12); }
  .out-text :global(.ab13) { background: var(--ansi-13); }
  .out-text :global(.ab14) { background: var(--ansi-14); }
  .out-text :global(.ab15) { background: var(--ansi-15); }
  .out-text :global(.affg) { color: var(--fg); }
  .out-text :global(.afbg) { color: var(--term-bg); }
  .out-text :global(.abfg) { background: var(--fg); }
  .out-text :global(.abbg) { background: var(--term-bg); }
  .out-text :global(.a-b) { font-weight: 600; }
  .out-text :global(.a-d) { opacity: 0.65; }
  .out-text :global(.a-i) { font-style: italic; }
  .out-text :global(.a-u) { text-decoration: underline; }

  /* Markdown cells: the reading view's typography, at the prose size. */
  .nb-md {
    font-size: var(--nb-prose-font);
    line-height: 1.6;
    color: var(--fg);
    overflow-wrap: break-word;
    padding: 0.1em 0.2em;
  }

  .nb-md > :global(:first-child) {
    margin-top: 0.2em;
  }

  .nb-md > :global(:last-child) {
    margin-bottom: 0.2em;
  }

  .nb-md :global(h1),
  .nb-md :global(h2),
  .nb-md :global(h3),
  .nb-md :global(h4),
  .nb-md :global(h5),
  .nb-md :global(h6) {
    line-height: 1.25;
    margin: 1.2em 0 0.5em;
    font-weight: 600;
    letter-spacing: -0.01em;
  }

  .nb-md :global(h1) {
    font-size: 1.55em;
    padding-bottom: 0.3em;
    border-bottom: 1px solid var(--edge);
  }

  .nb-md :global(h2) {
    font-size: 1.25em;
  }

  .nb-md :global(h3) {
    font-size: 1.08em;
  }

  .nb-md :global(h4),
  .nb-md :global(h5),
  .nb-md :global(h6) {
    font-size: 1em;
  }

  .nb-md :global(p) {
    margin: 0.6em 0;
  }

  .nb-md :global(a) {
    color: var(--accent);
    text-decoration: none;
  }

  .nb-md :global(a:hover) {
    text-decoration: underline;
  }

  .nb-md :global(code) {
    font-family: var(--mono);
    font-size: 0.84em;
    background: color-mix(in srgb, var(--fg) 6%, transparent);
    border-radius: 4px;
    padding: 0.12em 0.34em;
  }

  .nb-md :global(pre) {
    background: color-mix(in srgb, var(--fg) 4.5%, transparent);
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 0.7em 0.9em;
    overflow-x: auto;
    scrollbar-width: thin;
    line-height: 1.5;
  }

  .nb-md :global(pre code) {
    background: none;
    padding: 0;
  }

  .nb-md :global(blockquote) {
    margin: 0.8em 0;
    padding: 0.4em 1em;
    border-left: 3px solid color-mix(in srgb, var(--accent) 60%, transparent);
    border-radius: 0 6px 6px 0;
    background: color-mix(in srgb, var(--accent) 5%, transparent);
    color: color-mix(in srgb, var(--fg) 55%, var(--muted));
  }

  .nb-md :global(ul),
  .nb-md :global(ol) {
    padding-left: 1.6em;
    margin: 0.5em 0;
  }

  .nb-md :global(li)::marker {
    color: color-mix(in srgb, var(--accent) 70%, var(--muted));
  }

  .nb-md :global(hr) {
    border: none;
    border-top: 1px solid var(--edge);
    margin: 1.4em 0;
  }

  .nb-md :global(img) {
    max-width: 100%;
  }

  .nb-md :global(.nb-math-block) {
    overflow-x: auto;
    scrollbar-width: thin;
    text-align: center;
  }

  .out-md {
    font-size: calc(var(--nb-prose-font) * 0.94);
  }

  .nb-scroll :global(.md-link-hint) {
    position: absolute;
    z-index: 5;
    max-width: min(26rem, calc(100% - 16px));
    padding: 0.3rem 0.6rem;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: var(--overlay-bg);
    box-shadow: 0 4px 14px color-mix(in srgb, var(--fg) 12%, transparent);
    color: var(--muted);
    font-size: var(--text-xs);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    pointer-events: none;
  }

  @media (prefers-reduced-motion: reduce) {
    .copy,
    .out-rail {
      transition: none;
    }
    .cell:global(.flash) {
      animation: none;
    }
  }
</style>
