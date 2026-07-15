<script lang="ts">
  /**
   * The disclosure chevron shared by the chat collapse surfaces (ToolGroup,
   * AgentsTray): a `›` that rotates to `⌄` when open. One home for the glyph
   * and its rotation so the two never drift — see chat/AGENTS.md ("shared
   * chrome should be shared, not re-pasted per card").
   */
  interface Props {
    /** Rotated 90° (points down) when true. */
    open?: boolean;
    /** Square px size of the glyph. */
    size?: number;
  }
  let { open = false, size = 10 }: Props = $props();
</script>

<span class="chev" class:open aria-hidden="true">
  <svg viewBox="0 0 16 16" width={size} height={size}>
    <path
      d="M6 4l4 4-4 4"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
    />
  </svg>
</span>

<style>
  .chev {
    display: inline-flex;
    flex: none;
    color: var(--muted);
    transition: transform 0.14s ease;
  }
  .chev.open {
    transform: rotate(90deg);
  }
  @media (prefers-reduced-motion: reduce) {
    .chev {
      transition: none;
    }
  }
</style>
