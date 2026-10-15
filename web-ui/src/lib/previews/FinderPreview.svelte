<script lang="ts">
  import { dirname, formatMtime, humanSize, type FsEntry } from "./files";
  import FileIcon from "../shared/FileIcon.svelte";
  import EmbedCard from "../shared/embed/EmbedCard.svelte";
  import { pathTarget, resolveTargets, type TargetResult } from "../shared/embed/embed";
  import { watchPaneVisible } from "./paneVisible";
  import { pageVisible } from "../shared/visibility";
  import { isMac } from "../shared/keys";
  import Spinner from "./Spinner.svelte";

  let { entry, onOpen }: {
    entry: FsEntry | null;
    onOpen: (newSplit: boolean) => void;
  } = $props();

  let host = $state<HTMLElement | null>(null);
  let visible = $state(true);
  const path = $derived(entry?.path ?? null);
  const broken = $derived(entry?.broken === true);
  // Key the card by path: a slow response from the previous selection must
  // never paint under the new filename. Arrowing past files loads nothing.
  let ready = $state<string | null>(null);
  let resolver = $state<(() => Promise<TargetResult | null>) | null>(null);

  $effect(() => host === null ? undefined : watchPaneVisible(host, (v) => { visible = v; }));
  $effect(() => {
    const p = path;
    ready = null;
    resolver = null;
    if (p === null || !visible || !$pageVisible || broken) return;
    const abort = new AbortController();
    const target = pathTarget(p);
    const timer = setTimeout(() => {
      resolver = async () => (await resolveTargets([target], dirname(p), { signal: abort.signal }))[target] ?? null;
      ready = p;
    }, 150);
    return () => {
      clearTimeout(timer);
      abort.abort();
    };
  });
</script>

<section class="preview" aria-label="File preview" bind:this={host}>
  {#if entry !== null}
    <div class="head">
      <FileIcon path={entry.path} size={18} link={entry.symlink} broken={entry.broken} />
      <span class="name" title={entry.path}>{entry.name}</span>
      <button class="open" disabled={entry.broken}
        title={`Open in a tab · ${isMac ? "⌘" : "Ctrl"}-click to open beside`}
        onclick={(e) => onOpen(e.metaKey || e.ctrlKey)}>Open</button>
    </div>
    <div class="facts">
      <span>{humanSize(entry.size)}</span>
      <span>Modified {formatMtime(entry.mtime)}</span>
    </div>
    {#if entry.symlink}<div class="link-target" title={entry.target}>Link → {entry.target ?? entry.path}</div>{/if}
    <div class="body">
      {#if entry.broken}
        <div class="empty">This link’s target is missing.</div>
      {:else if ready === entry.path && resolver !== null}
        {#key ready}
          <EmbedCard path={ready} chrome={false} resolve={resolver} onOpen={() => onOpen(false)} />
        {/key}
      {:else}
        <Spinner label="loading preview…" />
      {/if}
    </div>
    <div class="hint">Double-click a file or press Enter to open</div>
  {:else}
    <div class="empty">
      <span class="empty-title">Select a file to preview</span>
      <span>Browse with a click or the arrow keys.</span>
    </div>
  {/if}
</section>

<style>
  .preview {
    height: 100%;
    min-height: 0;
    display: flex;
    flex-direction: column;
    color: var(--fg);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 12px 6px;
    min-width: 0;
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
    font-size: var(--text-sm);
  }
  .open {
    flex: none;
    padding: 5px 10px;
    border: 1px solid var(--edge);
    border-radius: 5px;
    background: var(--row-hover);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .open:hover:not(:disabled) {
    background: var(--row-active);
  }
  .open:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .open:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .facts {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
    padding: 0 12px 10px;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .link-target {
    padding: 0 12px 8px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .body {
    position: relative;
    flex: 1;
    min-height: 0;
    overflow: auto;
    scrollbar-width: thin;
    border-top: 1px solid var(--edge);
  }
  .hint {
    padding: 8px 12px;
    border-top: 1px solid var(--edge);
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    justify-content: center;
    align-items: center;
    gap: 7px;
    padding: 24px;
    text-align: center;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .empty-title {
    color: var(--fg);
    font-size: var(--text-sm);
  }
</style>
