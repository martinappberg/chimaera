<script lang="ts">
  /**
   * HTML file view with a preview | split | edit toggle (parity with
   * MarkdownView). Preview is the sandboxed iframe the daemon serves under CSP
   * "sandbox allow-scripts" (relative assets resolve through the /raw ticket).
   * Edit is the shared CodeMirror editor in HTML mode (Cmd/Ctrl+S saves; dirty
   * dot + conflict handling come from CodeView). SPLIT puts the editor beside a
   * live preview of the editor's buffer (a sandboxed `srcdoc` iframe, same
   * origin-less isolation) that re-renders as you type — the file is still only
   * written on save, and the plain Preview mode stays the authoritative /raw
   * render (the one that can load the page's relative assets). The editor mounts
   * once and survives every toggle, so flipping modes never drops an unsaved
   * buffer. The live srcdoc is debounced so a keystroke doesn't reload the whole
   * iframe on every character. Editing is offered only for files under the 1MB
   * cap.
  */
  import type { Component } from "svelte";
  import { EDIT_MAX_BYTES, type FileChunk } from "./files";
  import { retain, release, type FileEntry } from "./fileStore.svelte";
  import SplitEditPreview from "./SplitEditPreview.svelte";
  import Spinner from "./Spinner.svelte";

  interface Props {
    path: string;
  }

  let { path }: Props = $props();

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
  /** The editor's live buffer, and its debounced mirror for the live iframe (a
   *  full srcdoc reload per keystroke would flicker/scroll-reset). */
  let liveSource = $state("");
  let liveDebounced = $state("");
  $effect(() => {
    const src = liveSource;
    const t = setTimeout(() => (liveDebounced = src), 200);
    return () => clearTimeout(t);
  });

  // The shared store entry holds the ticketed /raw/ URL: cached across a tab
  // switch and re-minted in place when the file changes on disk (a save on the
  // edit side, or an agent write) — so preview reflects it without reopening.
  // The daemon serves HTML under CSP "sandbox allow-scripts" and the iframe
  // repeats the sandbox — the bearer token never appears in a URL.
  let entry = $state<FileEntry | null>(null);
  $effect(() => {
    const e = retain(path);
    entry = e;
    void e.ensureRawUrl();
    return () => release(path);
  });
  const url = $derived(entry?.rawUrl ?? null);
  const error = $derived(entry?.rawError ?? null);

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

  async function enterEditor(target: "split" | "edit"): Promise<void> {
    // Read the raw source from the shared store (cached with the preview URL);
    // CodeView handles the rest (background fill + save/dirty/conflict flow).
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

<div class="html-view">
  <div class="html-bar">
    <div class="toggle" role="tablist" aria-label="html mode">
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
    {#if chunkError !== null}<span class="html-bar-err">{chunkError}</span>{/if}
  </div>

  <div class="html-content">
    <!-- Authoritative /raw render (loads relative assets). Shown in preview
         mode; kept mounted (hidden) so re-entering needs no re-mint. -->
    <div class="layer" class:hidden={mode !== "preview"}>
      {#if error !== null}
        <div class="file-error">{error}</div>
      {:else if url !== null}
        <iframe src={url} title={path} sandbox="allow-scripts"></iframe>
      {:else}
        <Spinner />
      {/if}
    </div>

    <!-- Editor (+ live preview in split). Mounts on the first split/edit and
         then persists, CSS-hidden in preview, so no toggle drops the buffer. -->
    {#if entered && chunk !== null}
      {@const first = chunk}
      <div class="layer" class:hidden={mode === "preview"}>
        {#if CodeView !== null}
          <SplitEditPreview split={mode === "split"}>
            {#snippet editor()}
              <CodeView {path} {first} onDoc={(t) => (liveSource = t)} />
            {/snippet}
            {#snippet preview()}
              <iframe class="live" title="{path} (live)" sandbox="allow-scripts" srcdoc={liveDebounced}
              ></iframe>
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
  .html-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  /* Quiet mode toggle bar, matching the markdown/pane top-bar treatment. */
  .html-bar {
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

  .html-bar-err {
    font-size: var(--text-xs);
    color: var(--err);
  }

  .html-content {
    flex: 1;
    position: relative;
    min-height: 0;
  }

  .layer {
    position: absolute;
    inset: 0;
  }

  .hidden {
    display: none;
  }

  iframe {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    border: none;
    background: #ffffff; /* pages assume a white canvas regardless of theme */
  }

  /* The live srcdoc iframe fills its split pane rather than the whole view. */
  iframe.live {
    position: absolute;
    inset: 0;
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
</style>
