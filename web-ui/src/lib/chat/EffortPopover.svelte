<script lang="ts">
  /**
   * The reasoning-effort popover: a faster↔smarter dot scale. Anchored under
   * the effort chip in the header (its `.menu-host` supplies the relative
   * origin). Opening/closing is the header's `menu` state; this only renders
   * the surface and reports a pick.
   */
  interface Props {
    choices: string[];
    shown: string | null;
    onPick: (level: string) => void;
  }

  let { choices, shown, onPick }: Props = $props();
</script>

<div class="overlay-surface effort-pop" role="group" aria-label="reasoning effort">
  <div class="effort-head">
    <span>effort</span>
    <strong>{shown ?? "default"}</strong>
  </div>
  <div class="effort-scale" aria-hidden="true">
    <span>faster</span>
    <span>smarter</span>
  </div>
  <div class="effort-track" role="radiogroup" aria-label="effort level">
    {#each choices as level (level)}
      <button
        class="effort-dot"
        class:active={level === shown}
        role="radio"
        aria-checked={level === shown}
        aria-label={level}
        title={level}
        onclick={() => onPick(level)}
      ></button>
    {/each}
  </div>
  <div class="effort-names" aria-hidden="true">
    {#each choices as level (level)}
      <button
        class="effort-name"
        class:current={level === shown}
        tabindex="-1"
        onclick={() => onPick(level)}
      >
        {level}
      </button>
    {/each}
  </div>
</div>

<style>
  /* .overlay-surface (the floating-surface recipe) is in app.css; this adds
     the dropdown anchor and the effort-scale layout. */
  .effort-pop {
    top: 100%;
    left: 0;
    margin-top: 4px;
    z-index: 20;
    min-width: 240px;
    padding: 10px 14px 12px;
  }
  .effort-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    color: var(--muted);
    font-size: var(--text-sm);
    padding-bottom: 8px;
  }
  .effort-head strong {
    color: var(--fg);
    font-family: var(--mono, monospace);
    font-weight: 600;
  }
  .effort-scale {
    display: flex;
    justify-content: space-between;
    color: var(--muted);
    font-size: var(--text-xs);
    padding-bottom: 4px;
  }
  .effort-track {
    display: flex;
    justify-content: space-between;
    align-items: center;
    background: color-mix(in srgb, var(--fg) 6%, transparent);
    border-radius: 999px;
    padding: 5px 8px;
  }
  .effort-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    border: none;
    background: color-mix(in srgb, var(--fg) 30%, transparent);
    padding: 0;
    cursor: pointer;
    transition:
      transform 0.12s ease,
      background-color 0.12s ease;
  }
  .effort-dot:hover {
    transform: scale(1.5);
    background: var(--fg);
  }
  .effort-dot.active {
    background: var(--accent);
    transform: scale(1.75);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 25%, transparent);
  }
  .effort-names {
    display: flex;
    justify-content: space-between;
    padding-top: 6px;
  }
  .effort-name {
    background: none;
    border: none;
    padding: 0;
    color: var(--muted);
    font-size: var(--text-xs);
    font-family: var(--mono, monospace);
    cursor: pointer;
  }
  .effort-name.current {
    color: var(--accent);
  }
</style>
