<script lang="ts">
  /**
   * HTML file view with an Edit/Preview toggle (parity with MarkdownView).
   * Preview is the sandboxed iframe (the daemon serves the page under CSP
   * "sandbox allow-scripts"); Edit is the shared CodeMirror editor in HTML
   * mode (Cmd/Ctrl+S saves; dirty dot + conflict handling come from CodeView).
   * Returning to Preview re-mints the /raw/ ticket so a saved edit shows
   * immediately. Editing is offered only for files under the 1MB cap.
   */
  import { EDIT_MAX_BYTES, type FileChunk } from "./files";
  import { retain, release, type FileEntry } from "./fileStore.svelte";
  import CodeView from "./CodeView.svelte";
  import Spinner from "./Spinner.svelte";

  interface Props {
    path: string;
  }

  let { path }: Props = $props();

  let mode = $state<"preview" | "edit">("preview");
  let chunk = $state<FileChunk | null>(null);
  let chunkError = $state<string | null>(null);
  /** Null until the first fetch tells us whether the file fits the edit cap. */
  let editable = $state<boolean | null>(null);

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
  });

  async function enterEdit(): Promise<void> {
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
    mode = "edit";
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
        class:on={mode === "edit"}
        role="tab"
        aria-selected={mode === "edit"}
        title={editable === false ? "over 1 MB — preview only" : "edit source"}
        disabled={editable === false}
        onclick={() => void enterEdit()}>edit</button
      >
    </div>
    {#if chunkError !== null}<span class="html-bar-err">{chunkError}</span>{/if}
  </div>

  <div class="html-content">
    {#if mode === "edit" && chunk !== null}
      <CodeView {path} first={chunk} />
    {:else if error !== null}
      <div class="file-error">{error}</div>
    {:else if url !== null}
      <iframe src={url} title={path} sandbox="allow-scripts"></iframe>
    {:else}
      <Spinner />
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

  iframe {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    border: none;
    background: #ffffff; /* pages assume a white canvas regardless of theme */
  }

  .file-error {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--muted);
    font-size: 0.8rem;
    padding: 1rem;
    text-align: center;
  }
</style>
