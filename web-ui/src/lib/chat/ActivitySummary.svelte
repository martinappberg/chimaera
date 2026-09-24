<script lang="ts">
  import Chevron from "../shared/Chevron.svelte";

  /**
   * The one-line disclosure every collapsible activity line shares (a tool
   * group, a folded run): a muted sentence with a trailing chevron, a
   * breathing dot while work under it runs, a badge when it failed. A quiet
   * line, not a card — the Claude/Codex apps render activity this way, so
   * prose stays the page's voice.
   */
  interface Props {
    label: string;
    open: boolean;
    tooltip: string;
    running?: boolean;
    health?: "failed" | "recovered" | null;
    /** False while a retained chat tab is hidden; pauses the live dot. */
    visible?: boolean;
    onToggle: () => void;
  }

  let {
    label,
    open,
    tooltip,
    running = false,
    health = null,
    visible = true,
    onToggle,
  }: Props = $props();
</script>

<button
  class="summary"
  class:failed={health === "failed"}
  class:paused={!visible}
  aria-expanded={open}
  onclick={onToggle}
  title={tooltip}
>
  {#if running}
    <span class="live-dot" aria-hidden="true"></span>
  {/if}
  <span class="label">{label}</span>
  {#if health === "failed"}
    <span class="badge bad">failed</span>
  {:else if health === "recovered"}
    <span class="badge soft">recovered</span>
  {/if}
  <Chevron {open} />
</button>

<style>
  /* Block-level but content-wide: an inline box would sit on the parent's
     prose-height line and pad every activity line by the strut. */
  .summary {
    display: flex;
    align-items: center;
    gap: 6px;
    width: fit-content;
    max-width: 100%;
    margin-left: -6px;
    padding: 2px 6px;
    background: none;
    border: none;
    border-radius: 6px;
    color: var(--activity-fg, var(--muted));
    font: inherit;
    font-size: var(--text-xs);
    line-height: 1.4;
    text-align: left;
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }
  .summary:hover,
  .summary:focus-visible {
    color: var(--fg);
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }
  .summary :global(.chev) {
    opacity: 0.55;
  }
  .summary:hover :global(.chev) {
    opacity: 1;
  }
  .summary.failed {
    color: color-mix(in srgb, var(--err) 80%, var(--muted));
  }
  .label {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* Running: a small breathing dot instead of a badge (the label already
     says what is running, in the present tense). */
  .live-dot {
    flex: none;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
    animation: live-breathe 1.6s ease-in-out infinite;
  }
  @keyframes live-breathe {
    0%,
    100% {
      opacity: 0.35;
    }
    50% {
      opacity: 1;
    }
  }
  :global(html.app-hidden) .live-dot,
  .summary.paused .live-dot {
    animation-play-state: paused;
  }
  @media (prefers-reduced-motion: reduce) {
    .live-dot {
      animation: none;
    }
  }
  .badge {
    flex: none;
    font-size: var(--text-xs);
    padding: 0 6px;
    border-radius: 999px;
  }
  .badge.bad {
    color: var(--err);
    background: color-mix(in srgb, var(--err) 12%, transparent);
  }
  /* Recovered: worth a glance, not an alarm. */
  .badge.soft {
    color: var(--muted);
    background: color-mix(in srgb, var(--fg) 7%, transparent);
  }
</style>
