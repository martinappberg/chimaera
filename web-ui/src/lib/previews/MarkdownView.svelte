<script lang="ts">
  /**
   * Markdown preview (server-rendered comrak GFM, sanitized) with a
   * preview | split | edit toggle. The edit side is the shared CodeMirror editor
   * in markdown mode (Cmd/Ctrl+S saves; dirty dot + conflict handling all come
   * from CodeView). SPLIT shows the editor beside a live preview that re-renders
   * the editor's buffer client-side as you type — the file is still only written
   * on save; the plain Preview mode stays the authoritative server render, which
   * refreshes from disk on save (or an agent write). The editor mounts once and
   * survives every toggle, so flipping modes never drops an unsaved buffer.
   * Editing is offered only for files under the 1MB cap; larger markdown stays
   * preview-only.
  */
  import type { Component } from "svelte";
  import { EDIT_MAX_BYTES, type FileChunk } from "./files";
  import { retain, release, type FileEntry } from "./fileStore.svelte";
  import { clearSelection, setSelection } from "../shared/reference";
  import { getSetting } from "../settings/store.svelte";
  import { renderMarkdown } from "./mdRender";
  import SplitEditPreview from "./SplitEditPreview.svelte";
  import ReferenceChip from "../shared/ReferenceChip.svelte";
  import Spinner from "./Spinner.svelte";
  import { activateUrl, isWebUrl, urlMenuEntries } from "../shared/urlOpen";
  import { contextMenu } from "../shared/contextMenu.svelte";

  interface Props {
    path: string;
    /** Per-pane text-size override (px); the preview body scales to it. The
     *  A−/A+ pane controls and the Cmd/Ctrl +/− chords both drive this. */
    fontSize?: number;
  }

  let { path, fontSize = undefined }: Props = $props();

  // Preview base size: the pane override, else the Markdown preference. This
  // used to fall through to terminal.fontSize, coupling two unrelated content
  // surfaces and making the Editor settings appear only partly enforced.
  const bodyFont = $derived(fontSize ?? getSetting("editor.markdownFontSize"));
  const bodyLineHeight = $derived(getSetting("editor.markdownLineHeight"));

  type Mode = "preview" | "split" | "edit";
  let mode = $state<Mode>("preview");
  let chunk = $state<FileChunk | null>(null);
  let chunkError = $state<string | null>(null);
  /** Null until the first fetch tells us whether the file fits the edit cap. */
  let editable = $state<boolean | null>(null);
  /** The editor mounts on the first split/edit and then persists (CSS-hidden in
   *  preview) so no toggle drops the unsaved buffer. */
  let entered = $state(false);
  let CodeView = $state<
    Component<{ path: string; first: FileChunk; onDoc?: (text: string) => void }> | null
  >(null);
  let codeLoadError = $state<string | null>(null);
  $effect(() => {
    if (!entered || CodeView !== null) return;
    void import("./CodeView.svelte").then(
      (m) => (CodeView = m.default),
      () => (codeLoadError = "failed to load the editor"),
    );
  });
  /** The editor's live buffer, and its debounced mirror for the split preview —
   *  marked.parse + DOMPurify.sanitize over the whole buffer is too heavy to run
   *  on every keystroke (HtmlView debounces its iframe for the same reason). */
  let liveSource = $state("");
  let liveDebounced = $state("");
  $effect(() => {
    const src = liveSource;
    const t = setTimeout(() => (liveDebounced = src), 200);
    return () => clearTimeout(t);
  });
  const liveHtml = $derived(mode === "split" ? renderMarkdown(liveDebounced) : "");

  // The shared store entry: preview HTML lives here (cached across tab switches,
  // and re-rendered in place when the file changes on disk — a save on the edit
  // side, or an agent write, both flow through the store's revalidation).
  let entry = $state<FileEntry | null>(null);
  $effect(() => {
    const e = retain(path);
    entry = e;
    void e.ensureMarkdown();
    return () => release(path);
  });
  const html = $derived(entry?.markdown ?? null);
  const error = $derived(entry?.markdownError ?? null);

  // Reset per path.
  $effect(() => {
    void path;
    mode = "preview";
    chunk = null;
    chunkError = null;
    editable = null;
    entered = false;
    codeLoadError = null;
    liveSource = "";
    liveDebounced = "";
  });

  // --- context bridge: selection in the RENDERED preview ---------------------
  // No line mapping exists for rendered markdown, so the reference carries
  // the path + quoted excerpt only (the edit side goes through CodeView,
  // which has real line numbers).
  const selOwner = {};
  let contentEl = $state<HTMLDivElement | null>(null);

  /**
   * Links in a rendered document. Nothing set a `target` here, so a click was
   * a TOP-LEVEL navigation: in a browser that replaces the whole workbench,
   * and in the native app the shell's navigation guard swallows it. Route it
   * instead — a live local app (loopback / explicit port) opens in a browser
   * pane, anything else in the user's real browser. Delegated on `.md-content`
   * so it covers the authoritative render AND the live split preview.
   */
  function onLinkClick(e: MouseEvent): void {
    const anchor = (e.target as Element | null)?.closest?.("a[href]");
    const href = anchor?.getAttribute("href") ?? "";
    if (anchor === null || anchor === undefined) return;
    // Same-document anchors (a heading TOC) keep their native behavior.
    if (href.startsWith("#")) return;
    // `mailto:`/`tel:` are the browser's to handle — the OS knows what to do
    // with them and swallowing the click would just make the link look dead.
    // They cannot navigate the workbench away, so letting them through is safe.
    if (/^(mailto|tel):/i.test(href)) return;
    e.preventDefault();
    if (isWebUrl(href)) activateUrl(href, e.metaKey || e.ctrlKey);
    // A relative/in-repo href resolves against no meaningful base here, so it
    // is swallowed rather than allowed to navigate the workbench to a 404.
  }

  function onLinkContextMenu(e: MouseEvent): void {
    const anchor = (e.target as Element | null)?.closest?.("a[href]");
    const href = anchor?.getAttribute("href") ?? "";
    if (anchor === null || anchor === undefined || !isWebUrl(href)) return;
    contextMenu.openAt(e, urlMenuEntries(href));
  }
  let chipPos = $state<{ x: number; y: number } | null>(null);

  function syncPreviewSelection(): void {
    const content = contentEl;
    const s = document.getSelection();
    if (content === null || s === null || s.rangeCount === 0 || s.isCollapsed) {
      chipPos = null;
      clearSelection(selOwner);
      return;
    }
    const range = s.getRangeAt(0);
    if (!content.contains(range.commonAncestorContainer)) {
      // A selection elsewhere in the app: drop only what this view owns.
      chipPos = null;
      clearSelection(selOwner);
      return;
    }
    const text = s.toString();
    if (text.trim().length === 0) {
      chipPos = null;
      clearSelection(selOwner);
      return;
    }
    setSelection(selOwner, { kind: "file", path, startLine: null, endLine: null, text });
    const rects = range.getClientRects();
    const last = rects.length > 0 ? rects[rects.length - 1] : range.getBoundingClientRect();
    const rect = content.getBoundingClientRect();
    const clamp = (n: number, lo: number, hi: number) => Math.min(Math.max(n, lo), Math.max(lo, hi));
    chipPos = {
      x: clamp(last.right - rect.left + 4, 4, rect.width - 170),
      y: clamp(last.bottom - rect.top + 6, 4, rect.height - 58),
    };
  }

  $effect(() => {
    if (mode !== "preview") {
      chipPos = null;
      clearSelection(selOwner);
      return;
    }
    document.addEventListener("selectionchange", syncPreviewSelection);
    return () => {
      document.removeEventListener("selectionchange", syncPreviewSelection);
      chipPos = null;
      clearSelection(selOwner);
    };
  });

  async function enterEditor(target: "split" | "edit"): Promise<void> {
    // Read the raw source from the shared store (cached with the preview HTML);
    // CodeView handles the rest (background fill for under-cap truncated files
    // and the save/dirty/conflict flow).
    const e = entry;
    if (e === null) return;
    if (chunk === null && chunkError === null) {
      await e.ensureChunk();
      if (e.chunk !== null) {
        chunk = e.chunk;
        editable = e.chunk.size <= EDIT_MAX_BYTES;
      } else {
        chunkError = e.chunkError ?? "failed to load source";
        return;
      }
    }
    if (editable === false) return; // too large; stay in preview
    entered = true;
    mode = target;
  }
</script>

<div class="md-view" style:--markdown-line-height={bodyLineHeight}>
  <div class="md-bar">
    <div class="toggle" role="tablist" aria-label="markdown mode">
      <button
        class="seg"
        class:on={mode === "preview"}
        role="tab"
        aria-selected={mode === "preview"}
        onclick={() => (mode = "preview")}>preview</button
      >
      <button
        class="seg"
        class:on={mode === "split"}
        role="tab"
        aria-selected={mode === "split"}
        title={editable === false ? "over 1 MB — preview only" : "edit with a live preview"}
        disabled={editable === false}
        onclick={() => void enterEditor("split")}>split</button
      >
      <button
        class="seg"
        class:on={mode === "edit"}
        role="tab"
        aria-selected={mode === "edit"}
        title={editable === false ? "over 1 MB — preview only" : "edit source"}
        disabled={editable === false}
        onclick={() => void enterEditor("edit")}>edit</button
      >
    </div>
    {#if chunkError !== null}<span class="md-bar-err">{chunkError}</span>{/if}
  </div>

  <!-- Delegated link handling: the interactive targets are the rendered
       document's own <a> elements, which are already focusable and fire a
       native click on Enter that bubbles here — so keyboard access needs no
       separate handler on the container. -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="md-content"
    bind:this={contentEl}
    onclick={onLinkClick}
    oncontextmenu={onLinkContextMenu}
  >
    {#if mode === "preview" && chipPos !== null}
      <ReferenceChip x={chipPos.x} y={chipPos.y} />
    {/if}

    <!-- Authoritative server render (comrak). Shown in preview mode; kept in the
         DOM (just hidden) so re-entering preview needs no re-render. -->
    <div class="md-scroll" class:hidden={mode !== "preview"} onscroll={syncPreviewSelection}>
      {#if error !== null}
        <div class="file-error">{error}</div>
      {:else if html !== null}
        <article class="md-body" style:font-size="{bodyFont}px">
          <!-- eslint-disable-next-line svelte/no-at-html-tags — sanitized server-side -->
          {@html html}
        </article>
      {:else}
        <Spinner />
      {/if}
    </div>

    <!-- Editor (+ live preview in split). Mounts on the first split/edit and
         then persists, CSS-hidden in preview, so no toggle drops the buffer. -->
    {#if entered && chunk !== null}
      {@const first = chunk}
      <div class="edit-layer" class:hidden={mode === "preview"}>
        {#if CodeView !== null}
          <SplitEditPreview split={mode === "split"}>
            {#snippet editor()}
              <CodeView {path} {first} onDoc={(t) => (liveSource = t)} />
            {/snippet}
            {#snippet preview()}
              <div class="md-scroll">
                <article class="md-body" style:font-size="{bodyFont}px">
                  <!-- eslint-disable-next-line svelte/no-at-html-tags — sanitized in renderMarkdown -->
                  {@html liveHtml}
                </article>
              </div>
            {/snippet}
          </SplitEditPreview>
        {:else if codeLoadError !== null}
          <div class="file-error">{codeLoadError}</div>
        {:else}
          <Spinner />
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  .md-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  /* Quiet mode toggle bar, matching the pane top-bar treatment. */
  .md-bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    height: 26px;
    padding: 0 0.6rem;
    border-bottom: 1px solid var(--edge);
  }

  .toggle {
    display: flex;
    align-items: center;
    gap: 1px;
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

  .seg:hover:not(:disabled) {
    color: var(--fg);
  }

  .seg.on {
    color: var(--fg);
    background: var(--row-active);
  }

  .seg:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .md-bar-err {
    font-size: var(--text-xs);
    color: var(--err);
  }

  .md-content {
    flex: 1;
    position: relative;
    min-height: 0;
  }

  .md-scroll {
    position: absolute;
    inset: 0;
    overflow-y: auto;
    overflow-x: hidden;
  }

  .edit-layer {
    position: absolute;
    inset: 0;
  }

  .hidden {
    display: none;
  }

  .file-error {
    padding: 2rem;
    color: var(--muted);
    font-size: var(--text-md);
    text-align: center;
  }

  /* Base font-size is set inline (per-pane text size); every size below is in
     `em` so A−/A+ scales the whole document uniformly, like the terminal. */
  .md-body {
    max-width: 70ch;
    margin: 0 auto;
    padding: 2.2rem 2rem 3.5rem;
    font-size: var(--text-lg);
    line-height: var(--markdown-line-height);
    color: var(--fg);
    overflow-wrap: break-word;
  }

  .md-body :global(h1),
  .md-body :global(h2),
  .md-body :global(h3),
  .md-body :global(h4),
  .md-body :global(h5),
  .md-body :global(h6) {
    line-height: 1.25;
    margin: 1.6em 0 0.55em;
    font-weight: 600;
    letter-spacing: -0.01em;
  }

  .md-body :global(h1) {
    font-size: 1.576em;
    margin-top: 0.2em;
    padding-bottom: 0.35em;
    border-bottom: 1px solid var(--edge);
  }

  .md-body :global(h2) {
    font-size: 1.25em;
    padding-bottom: 0.25em;
    border-bottom: 1px solid var(--edge);
  }

  .md-body :global(h3) {
    font-size: 1.087em;
  }

  .md-body :global(h4),
  .md-body :global(h5),
  .md-body :global(h6) {
    font-size: 1em;
  }

  .md-body :global(p) {
    margin: 0.7em 0;
  }

  .md-body :global(a) {
    color: var(--accent);
    text-decoration: none;
  }

  .md-body :global(a:hover) {
    text-decoration: underline;
  }

  .md-body :global(code) {
    font-family: var(--mono);
    font-size: 0.82em;
    background: color-mix(in srgb, var(--fg) 6%, transparent);
    border-radius: 4px;
    padding: 0.12em 0.34em;
  }

  .md-body :global(pre) {
    background: color-mix(in srgb, var(--fg) 4.5%, transparent);
    border: 1px solid var(--edge);
    border-radius: 8px;
    padding: 0.8em 1em;
    overflow-x: auto;
    line-height: 1.5;
  }

  .md-body :global(pre code) {
    background: none;
    padding: 0;
    font-size: 0.848em;
  }

  .md-body :global(blockquote) {
    margin: 0.8em 0;
    padding: 0.1em 1em;
    border-left: 3px solid var(--edge);
    color: var(--muted);
  }

  .md-body :global(ul),
  .md-body :global(ol) {
    padding-left: 1.6em;
    margin: 0.6em 0;
  }

  .md-body :global(li) {
    margin: 0.2em 0;
  }

  .md-body :global(hr) {
    border: none;
    border-top: 1px solid var(--edge);
    margin: 1.8em 0;
  }

  .md-body :global(img) {
    max-width: 100%;
  }

  .md-body :global(table) {
    border-collapse: collapse;
    margin: 1em 0;
    display: block;
    overflow-x: auto;
    font-size: 0.924em;
  }

  .md-body :global(th),
  .md-body :global(td) {
    border: 1px solid var(--edge);
    padding: 0.35em 0.7em;
    text-align: left;
  }

  .md-body :global(th) {
    font-weight: 600;
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }

  .md-body :global(input[type="checkbox"]) {
    accent-color: var(--accent);
    margin-right: 0.4em;
  }
</style>
