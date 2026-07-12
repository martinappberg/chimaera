<script lang="ts">
  /**
   * The split edit|preview layout: an editor pane and a live-preview pane with a
   * draggable divider. Surface-agnostic — the host (MarkdownView / HtmlView)
   * passes the editor and the preview as snippets, so this owns only geometry.
   *
   * The editor pane is ALWAYS the first child, in a fixed slot: toggling `split`
   * off collapses to editor-only WITHOUT re-rendering the editor snippet, so the
   * host can flip preview⇄split⇄edit without ever remounting its CodeView (which
   * would drop the unsaved buffer + undo history). Keeping the preview "as you
   * type" in sync is the host's job — it renders the editor's live buffer.
   */
  import type { Snippet } from "svelte";

  interface Props {
    editor: Snippet;
    preview: Snippet;
    /** true = editor | divider | preview; false = editor only (full width). */
    split: boolean;
  }

  let { editor, preview, split }: Props = $props();

  const MIN = 0.2; // neither pane below 20% of the width
  let ratio = $state(0.5);
  let host = $state<HTMLDivElement | null>(null);
  let dragging = $state(false);

  function onPointerDown(e: PointerEvent): void {
    if (host === null) return;
    dragging = true;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    e.preventDefault();
  }

  function onPointerMove(e: PointerEvent): void {
    if (!dragging || host === null) return;
    const rect = host.getBoundingClientRect();
    if (rect.width === 0) return;
    const r = (e.clientX - rect.left) / rect.width;
    ratio = Math.min(1 - MIN, Math.max(MIN, r));
  }

  function onPointerUp(e: PointerEvent): void {
    if (!dragging) return;
    dragging = false;
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
  }
</script>

<div class="split" bind:this={host} class:dragging>
  <div class="pane editor" style:flex-basis={split ? `${ratio * 100}%` : "100%"}>
    {@render editor()}
  </div>
  {#if split}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="divider"
      role="separator"
      aria-orientation="vertical"
      onpointerdown={onPointerDown}
      onpointermove={onPointerMove}
      onpointerup={onPointerUp}
    ></div>
    <div class="pane preview">
      {@render preview()}
    </div>
  {/if}
</div>

<style>
  .split {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: stretch;
  }

  .pane {
    position: relative;
    min-width: 0;
    min-height: 0;
    flex: 0 0 auto;
  }

  .pane.preview {
    flex: 1 1 0;
    border-left: 1px solid var(--edge);
  }

  .divider {
    flex: 0 0 auto;
    width: 7px;
    margin: 0 -3px; /* widen the hit area without shifting the panes */
    z-index: 2;
    cursor: col-resize;
    background: transparent;
    transition: background-color 0.12s ease;
  }

  .divider:hover,
  .split.dragging .divider {
    background: color-mix(in srgb, var(--accent) 45%, transparent);
  }

  /* While dragging, don't let the editor/preview swallow the pointer. */
  .split.dragging .pane {
    pointer-events: none;
  }
</style>
