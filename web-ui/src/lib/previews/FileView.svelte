<script lang="ts">
  /**
   * Dispatch a file tab to its preview by extension: image / markdown /
   * sandboxed html / paged table / read-only code / binary info card.
   * The "text" path fetches the first 256KB here and sniffs it — anything
   * with NUL bytes falls through to the info card, so extensionless
   * binaries and .gz never render as garbage.
   */
  import type { Component } from "svelte";
  import { looksBinary, midTruncate, viewKindFor, type FileChunk } from "./files";
  import { retain, release, type FileEntry } from "./fileStore.svelte";
  import ImageView from "./ImageView.svelte";
  import MarkdownView from "./MarkdownView.svelte";
  import HtmlView from "./HtmlView.svelte";
  import TableView from "./TableView.svelte";
  import BinaryView from "./BinaryView.svelte";
  import PdfView from "./PdfView.svelte";
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

  // CodeMirror is by far the heaviest dependency in the app; load it only
  // when a text file is actually opened so the terminal-only path stays lean.
  let CodeView = $state<Component<{ path: string; first: FileChunk }> | null>(null);
  $effect(() => {
    if (kind !== "text" || CodeView !== null) return;
    void import("./CodeView.svelte").then((m) => (CodeView = m.default));
  });

  type TextProbe =
    | { state: "loading" }
    | { state: "text"; chunk: FileChunk }
    | { state: "binary"; size: number }
    | { state: "error"; message: string };

  // The store entry for this path: retaining pins it warm across a tab switch
  // (no refetch on return) and marks it on-screen, so a disk change revalidates
  // it live. Only the "text" kind reads its first chunk here; the other kinds
  // mount a sub-view that reads its own payload from the same entry.
  let entry = $state<FileEntry | null>(null);
  $effect(() => {
    const p = path;
    const e = retain(p);
    entry = e;
    if (viewKindFor(p) === "text") void e.ensureChunk();
    return () => release(p);
  });

  const probe = $derived.by<TextProbe>(() => {
    if (kind !== "text") return { state: "loading" };
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
    return looksBinary(chunk.bytes)
      ? { state: "binary", size: chunk.size }
      : { state: "text", chunk };
  });
</script>

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
    <div class="viewer">
      {#if kind === "image"}
        <ImageView {path} />
      {:else if kind === "markdown"}
        <MarkdownView {path} {fontSize} />
      {:else if kind === "html"}
        <HtmlView {path} />
      {:else if kind === "table"}
        <TableView {path} />
      {:else if kind === "pdf"}
        <PdfView {path} />
      {:else if kind === "binary"}
        <BinaryView {path} />
      {:else if probe.state === "text"}
        {#if CodeView !== null}
          <CodeView {path} first={probe.chunk} />
        {:else}
          <Spinner />
        {/if}
      {:else if probe.state === "binary"}
        <BinaryView {path} knownSize={probe.size} />
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
