<script lang="ts">
  import Chevron from "../shared/Chevron.svelte";

  /**
   * The one-line disclosure every collapsible activity line shares (a tool
   * group, a folded run): a muted sentence with a trailing chevron, a
   * small working glyph while work under it runs, its elapsed (the
   * duration once settled), a badge when it failed. A quiet line, not a
   * card — the Claude/Codex apps render activity this way, so prose stays
   * the page's voice.
   */
  interface Props {
    label: string;
    open: boolean;
    tooltip: string;
    running?: boolean;
    health?: "failed" | "recovered" | null;
    /** "1m 22s": how long the work under the line has run (live) or took
     *  (settled); null hides it (old journals, sub-second work). */
    elapsed?: string | null;
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
    elapsed = null,
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
    <!-- ⁘ — four dots taking turns: work in flight, not an alarm. -->
    <svg class="working" viewBox="0 0 10 10" width="10" height="10" aria-hidden="true">
      <circle cx="5" cy="1.6" r="1.25" />
      <circle cx="8.4" cy="5" r="1.25" />
      <circle cx="5" cy="8.4" r="1.25" />
      <circle cx="1.6" cy="5" r="1.25" />
    </svg>
  {/if}
  <span class="label">{label}</span>
  {#if elapsed !== null}
    <span class="elapsed">{elapsed}</span>
  {/if}
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
  /* Running: the four-dot glyph, each dot brightening in turn (the label
     already says what is running). */
  .working {
    flex: none;
    fill: var(--accent);
  }
  .working circle {
    opacity: 0.3;
    animation: working-turn 1.2s ease-in-out infinite;
  }
  .working circle:nth-child(2) {
    animation-delay: 0.3s;
  }
  .working circle:nth-child(3) {
    animation-delay: 0.6s;
  }
  .working circle:nth-child(4) {
    animation-delay: 0.9s;
  }
  @keyframes working-turn {
    0%,
    60%,
    100% {
      opacity: 0.3;
    }
    25% {
      opacity: 1;
    }
  }
  :global(html.app-hidden) .working circle,
  .summary.paused .working circle {
    animation-play-state: paused;
  }
  @media (prefers-reduced-motion: reduce) {
    .working circle {
      animation: none;
      opacity: 0.8;
    }
  }
  .elapsed {
    flex: none;
    color: color-mix(in srgb, var(--muted) 80%, transparent);
    font-variant-numeric: tabular-nums;
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
