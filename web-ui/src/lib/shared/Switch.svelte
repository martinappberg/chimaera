<script lang="ts">
  /**
   * The on/off switch — lifted out of settings/SettingRow.svelte's boolean
   * control so the Plugins cards and the settings form share ONE recipe
   * (accent wash + accent knob when on, muted knob on the row-hover ground
   * when off). A `role="switch"` button; the label is the caller's.
   */
  interface Props {
    on: boolean;
    /** Accessible name (the visible label lives in the caller's markup). */
    label: string;
    disabled?: boolean;
    onToggle: (next: boolean) => void;
  }

  let { on, label, disabled = false, onToggle }: Props = $props();
</script>

<button
  class="toggle"
  class:on
  role="switch"
  aria-checked={on}
  aria-label={label}
  {disabled}
  onclick={() => onToggle(!on)}
>
  <span class="knob"></span>
</button>

<style>
  .toggle {
    flex: none;
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--row-hover);
    width: 34px;
    height: 19px;
    border-radius: 10px;
    padding: 0;
    cursor: pointer;
    position: relative;
    transition:
      background-color 0.14s ease,
      border-color 0.14s ease;
  }

  .toggle:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .toggle .knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 13px;
    height: 13px;
    border-radius: 50%;
    background: var(--muted);
    transition:
      transform 0.14s ease,
      background-color 0.14s ease;
  }

  .toggle.on {
    background: color-mix(in srgb, var(--accent) 28%, var(--row-hover));
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }

  .toggle.on .knob {
    transform: translateX(15px);
    background: var(--accent);
  }

  @media (prefers-reduced-motion: reduce) {
    .toggle,
    .toggle .knob {
      transition: none;
    }
  }
</style>
