<script lang="ts">
  /** The goal mark: a ring around a point. `active` takes the accent (the
   *  agent is working toward it), `warn` a stopped goal, `idle` a paused one. */
  interface Props {
    tone?: "active" | "idle" | "warn" | "done";
    size?: number;
  }
  let { tone = "active", size = 13 }: Props = $props();
</script>

<svg class="goal {tone}" viewBox="0 0 14 14" width={size} height={size} aria-hidden="true">
  <circle cx="7" cy="7" r="5.2" fill="none" stroke="currentColor" stroke-width="1.4" />
  <circle cx="7" cy="7" r="1.9" fill="currentColor" />
</svg>

<style>
  .goal {
    flex: none;
    display: block;
    color: var(--muted);
  }
  .goal.active,
  .goal.done {
    color: var(--accent);
  }
  .goal.warn {
    color: var(--warn);
  }
</style>
