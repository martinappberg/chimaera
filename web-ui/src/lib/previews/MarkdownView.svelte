<script lang="ts">
  /**
   * Markdown preview (server-rendered comrak GFM, sanitized) with an
   * Edit/Preview toggle. The edit side is the shared CodeMirror editor in
   * markdown mode (Cmd/Ctrl+S saves; dirty dot + conflict handling all come
   * from CodeView). Switching back to Preview re-renders from disk so saved
   * edits show immediately. Editing is offered only for files under the 1MB
   * cap; larger markdown stays preview-only.
   */
  import { fsMarkdown, fsFile, EDIT_MAX_BYTES, type FileChunk } from "./files";
  import { clearSelection, setSelection } from "../shared/reference";
  import { getSetting } from "../settings/store.svelte";
  import CodeView from "./CodeView.svelte";
  import ReferenceChip from "../shared/ReferenceChip.svelte";

  interface Props {
    path: string;
    /** Per-pane text-size override (px); the preview body scales to it. The
     *  A−/A+ pane controls and the Cmd/Ctrl +/− chords both drive this. */
    fontSize?: number;
  }

  let { path, fontSize = undefined }: Props = $props();

  // Preview base size: the pane override, else the app's base text size (the
  // same value the A−/A+ steps start from, so the first step never jumps).
  const bodyFont = $derived(fontSize ?? getSetting("terminal.fontSize"));

  let mode = $state<"preview" | "edit">("preview");
  let html = $state<string | null>(null);
  let error = $state<string | null>(null);
  let chunk = $state<FileChunk | null>(null);
  let chunkError = $state<string | null>(null);
  /** Null until the first fetch tells us whether the file fits the edit cap. */
  let editable = $state<boolean | null>(null);

  // Reset per path.
  $effect(() => {
    void path;
    mode = "preview";
    chunk = null;
    chunkError = null;
    editable = null;
  });

  // Preview HTML: (re)rendered from disk whenever we enter/return to preview,
  // so a save on the edit side is reflected without reopening the tab.
  $effect(() => {
    if (mode !== "preview") return;
    const p = path;
    html = null;
    error = null;
    let stale = false;
    fsMarkdown(p)
      .then((h) => {
        if (!stale) html = h;
      })
      .catch((e) => {
        if (!stale) error = e instanceof Error ? e.message : "failed to render markdown";
      });
    return () => {
      stale = true;
    };
  });

  // --- context bridge: selection in the RENDERED preview ---------------------
  // No line mapping exists for rendered markdown, so the reference carries
  // the path + quoted excerpt only (the edit side goes through CodeView,
  // which has real line numbers).
  const selOwner = {};
  let contentEl = $state<HTMLDivElement | null>(null);
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

  async function enterEdit(): Promise<void> {
    // Fetch the raw source once; CodeView handles the rest (incl. background
    // fill for under-cap truncated files and the save/dirty/conflict flow).
    if (chunk === null && chunkError === null) {
      try {
        const c = await fsFile(path);
        chunk = c;
        editable = c.size <= EDIT_MAX_BYTES;
      } catch (e) {
        chunkError = e instanceof Error ? e.message : "failed to load source";
        return;
      }
    }
    if (editable === false) return; // too large; stay in preview
    mode = "edit";
  }
</script>

<div class="md-view">
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
        class:on={mode === "edit"}
        role="tab"
        aria-selected={mode === "edit"}
        title={editable === false ? "over 1 MB — preview only" : "edit source"}
        disabled={editable === false}
        onclick={() => void enterEdit()}>edit</button
      >
    </div>
    {#if chunkError !== null}<span class="md-bar-err">{chunkError}</span>{/if}
  </div>

  <div class="md-content" bind:this={contentEl}>
    {#if mode !== "edit" && chipPos !== null}
      <ReferenceChip x={chipPos.x} y={chipPos.y} />
    {/if}
    {#if mode === "edit" && chunk !== null}
      <CodeView {path} first={chunk} />
    {:else}
      <div class="md-scroll" onscroll={syncPreviewSelection}>
        {#if error !== null}
          <div class="file-error">{error}</div>
        {:else if html !== null}
          <article class="md-body" style:font-size="{bodyFont}px">
            <!-- eslint-disable-next-line svelte/no-at-html-tags — sanitized server-side -->
            {@html html}
          </article>
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

  .file-error {
    padding: 2rem;
    color: var(--muted);
    font-size: 0.8rem;
    text-align: center;
  }

  /* Base font-size is set inline (per-pane text size); every size below is in
     `em` so A−/A+ scales the whole document uniformly, like the terminal. */
  .md-body {
    max-width: 70ch;
    margin: 0 auto;
    padding: 2.2rem 2rem 3.5rem;
    font-size: 0.92rem;
    line-height: 1.65;
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
