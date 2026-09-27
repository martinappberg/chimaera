<script lang="ts">
  /**
   * A composer attachment shown large before it is sent — the check that
   * the screenshot is the right one. The picture has no file yet, so this
   * is an overlay, not a pane (a sent picture opens in a pane). Esc, the
   * backdrop and ✕ close it; "remove" drops the attachment and closes.
   * Same overlay shape as the plan card's full-plan view.
   */
  interface Props {
    src: string;
    label: string;
    /** False while the owning view is hidden: never steal focus then. */
    visible?: boolean;
    onClose: () => void;
    onRemove?: () => void;
  }

  let { src, label, visible = true, onClose, onRemove }: Props = $props();

  let panel = $state<HTMLDivElement | null>(null);
  $effect(() => {
    if (visible) panel?.focus({ preventScroll: true });
  });

  function onKeydown(e: KeyboardEvent): void {
    // Esc closes the preview only — never reaches the composer's interrupt.
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
    }
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div class="preview" role="dialog" aria-modal="true" aria-label="preview {label}" tabindex="-1" onkeydown={onKeydown}>
  <button class="backdrop" type="button" aria-label="close preview" onclick={onClose}></button>
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div class="panel" tabindex="-1" bind:this={panel}>
    <img {src} alt={label} draggable="false" />
    <div class="bar">
      <span class="label">{label}</span>
      {#if onRemove !== undefined}
        <button class="act remove" type="button" onclick={onRemove}>remove</button>
      {/if}
      <button class="act" type="button" aria-label="close" onclick={onClose}>✕</button>
    </div>
  </div>
</div>

<style>
  .preview {
    position: fixed;
    inset: 0;
    z-index: 40;
    display: grid;
    place-items: center;
    padding: 4vh 4vw;
  }
  .backdrop {
    position: absolute;
    inset: 0;
    padding: 0;
    border: none;
    background: color-mix(in srgb, var(--bg) 70%, transparent);
    cursor: zoom-out;
  }
  .panel {
    position: relative;
    z-index: 1;
    display: flex;
    flex-direction: column;
    min-width: min(320px, 100%);
    max-width: 100%;
    max-height: 92vh;
    overflow: hidden;
    border: 1px solid var(--edge);
    border-radius: 10px;
    background: var(--overlay-bg);
    box-shadow: 0 10px 32px rgba(0, 0, 0, 0.28);
    outline: none;
    animation: rise 0.14s ease; /* @keyframes rise lives in app.css */
  }
  @media (prefers-reduced-motion: reduce) {
    .panel {
      animation: none;
    }
  }
  img {
    display: block;
    min-height: 0;
    max-width: 100%;
    max-height: calc(92vh - 36px);
    object-fit: contain;
    background: color-mix(in srgb, var(--fg) 3%, transparent);
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 34px;
    padding: 0 6px 0 12px;
    border-top: 1px solid color-mix(in srgb, var(--edge) 55%, transparent);
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono, monospace);
    font-size: var(--text-xs);
  }
  .act {
    flex: none;
    padding: 3px 8px;
    border: none;
    border-radius: 6px;
    background: none;
    color: var(--muted);
    font: inherit;
    cursor: pointer;
    transition:
      color 0.12s ease,
      background-color 0.12s ease;
  }
  .act:hover {
    color: var(--fg);
    background: color-mix(in srgb, var(--fg) 7%, transparent);
  }
  .act.remove:hover {
    color: var(--err);
  }
</style>
