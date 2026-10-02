<script lang="ts">
  import { toolbarPopover } from "../shared/toolbarPopover";

  interface Props {
    choices: string[];
    shown: string | null;
    onPick: (level: string) => void;
    onClose: () => void;
  }

  let { choices, shown, onPick, onClose }: Props = $props();
</script>

<div class="overlay-surface effort-pop" use:toolbarPopover={{ onClose }} role="menu" aria-label="Reasoning effort">
  <div class="effort-head">Reasoning effort <strong>{shown ?? "default"}</strong></div>
  <div class="effort-scale" aria-hidden="true"><span>Faster</span><span>Smarter</span></div>
  <div class="effort-track" style:--steps={choices.length}>
    {#each choices as level (level)}
      <button class="effort-option" class:current={level === shown}
        role="menuitemradio" aria-checked={level === shown} onclick={() => onPick(level)}>
        <span class="effort-dot" aria-hidden="true"></span>
        <span>{level}</span>
      </button>
    {/each}
  </div>
</div>

<style>
  .effort-pop { min-width: 264px; padding: 14px; }
  .effort-head { display: flex; justify-content: space-between; gap: 16px; color: var(--muted); font-size: var(--text-sm); margin-bottom: 12px; }
  .effort-head strong { color: var(--fg); font-family: var(--mono); font-weight: 600; }
  .effort-scale { display: flex; justify-content: space-between; color: var(--muted); font-size: var(--text-xs); padding-bottom: 6px; }
  .effort-track { display: flex; position: relative; }
  /* One continuous scale preserves the faster → smarter relationship. The
     full column is clickable, while the visual mark stays small and quiet. */
  .effort-track::before { content: ""; position: absolute; top: 12px; left: calc(50% / var(--steps)); right: calc(50% / var(--steps)); height: 3px; border-radius: 2px; background: color-mix(in srgb, var(--muted) 24%, var(--overlay-bg)); pointer-events: none; }
  .effort-option { position: relative; flex: 1; min-width: 0; display: flex; flex-direction: column; align-items: center; gap: 10px; border: 0; border-radius: 6px; padding: 9px 4px 6px; background: none; color: var(--muted); font: inherit; font-family: var(--mono); font-size: var(--text-xs); cursor: pointer; }
  .effort-option:hover { color: var(--fg); }
  .effort-option:hover .effort-dot { background: var(--fg); }
  .effort-option:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: -2px; }
  .effort-option.current { color: var(--accent); }
  .effort-dot { width: 9px; height: 9px; border-radius: 50%; background: color-mix(in srgb, var(--muted) 50%, var(--overlay-bg)); box-shadow: 0 0 0 3px var(--overlay-bg); }
  .effort-option.current .effort-dot { background: var(--accent); box-shadow: 0 0 0 3px var(--overlay-bg), 0 0 0 5px color-mix(in srgb, var(--accent) 22%, var(--overlay-bg)); }
</style>
