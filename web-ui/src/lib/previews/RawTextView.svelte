<script lang="ts">
  /**
   * A binary file's bytes shown as text, read-only — the "open as text
   * anyway" override. The editor refuses binary content (saving it would
   * rewrite bytes nobody touched), so this is a plain decode: UTF-8 with
   * replacement characters, control bytes drawn as their Unicode control
   * pictures (so a NUL is visible and never collapses the layout), paged
   * 256 KB at a time up to a 4 MB ceiling.
   */
  import { untrack } from "svelte";
  import { FILE_CHUNK, fsFile, humanSize, type FileChunk } from "./files";
  import { getSetting } from "../settings/store.svelte";

  interface Props {
    path: string;
    /** The first chunk the host already read. */
    first: FileChunk;
  }

  let { path, first }: Props = $props();

  /** Most bytes shown; past this the file belongs in a real tool. */
  const MAX_SHOWN = 4 * 1024 * 1024;

  // `first` seeds the view once; later chunks are this view's own reads.
  const initial = untrack(() => first);
  const decoder = new TextDecoder("utf-8", { fatal: false });
  let text = $state(picture(decoder.decode(initial.bytes, { stream: initial.truncated })));
  let loaded = $state(initial.bytes.length);
  let total = $state(initial.size);
  let more = $state(initial.truncated);
  let loading = $state(false);
  let error = $state<string | null>(null);

  const fontSize = $derived(getSetting("editor.fontSize"));

  /** C0 controls (except tab and newline) and DEL as control pictures. */
  function picture(s: string): string {
    // eslint-disable-next-line no-control-regex
    return s.replace(/[\u0000-\u0008\u000b-\u001f\u007f]/g, (c) =>
      String.fromCharCode(c === "\u007f" ? 0x2421 : 0x2400 + c.charCodeAt(0)),
    );
  }

  async function loadMore(): Promise<void> {
    if (loading || !more || loaded >= MAX_SHOWN) return;
    loading = true;
    error = null;
    try {
      const c = await fsFile(path, loaded, FILE_CHUNK);
      text += picture(decoder.decode(c.bytes, { stream: c.truncated }));
      loaded += c.bytes.length;
      total = c.size;
      more = c.truncated && c.bytes.length > 0;
    } catch (e) {
      error = e instanceof Error ? e.message : "failed to read more";
    } finally {
      loading = false;
    }
  }
</script>

<div class="raw-view">
  <pre class="raw-text" style:font-size="{fontSize}px">{text}</pre>
  <footer class="bar">
    <span class="status">{more ? `${humanSize(loaded)} of ${humanSize(total)}` : humanSize(total)} · read-only</span>
    {#if error !== null}<span class="bar-err">{error}</span>{/if}
    <span class="spacer"></span>
    {#if more && loaded < MAX_SHOWN}
      <button class="more-btn" disabled={loading} onclick={() => void loadMore()}>
        {loading ? "loading…" : "load more"}
      </button>
    {:else if more}
      <span class="hint">showing the first {humanSize(MAX_SHOWN)}</span>
    {/if}
  </footer>
</div>

<style>
  .raw-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  .raw-text {
    flex: 1;
    min-height: 0;
    margin: 0;
    padding: 10px 14px 14px;
    overflow: auto;
    scrollbar-width: thin;
    font-family: var(--editor-font);
    line-height: 1.5;
    color: var(--fg);
    white-space: pre;
    tab-size: 8;
  }

  .bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    height: 26px;
    padding: 0 0.7rem;
    border-top: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }

  .bar-err {
    color: var(--err);
  }

  .spacer {
    flex: 1;
  }

  .hint {
    font-family: var(--mono);
    opacity: 0.7;
  }

  .more-btn {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    padding: 0.1rem 0.4rem;
    border-radius: 4px;
  }

  .more-btn:hover:not(:disabled) {
    background: var(--row-hover);
    color: var(--fg);
  }

  .more-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
