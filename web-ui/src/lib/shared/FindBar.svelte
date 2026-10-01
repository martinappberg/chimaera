<script lang="ts">
  import { onMount } from "svelte";
  interface Props {
    query: string;
    caseSensitive?: boolean;
    status: string;
    scope: string;
    canStep: boolean;
    onQuery: (query: string) => void;
    onCase: (value: boolean) => void;
    onStep: (direction: 1 | -1) => void;
    onClose: () => void;
  }
  let { query, caseSensitive = false, status, scope, canStep, onQuery, onCase, onStep, onClose }: Props = $props();
  let input: HTMLInputElement;
  export function focus(): void { input?.focus(); input?.select(); }
  onMount(focus);
  function keydown(e: KeyboardEvent): void {
    if (e.isComposing) return;
    if (e.key === "Enter" && e.target !== input) return;
    if (e.key === "Escape" || e.key === "Enter") {
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape") onClose();
      else if (canStep) onStep(e.shiftKey ? -1 : 1);
    }
  }
</script>

<!-- Escape closes from every control; Enter in the input advances the match. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div class="find-bar" role="search" aria-label={scope} onkeydown={keydown}>
  <div class="find-controls">
    <input bind:this={input} value={query} oninput={(e) => onQuery(e.currentTarget.value)}
      aria-label={scope} placeholder={scope} type="text" spellcheck="false" autocomplete="off" maxlength="512" />
    <button class:enabled={caseSensitive} aria-pressed={caseSensitive} aria-label="Match case" title="Match case"
      onclick={() => onCase(!caseSensitive)}>Aa</button>
    <span class="status" aria-live="polite" aria-atomic="true">{status}</span>
    <button disabled={!canStep} aria-label="Previous match" title="Previous match (Shift+Enter)" onclick={() => onStep(-1)}>↑</button>
    <button disabled={!canStep} aria-label="Next match" title="Next match (Enter)" onclick={() => onStep(1)}>↓</button>
    <button aria-label="Close find" title="Close find (Esc)" onclick={onClose}>×</button>
  </div>
  <div class="scope">{scope}</div>
</div>

<style>
  .find-bar { flex: none; padding: 6px 10px 4px; background: var(--bg); border-bottom: 1px solid var(--edge); color: var(--fg); }
  .find-controls { display: flex; flex-wrap: wrap; align-items: center; gap: 4px; min-width: 0; }
  input { width: 180px; flex: 1; min-width: 40px; border: 1px solid var(--edge); border-radius: 4px; padding: 4px 6px; color: var(--fg); background: var(--term-bg); font: inherit; font-size: 12px; }
  input:focus { outline: 1px solid var(--accent); border-color: var(--accent); }
  button { flex: none; border: 0; background: transparent; color: var(--fg-dim); border-radius: 3px; min-width: 24px; height: 25px; cursor: pointer; }
  button:hover, button.enabled { background: color-mix(in srgb, var(--accent) 15%, transparent); color: var(--fg); }
  button:focus-visible { outline: 1px solid var(--accent); }
  button:disabled { opacity: .35; cursor: default; }
  .status { font-size: 11px; color: var(--fg-dim); white-space: nowrap; min-width: 55px; text-align: center; }
  .scope { font-size: 10px; color: var(--fg-dim); padding-top: 3px; }
</style>
