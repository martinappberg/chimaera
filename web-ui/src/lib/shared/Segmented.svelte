<script lang="ts">
  /**
   * The segmented control — lifted out of settings/SettingRow.svelte's enum
   * control so the Plugins tab's Installed · Skills · Browse switcher and the
   * settings form share ONE recipe: a hairline pill group, the chosen segment
   * on an accent wash. A disabled option renders quietly with its own hint
   * (Browse "later") instead of vanishing.
   */
  interface Option {
    value: string;
    label: string;
    /** Rendered but not choosable; `hint` sits beside the label. */
    disabled?: boolean;
    hint?: string;
    title?: string;
  }

  interface Props {
    options: readonly Option[];
    value: string;
    /** Accessible name for the group. */
    label: string;
    onChange: (value: string) => void;
    /** Let the bar wrap into separate pills at narrow widths. */
    wrap?: boolean;
  }

  let { options, value, label, onChange, wrap = false }: Props = $props();
</script>

<div class="seg" class:wrap role="radiogroup" aria-label={label}>
  {#each options as opt (opt.value)}
    <button
      class="seg-btn"
      class:on={value === opt.value}
      role="radio"
      aria-checked={value === opt.value}
      disabled={opt.disabled === true}
      title={opt.title}
      onclick={() => onChange(opt.value)}
    >
      {opt.label}{#if opt.hint}<span class="hint">{opt.hint}</span>{/if}
    </button>
  {/each}
</div>

<style>
  .seg {
    display: flex;
    border: 1px solid var(--edge);
    border-radius: 7px;
    overflow: hidden;
  }

  .seg-btn {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--muted);
    padding: 3px 11px;
    cursor: pointer;
    white-space: nowrap;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .seg-btn + .seg-btn {
    border-left: 1px solid var(--edge);
  }

  .seg-btn:hover:not(:disabled) {
    color: var(--fg);
    background: var(--row-hover);
  }

  .seg-btn.on {
    color: var(--fg);
    font-weight: 600;
    background: color-mix(in srgb, var(--accent) 16%, transparent);
  }

  .seg-btn:disabled {
    cursor: default;
    color: color-mix(in srgb, var(--muted) 70%, transparent);
  }

  .hint {
    margin-left: 6px;
    font-size: var(--text-xs);
    font-weight: 400;
    opacity: 0.8;
  }

  /* Narrow: the bar can't shrink, so it wraps as separated pills. */
  .seg.wrap {
    flex-wrap: wrap;
    gap: 5px;
    border: none;
    border-radius: 0;
    overflow: visible;
  }
  .seg.wrap .seg-btn {
    border: 1px solid var(--edge);
    border-radius: 6px;
  }
</style>
