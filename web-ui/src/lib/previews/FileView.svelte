<script lang="ts">
  /**
   * Dispatch a file tab to its preview by extension: image / markdown (or
   * slides, for a Marp deck) / sandboxed html / paged table / PDF / video +
   * audio / notebook / log / mermaid diagram / Word / PowerPoint / diagram
   * board (JSON Canvas, Excalidraw, draw.io) / Parquet / read-only code /
   * binary info card.
   * The "text" path fetches the first 256KB here and sniffs it — anything
   * with NUL bytes falls through to the info card, so extensionless
   * binaries and .gz never render as garbage.
   *
   * Per-tab overrides live here, reset when the tab shows another path: a
   * binary file opened as text anyway, a Marp deck shown as markdown, a
   * diagram or board shown as source.
   */
  import { untrack, type Component, type Snippet } from "svelte";
  import { looksBinary, midTruncate, viewKindFor, type FileChunk } from "./files";
  import { retain, release, type FileEntry } from "./fileStore.svelte";
  import { isMarpFrontmatter, isMarpSource } from "./marp";
  import ImageView from "./ImageView.svelte";
  import MediaView from "./MediaView.svelte";
  import TableView from "./TableView.svelte";
  import BinaryView from "./BinaryView.svelte";
  import RawTextView from "./RawTextView.svelte";
  import Spinner from "./Spinner.svelte";

  interface Props {
    path: string;
    /** Active workspace root — files outside it show their full path. */
    wsRoot?: string | null;
    /** Per-pane text-size override (px); markdown preview scales to it. */
    fontSize?: number;
  }

  let { path, wsRoot = null, fontSize = undefined }: Props = $props();

  const kind = $derived(viewKindFor(path));

  // A file OUTSIDE the workspace shows its full path so its origin is
  // unambiguous — an in-workspace file's location is implied by the workspace
  // and the FILES tree, so it stays clean.
  const wsNorm = $derived(
    wsRoot !== null && wsRoot.length > 1 && wsRoot.endsWith("/") ? wsRoot.slice(0, -1) : wsRoot,
  );
  const external = $derived(wsNorm !== null && path !== wsNorm && !path.startsWith(`${wsNorm}/`));

  // --- per-tab overrides -----------------------------------------------------------

  /** A binary file the reader asked to see as text anyway. */
  let asText = $state(false);
  /** A log whose bytes turned out binary (its size, for the card). */
  let logBinary = $state<number | null>(null);
  /** Whether this markdown file is a Marp deck: null until its first chunk
   *  or render says. Decided once per tab, so an edit that adds or removes
   *  `marp: true` never swaps the view out from under the editor. */
  let marp = $state<boolean | null>(null);
  let slidesMode = $state<"slides" | "markdown">("slides");
  let mermaidMode = $state<"diagram" | "source">("diagram");
  let boardMode = $state<"board" | "source">("board");
  $effect(() => {
    void path;
    asText = false;
    logBinary = null;
    marp = null;
    slidesMode = "slides";
    mermaidMode = "diagram";
    boardMode = "board";
  });

  /** Kinds whose first chunk this view reads (and sniffs) itself. */
  const readsChunk = (k: string, text: boolean) => k === "text" || k === "mermaid" || text;
  /** A board's source view reads its chunk too (only once asked for). */
  const boardSource = $derived(kind === "board" && boardMode === "source");

  // CodeMirror is by far the heaviest dependency in the app; load it only
  // when a text file is actually opened so the terminal-only path stays lean.
  let CodeView = $state<Component<{ path: string; first: FileChunk }> | null>(null);
  let MarkdownView = $state<Component<{
    path: string;
    fontSize?: number;
    wsRoot?: string | null;
  }> | null>(null);
  let HtmlView = $state<Component<{ path: string }> | null>(null);
  let XlsxView = $state<Component<{ path: string }> | null>(null);
  let PdfView = $state<Component<{ path: string }> | null>(null);
  let NotebookView = $state<Component<{ path: string; wsRoot?: string | null }> | null>(null);
  let LogView = $state<Component<{ path: string; onBinary?: (size: number) => void }> | null>(null);
  let SlidesView = $state<Component<{ path: string; switcher?: Snippet }> | null>(null);
  let MermaidView = $state<Component<{ path: string; chunk: FileChunk; switcher?: Snippet }> | null>(
    null,
  );
  let DocxView = $state<Component<{ path: string }> | null>(null);
  let PptxView = $state<Component<{ path: string }> | null>(null);
  let BoardView = $state<Component<{ path: string; wsRoot?: string | null; switcher?: Snippet }> | null>(null);
  let ParquetView = $state<Component<{ path: string }> | null>(null);
  let lazyError = $state<string | null>(null);
  const wantsCode = $derived(
    (kind === "text" && !asText) || (kind === "mermaid" && mermaidMode === "source") || boardSource,
  );
  $effect(() => {
    if (!wantsCode || CodeView !== null) return;
    void import("./CodeView.svelte").then(
      (m) => (CodeView = m.default),
      () => (lazyError = "failed to load the text preview"),
    );
  });
  $effect(() => {
    if (kind !== "markdown" || MarkdownView !== null) return;
    void import("./MarkdownView.svelte").then(
      (m) => (MarkdownView = m.default),
      () => (lazyError = "failed to load the markdown preview"),
    );
  });
  $effect(() => {
    if (kind !== "html" || HtmlView !== null) return;
    void import("./HtmlView.svelte").then(
      (m) => (HtmlView = m.default),
      () => (lazyError = "failed to load the HTML preview"),
    );
  });
  $effect(() => {
    if (kind !== "xlsx" || XlsxView !== null) return;
    void import("./XlsxView.svelte").then(
      (m) => (XlsxView = m.default),
      () => (lazyError = "failed to load the spreadsheet preview"),
    );
  });
  $effect(() => {
    if (kind !== "pdf" || PdfView !== null) return;
    void import("./PdfView.svelte").then(
      (m) => (PdfView = m.default),
      () => (lazyError = "failed to load the PDF preview"),
    );
  });
  $effect(() => {
    if (kind !== "notebook" || NotebookView !== null) return;
    void import("./NotebookView.svelte").then(
      (m) => (NotebookView = m.default),
      () => (lazyError = "failed to load the notebook preview"),
    );
  });
  $effect(() => {
    if (kind !== "log" || LogView !== null) return;
    void import("./LogView.svelte").then(
      (m) => (LogView = m.default),
      () => (lazyError = "failed to load the log preview"),
    );
  });
  $effect(() => {
    if (kind !== "markdown" || marp !== true || SlidesView !== null) return;
    void import("./SlidesView.svelte").then(
      (m) => (SlidesView = m.default),
      () => (lazyError = "failed to load the slides preview"),
    );
  });
  $effect(() => {
    if (kind !== "mermaid" || MermaidView !== null) return;
    void import("./MermaidView.svelte").then(
      (m) => (MermaidView = m.default),
      () => (lazyError = "failed to load the diagram preview"),
    );
  });
  $effect(() => {
    if (kind !== "docx" || DocxView !== null) return;
    void import("./DocxView.svelte").then(
      (m) => (DocxView = m.default),
      () => (lazyError = "failed to load the Word preview"),
    );
  });
  $effect(() => {
    if (kind !== "pptx" || PptxView !== null) return;
    void import("./PptxView.svelte").then(
      (m) => (PptxView = m.default),
      () => (lazyError = "failed to load the PowerPoint preview"),
    );
  });
  $effect(() => {
    if (kind !== "board" || BoardView !== null) return;
    void import("./BoardView.svelte").then(
      (m) => (BoardView = m.default),
      () => (lazyError = "failed to load the board preview"),
    );
  });
  $effect(() => {
    if (kind !== "parquet" || ParquetView !== null) return;
    void import("./ParquetView.svelte").then(
      (m) => (ParquetView = m.default),
      () => (lazyError = "failed to load the Parquet preview"),
    );
  });
  $effect(() => {
    void path;
    lazyError = null;
  });

  type TextProbe =
    | { state: "loading" }
    | { state: "text"; chunk: FileChunk }
    | { state: "binary"; size: number }
    | { state: "error"; message: string };

  // The store entry for this path: retaining pins it warm across a tab switch
  // (no refetch on return) and marks it on-screen, so a disk change revalidates
  // it live. Only the kinds read as text read their first chunk here; the
  // other kinds mount a sub-view that reads its own payload from the same entry.
  let entry = $state<FileEntry | null>(null);
  $effect(() => {
    const p = path;
    const e = retain(p);
    entry = e;
    if (readsChunk(viewKindFor(p), false)) void e.ensureChunk();
    else void e.ensureMtime();
    return () => release(p);
  });
  // "Open as text" on an extension-known binary, or a board's source: its
  // chunk was never read.
  $effect(() => {
    if ((asText && kind === "binary") || boardSource) void entry?.ensureChunk();
  });

  const probe = $derived.by<TextProbe>(() => {
    if (!readsChunk(kind, asText) && !boardSource) return { state: "loading" };
    const e = entry;
    // `entry` is assigned in the effect below (which runs AFTER this derived
    // re-evaluates on a path change), so on a switch it briefly still points at
    // the PREVIOUS path. Treat a mismatched entry as loading, forcing the
    // {#key path} block to unmount/remount CodeView with the correct chunk
    // rather than seeding it from the old file's bytes.
    if (e === null || e.path !== path || (e.chunk === null && e.chunkError === null))
      return { state: "loading" };
    if (e.chunkError !== null) return { state: "error", message: e.chunkError };
    const chunk = e.chunk;
    if (chunk === null) return { state: "loading" };
    return looksBinary(chunk.bytes) && !asText
      ? { state: "binary", size: chunk.size }
      : { state: "text", chunk };
  });

  // Decide once whether a markdown file is a Marp deck, from whichever of
  // its payloads lands first: the reading render's frontmatter, or the
  // editor's source chunk. No extra request either way.
  $effect(() => {
    if (kind !== "markdown" || untrack(() => marp) !== null) return;
    const e = entry;
    if (e === null || e.path !== path) return;
    const md = e.markdown;
    if (md !== null) {
      marp = md.frontmatter !== null && isMarpFrontmatter(md.frontmatter);
      return;
    }
    const chunk = e.chunk;
    if (chunk !== null) {
      marp = isMarpSource(new TextDecoder().decode(chunk.bytes.subarray(0, 16 * 1024)));
    }
  });
</script>

{#snippet slidesSwitch()}
  <div class="switch" role="tablist" aria-label="deck view">
    <button class="seg" class:on={slidesMode === "slides"} role="tab" aria-selected={slidesMode === "slides"}
      onclick={() => (slidesMode = "slides")}>slides</button
    >
    <button class="seg" class:on={slidesMode === "markdown"} role="tab" aria-selected={slidesMode === "markdown"}
      onclick={() => (slidesMode = "markdown")}>markdown</button
    >
  </div>
{/snippet}

{#snippet mermaidSwitch()}
  <div class="switch" role="tablist" aria-label="diagram view">
    <button class="seg" class:on={mermaidMode === "diagram"} role="tab" aria-selected={mermaidMode === "diagram"}
      onclick={() => (mermaidMode = "diagram")}>diagram</button
    >
    <button class="seg" class:on={mermaidMode === "source"} role="tab" aria-selected={mermaidMode === "source"}
      onclick={() => (mermaidMode = "source")}>source</button
    >
  </div>
{/snippet}

{#snippet boardSwitch()}
  <div class="switch" role="tablist" aria-label="board view">
    <button class="seg" class:on={boardMode === "board"} role="tab" aria-selected={boardMode === "board"}
      onclick={() => (boardMode = "board")}>board</button
    >
    <button class="seg" class:on={boardMode === "source"} role="tab" aria-selected={boardMode === "source"}
      onclick={() => (boardMode = "source")}>source</button
    >
  </div>
{/snippet}

{#snippet lazyFallback()}
  {#if lazyError !== null}
    <div class="file-error">{lazyError}</div>
  {:else}
    <Spinner />
  {/if}
{/snippet}

{#key path}
  <div class="file-view">
    {#if external}
      <!-- Full path for an out-of-workspace file: where is this coming from? -->
      <div class="ext-path" title={path}>
        <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M12 6h-6a2 2 0 0 0 -2 2v10a2 2 0 0 0 2 2h10a2 2 0 0 0 2 -2v-6" />
          <path d="M11 13l9 -9" />
          <path d="M15 4h5v5" />
        </svg>
        <span class="ext-text">{midTruncate(path, 80)}</span>
      </div>
    {/if}
    {#if asText && probe.state === "text"}
      <!-- A binary shown as text: say so, and offer the way back. -->
      <div class="alt-bar" role="status">
        <span class="alt-note">binary file shown as text</span>
        <span class="spacer"></span>
        <button class="seg" onclick={() => (asText = false)}>file info</button>
      </div>
    {:else if kind === "mermaid" && mermaidMode === "source"}
      <div class="alt-bar">
        <span class="spacer"></span>
        {@render mermaidSwitch()}
      </div>
    {:else if boardSource}
      <div class="alt-bar">
        <span class="spacer"></span>
        {@render boardSwitch()}
      </div>
    {/if}
    <div class="viewer">
      {#if kind === "image"}
        <ImageView {path} />
      {:else if kind === "markdown" && marp === true && slidesMode === "slides"}
        {#if SlidesView !== null}
          <SlidesView {path} switcher={slidesSwitch} />
        {:else}
          {@render lazyFallback()}
        {/if}
      {:else if kind === "markdown"}
        {#if MarkdownView !== null}
          <MarkdownView {path} {fontSize} {wsRoot} />
          {#if marp === true}
            <!-- Over the markdown bar's free right end. -->
            <div class="switch-float">{@render slidesSwitch()}</div>
          {/if}
        {:else}
          {@render lazyFallback()}
        {/if}
      {:else if kind === "html"}
        {#if HtmlView !== null}
          <HtmlView {path} />
        {:else}
          {@render lazyFallback()}
        {/if}
      {:else if kind === "table"}
        <TableView {path} />
      {:else if kind === "xlsx"}
        {#if XlsxView !== null}
          {#key entry?.mtime ?? path}
            <XlsxView {path} />
          {/key}
        {:else}
          {@render lazyFallback()}
        {/if}
      {:else if kind === "pdf"}
        {#if PdfView !== null}
          {#key entry?.mtime ?? path}
            <PdfView {path} />
          {/key}
        {:else}
          {@render lazyFallback()}
        {/if}
      {:else if kind === "video" || kind === "audio"}
        <!-- Not keyed on mtime like the other ticketed views: MediaView
             swaps a re-minted URL in place and keeps the playhead. -->
        <MediaView {path} {kind} />
      {:else if kind === "notebook"}
        {#if NotebookView !== null}
          <NotebookView {path} {wsRoot} />
        {:else}
          {@render lazyFallback()}
        {/if}
      {:else if kind === "log" && logBinary !== null}
        <BinaryView {path} knownSize={logBinary} />
      {:else if kind === "log"}
        {#if LogView !== null}
          <LogView {path} onBinary={(size) => (logBinary = size)} />
        {:else}
          {@render lazyFallback()}
        {/if}
      {:else if kind === "mermaid" && mermaidMode === "diagram" && probe.state === "text" && !asText}
        {#if MermaidView !== null}
          <MermaidView {path} chunk={probe.chunk} switcher={mermaidSwitch} />
        {:else}
          {@render lazyFallback()}
        {/if}
      {:else if kind === "docx"}
        {#if DocxView !== null}
          <DocxView {path} />
        {:else}
          {@render lazyFallback()}
        {/if}
      {:else if kind === "pptx"}
        {#if PptxView !== null}
          <PptxView {path} />
        {:else}
          {@render lazyFallback()}
        {/if}
      {:else if kind === "board" && boardMode === "board"}
        {#if BoardView !== null}
          <BoardView {path} {wsRoot} switcher={boardSwitch} />
        {:else}
          {@render lazyFallback()}
        {/if}
      {:else if kind === "parquet"}
        {#if ParquetView !== null}
          <!-- Keyed on the version: a rewritten file is a new footer and new offsets. -->
          {#key entry?.mtime ?? path}
            <ParquetView {path} />
          {/key}
        {:else}
          {@render lazyFallback()}
        {/if}
      {:else if kind === "binary" && !asText}
        {#key entry?.mtime ?? path}
          <BinaryView {path} onText={() => (asText = true)} />
        {/key}
      {:else if probe.state === "text" && asText}
        <RawTextView {path} first={probe.chunk} />
      {:else if probe.state === "text"}
        {#if CodeView !== null}
          <CodeView {path} first={probe.chunk} />
        {:else}
          {@render lazyFallback()}
        {/if}
      {:else if probe.state === "binary"}
        {#key entry?.mtime ?? path}
          <BinaryView {path} knownSize={probe.size} onText={() => (asText = true)} />
        {/key}
      {:else if probe.state === "error"}
        <div class="file-error">{probe.message}</div>
      {:else if probe.state === "loading"}
        <Spinner />
      {/if}
    </div>
  </div>
{/key}

<style>
  .file-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    background: var(--term-bg);
  }

  /* The sub-viewers fill this positioned box (they are absolute inset:0). */
  .viewer {
    position: relative;
    flex: 1;
    min-height: 0;
  }

  .ext-path {
    flex: none;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 3px 10px;
    border-bottom: 1px solid var(--edge);
    background: color-mix(in srgb, var(--accent) 5%, transparent);
    color: var(--muted);
    font-family: var(--mono);
    font-size: var(--text-xs);
    overflow: hidden;
  }

  .ext-path svg {
    flex: none;
    opacity: 0.8;
  }

  .ext-text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: ltr;
  }

  /* The strip an alternate text view gets, matching the viewers' own bars. */
  .alt-bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    height: 26px;
    padding: 0 0.5rem 0 0.7rem;
    border-bottom: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
    min-width: 0;
  }

  .alt-note {
    color: var(--warn);
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .spacer {
    flex: 1;
  }

  .switch {
    display: flex;
    align-items: center;
    gap: 1px;
  }

  .switch-float {
    position: absolute;
    top: 0;
    right: 0;
    /* One pixel short of the bar, so its bottom rule stays unbroken. */
    height: 25px;
    display: flex;
    align-items: center;
    padding: 0 0.5rem 0 0.6rem;
    background: var(--term-bg);
  }

  .seg {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    letter-spacing: 0.04em;
    color: var(--muted);
    cursor: pointer;
    padding: 2px 8px;
    border-radius: 4px;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .seg:hover {
    color: var(--fg);
  }

  .seg.on {
    color: var(--fg);
    background: var(--row-active);
  }

  .file-error {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--muted);
    font-size: var(--text-md);
    padding: 1rem;
    text-align: center;
  }

  @media (prefers-reduced-motion: reduce) {
    .seg {
      transition: none;
    }
  }
</style>
