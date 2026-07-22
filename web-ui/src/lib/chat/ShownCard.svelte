<script lang="ts">
  import { basename, fsBoardRender } from "../previews/files";
  import Spinner from "../previews/Spinner.svelte";

  /**
   * Inline board card under the tool call that produced it — the agent
   * "showing you something" mid-work via `chimaera board show`
   * (docs/board-plan.md §10.1). v1 is deliberately client-side: ToolCallCard
   * detects the `shown … → *.board.json` signature in the completed result
   * text and mounts this card; the planned daemon-injected `shown` journal
   * event can replace that detection later without touching the card itself.
   * The render is server-side and content-addressed, so a re-mount (transcript
   * paging, tab re-activation) is a cache hit, not a re-render.
   */
  interface Props {
    /** The .board.json path (already resolved by the caller). */
    path: string;
    /** Open the board in a file tab (the workbench path-click flow). */
    onOpen?: (path: string) => void;
  }

  let { path, onOpen }: Props = $props();

  let imgUrl = $state<string | null>(null);
  let size = $state<[number, number] | null>(null);
  let error = $state<string | null>(null);

  $effect(() => {
    const p = path;
    let cancelled = false;
    imgUrl = null;
    error = null;
    fsBoardRender(p, 0).then(
      (r) => {
        if (cancelled) return;
        imgUrl = `/raw/${r.ticket}`;
        size = [r.width, r.height];
      },
      (err: unknown) => {
        if (cancelled) return;
        error = err instanceof Error ? err.message : String(err);
      },
    );
    return () => {
      cancelled = true;
    };
  });
</script>

<div class="shown">
  <button class="head" title="open {basename(path)} in a pane" onclick={() => onOpen?.(path)}>
    <span class="chip">board</span>
    <span class="name">{basename(path)}</span>
  </button>
  {#if error !== null}
    <!-- Quiet failure: an expired/unrenderable board never shows a broken img. -->
    <div class="err">board preview unavailable — {error}</div>
  {:else if imgUrl !== null}
    <button class="stage" title="open in a pane" onclick={() => onOpen?.(path)}>
      <img
        src={imgUrl}
        alt={basename(path)}
        width={size?.[0]}
        height={size?.[1]}
        loading="lazy"
        decoding="async"
      />
    </button>
  {:else}
    <div class="loading">
      <Spinner />
    </div>
  {/if}
</div>

<style>
  .shown {
    margin: 6px 10px 8px;
    border: 1px solid var(--edge);
    border-radius: 8px;
    overflow: hidden;
    background: color-mix(in srgb, var(--fg) 2%, transparent);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 5px 10px;
    background: none;
    border: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    cursor: pointer;
    transition: background-color 0.12s ease;
  }
  .head:hover {
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }
  .chip {
    flex: none;
    padding: 0 6px;
    border-radius: 999px;
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    font-family: var(--mono, monospace);
    font-size: var(--text-xs);
  }
  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono, monospace);
  }
  .stage {
    display: block;
    width: 100%;
    padding: 8px;
    background: none;
    border: none;
    border-top: 1px solid color-mix(in srgb, var(--edge) 55%, transparent);
    cursor: zoom-in;
    text-align: center;
  }
  .stage img {
    width: auto;
    height: auto;
    max-width: 100%;
    max-height: 320px;
    object-fit: contain;
    border-radius: 4px;
  }
  .loading {
    position: relative;
    min-height: 96px;
    border-top: 1px solid color-mix(in srgb, var(--edge) 55%, transparent);
  }
  .err {
    padding: 5px 10px;
    border-top: 1px solid color-mix(in srgb, var(--edge) 55%, transparent);
    color: var(--muted);
    font-size: var(--text-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
