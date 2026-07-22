<script lang="ts">
  /**
   * The present-mode overlay chrome for BoardView: a minimal auto-hiding
   * pagebar plus the presenter-notes strip. Purely presentational — plain
   * value props and callbacks only, no shared state.
   */
  interface Props {
    pageLabel: string;
    page: number;
    pageCount: number;
    /** The auto-hide state; the parent owns the mouse-idle timer. */
    faded: boolean;
    notesOpen: boolean;
    notes: string | null;
    onstep: (delta: number) => void;
    onexit: () => void;
    ontogglenotes: () => void;
  }
  let { pageLabel, page, pageCount, faded, notesOpen, notes, onstep, onexit, ontogglenotes }: Props =
    $props();
</script>

<div class="present-chrome">
  <div class="present-bar" class:faded>
    <button class="nav" disabled={page === 0} onclick={() => onstep(-1)} aria-label="previous page"
      >‹</button
    >
    <span class="page-label">{pageLabel} · {page + 1}/{pageCount}</span>
    <button class="nav" disabled={page + 1 >= pageCount} onclick={() => onstep(1)} aria-label="next page"
      >›</button
    >
    <button
      class="nav wide"
      class:on={notesOpen}
      onclick={ontogglenotes}
      aria-label="toggle presenter notes"
      title="presenter notes (n)">notes</button
    >
    <button class="nav wide" onclick={onexit} aria-label="exit presentation" title="exit (esc)"
      >exit</button
    >
  </div>
  {#if notesOpen}
    <div class="notes-strip">{notes ?? "no notes for this page"}</div>
  {/if}
</div>

<style>
  .present-chrome {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    padding: 12px;
    pointer-events: none;
  }
  .present-bar {
    pointer-events: auto;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    background: color-mix(in srgb, var(--term-bg) 82%, transparent);
    border: 1px solid var(--edge);
    border-radius: 8px;
    font-size: var(--text-xs);
    color: var(--muted);
    backdrop-filter: blur(6px);
    transition: opacity 0.25s ease;
  }
  .present-bar.faded {
    opacity: 0;
    pointer-events: none;
  }
  .notes-strip {
    pointer-events: auto;
    align-self: stretch;
    max-height: 26vh;
    overflow-y: auto;
    padding: 10px 14px;
    background: color-mix(in srgb, var(--term-bg) 88%, transparent);
    border: 1px solid var(--edge);
    border-radius: 8px;
    font-size: var(--text-sm);
    color: var(--fg);
    white-space: pre-wrap;
    backdrop-filter: blur(6px);
  }
  .nav {
    background: none;
    border: 1px solid var(--edge);
    border-radius: 4px;
    color: var(--fg);
    width: 22px;
    height: 22px;
    line-height: 1;
    cursor: pointer;
  }
  .nav.wide {
    width: auto;
    padding: 0 8px;
    font-size: var(--text-xs);
  }
  .nav.on {
    background: var(--row-active);
  }
  .nav:disabled {
    opacity: 0.35;
    cursor: default;
  }
  .nav:not(:disabled):hover {
    background: var(--row-hover);
  }
  .page-label {
    font-family: var(--mono);
  }
</style>
