<script lang="ts">
  import type { Snippet } from "svelte";

  /**
   * One live row in a work tray: the pulsing presence dot, the caller's
   * body content, and the optional square stop button — shared so the stop
   * affordance and the row rhythm can't drift between the sibling trays.
   */
  interface Props {
    /** Stop this row's work; omitted = no stop affordance (codex). */
    onStop?: () => void;
    /** The stop button's tooltip ("stop this subagent"). */
    stopTitle?: string;
    /** The row body (name, progress/status/elapsed spans — caller-styled). */
    children: Snippet;
  }
  let { onStop, stopTitle, children }: Props = $props();
</script>

<div class="row">
  <span class="dot" aria-hidden="true"></span>
  <div class="body">
    {@render children()}
  </div>
  {#if onStop !== undefined}
    <button class="stop" title={stopTitle} onclick={onStop}>
      <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
        <rect x="4.5" y="4.5" width="7" height="7" rx="1" fill="currentColor" />
      </svg>
    </button>
  {/if}
</div>

<style>
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 2px 0;
  }
  .dot {
    flex: none;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
    animation: pulse 1.4s ease-in-out infinite; /* shared keyframe in app.css */
  }
  .body {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: 8px;
    overflow: hidden;
  }
  .stop {
    flex: none;
    display: inline-flex;
    background: none;
    border: none;
    color: var(--muted);
    padding: 2px;
    cursor: pointer;
    transition: color 0.12s ease;
  }
  .stop:hover {
    color: var(--err);
  }
  @media (prefers-reduced-motion: reduce) {
    .dot {
      animation: none;
    }
  }
</style>
