<script lang="ts">
  /**
   * A still countdown: a thin ring whose arc is the share of a wait still to
   * go (loops and scheduled jobs — the Ahead card, the rail, the dashboard).
   * Waiting is quiet, so it never animates on its own; it turns accent in
   * the last minute, and a caller swaps it for a live dot once the wait is
   * over.
   */
  interface Props {
    /** 0..1: the share of the wait left. */
    fraction: number;
    /** The last minute: the arc takes the accent. */
    soon?: boolean;
    size?: number;
    title?: string;
  }
  let { fraction, soon = false, size = 13, title }: Props = $props();

  const R = 5.2;
  const C = 2 * Math.PI * R;
  const arc = $derived(Math.max(0, Math.min(1, fraction)) * C);
</script>

<svg
  class="ring"
  class:soon
  viewBox="0 0 14 14"
  width={size}
  height={size}
  role={title !== undefined ? "img" : undefined}
  aria-label={title}
  aria-hidden={title === undefined}
>
  {#if title !== undefined}<title>{title}</title>{/if}
  <circle class="track" cx="7" cy="7" r={R} fill="none" stroke-width="1.6" />
  {#if arc > 0.05}
    <circle
      class="arc"
      cx="7"
      cy="7"
      r={R}
      fill="none"
      stroke-width="1.6"
      stroke-linecap="round"
      stroke-dasharray="{arc} {C}"
      transform="rotate(-90 7 7)"
    />
  {/if}
</svg>

<style>
  .ring {
    flex: none;
    display: block;
  }
  .track {
    stroke: color-mix(in srgb, var(--muted) 35%, transparent);
  }
  .arc {
    stroke: var(--muted);
  }
  .soon .arc {
    stroke: var(--accent);
  }
</style>
