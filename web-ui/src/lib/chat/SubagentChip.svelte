<script lang="ts">
  /**
   * The "agent" label on a subagent's tray row. When the subagent has a
   * conversation to open it is the button that opens it (as a view of its
   * own, beside this chat); otherwise it is the plain lane label it always
   * was. Shared by the two trays so the affordance cannot drift.
   */
  interface Props {
    label?: string;
    /** Open the subagent's conversation; omitted = a plain label. */
    onOpen?: (newSplit: boolean) => void;
  }
  let { label = "agent", onOpen }: Props = $props();
</script>

{#if onOpen !== undefined}
  <button
    class="chip open"
    title="open this subagent's conversation"
    onclick={(e) => onOpen?.(e.metaKey || e.ctrlKey)}
  >
    {label}
    <svg viewBox="0 0 16 16" width="9" height="9" aria-hidden="true">
      <path
        d="M6 4h6v6M12 4L5 11"
        fill="none"
        stroke="currentColor"
        stroke-width="1.6"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
    </svg>
  </button>
{:else}
  <span class="chip">{label}</span>
{/if}

<style>
  .chip {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font: inherit;
    font-size: var(--text-xs);
    line-height: inherit;
    padding: 0 6px;
    border: none;
    border-radius: 999px;
    color: var(--muted);
    background: color-mix(in srgb, var(--fg) 6%, transparent);
  }
  .open {
    cursor: pointer;
    transition:
      color 0.12s ease,
      background-color 0.12s ease;
  }
  .open svg {
    opacity: 0.55;
  }
  .open:hover,
  .open:focus-visible {
    color: var(--fg);
    background: color-mix(in srgb, var(--accent) 22%, transparent);
  }
  .open:hover svg,
  .open:focus-visible svg {
    opacity: 1;
  }
</style>
