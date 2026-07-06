<script lang="ts">
  /**
   * Dispatch a file tab to its preview by extension: image / markdown /
   * sandboxed html / paged table / read-only code / binary info card.
   * The "text" path fetches the first 256KB here and sniffs it — anything
   * with NUL bytes falls through to the info card, so extensionless
   * binaries and .gz never render as garbage.
   */
  import type { Component } from "svelte";
  import { fsFile, looksBinary, viewKindFor, type FileChunk } from "./files";
  import ImageView from "./ImageView.svelte";
  import MarkdownView from "./MarkdownView.svelte";
  import HtmlView from "./HtmlView.svelte";
  import TableView from "./TableView.svelte";
  import BinaryView from "./BinaryView.svelte";

  interface Props {
    path: string;
  }

  let { path }: Props = $props();

  const kind = $derived(viewKindFor(path));

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

  let probe = $state<TextProbe>({ state: "loading" });

  $effect(() => {
    const p = path;
    if (viewKindFor(p) !== "text") return;
    probe = { state: "loading" };
    let stale = false;
    fsFile(p)
      .then((chunk) => {
        if (stale) return;
        probe = looksBinary(chunk.bytes)
          ? { state: "binary", size: chunk.size }
          : { state: "text", chunk };
      })
      .catch((e) => {
        if (!stale) {
          probe = { state: "error", message: e instanceof Error ? e.message : "failed to load file" };
        }
      });
    return () => {
      stale = true;
    };
  });
</script>

{#key path}
  <div class="file-view">
    {#if kind === "image"}
      <ImageView {path} />
    {:else if kind === "markdown"}
      <MarkdownView {path} />
    {:else if kind === "html"}
      <HtmlView {path} />
    {:else if kind === "table"}
      <TableView {path} />
    {:else if kind === "binary"}
      <BinaryView {path} />
    {:else if probe.state === "text"}
      {#if CodeView !== null}
        <CodeView {path} first={probe.chunk} />
      {/if}
    {:else if probe.state === "binary"}
      <BinaryView {path} knownSize={probe.size} />
    {:else if probe.state === "error"}
      <div class="file-error">{probe.message}</div>
    {/if}
  </div>
{/key}

<style>
  .file-view {
    position: absolute;
    inset: 0;
    background: var(--term-bg);
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
