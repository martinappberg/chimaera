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
    <div class="field">
    <svg class="search-icon" viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.4" aria-hidden="true"><circle cx="6.8" cy="6.8" r="4.2" /><path d="m10 10 3.5 3.5" /></svg>
    <input bind:this={input} value={query} oninput={(e) => onQuery(e.currentTarget.value)}
      aria-label={scope} placeholder={scope} type="text" spellcheck="false" autocomplete="off" maxlength="512" />
    <button class:enabled={caseSensitive} aria-pressed={caseSensitive} aria-label="Match case" title="Match case"
      onclick={() => onCase(!caseSensitive)}>Aa</button>
    </div>
    <button disabled={!canStep} aria-label="Previous match" title="Previous match (Shift+Enter)" onclick={() => onStep(-1)}><svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="m4 10 4-4 4 4" /></svg></button>
    <button disabled={!canStep} aria-label="Next match" title="Next match (Enter)" onclick={() => onStep(1)}><svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="m4 6 4 4 4-4" /></svg></button>
    <button aria-label="Close find" title="Close find (Esc)" onclick={onClose}><svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="m4 4 8 8M4 12l8-8" /></svg></button>
  </div>
  <div class="find-meta">
    <span class="scope" title={scope}>{scope}</span>
    <span class="status" aria-live="polite" aria-atomic="true">{status}</span>
  </div>
</div>

<style>
  .find-bar { flex: none; min-width: 0; padding: 6px 8px; background: var(--bg); border-bottom: 1px solid var(--edge); color: var(--fg); }
  .find-controls { display: flex; align-items: center; gap: 4px; min-width: 0; }
  .field { display: flex; align-items: center; flex: 1; min-width: 0; border: 1px solid var(--edge); border-radius: 6px; background: var(--term-bg); }
  .field:focus-within { outline: 2px solid var(--focus-ring); outline-offset: -1px; }
  .search-icon { flex: none; margin-left: 8px; color: var(--muted); }
  input { width: 0; flex: 1; min-width: 0; height: 26px; border: 0; padding: 0 7px; color: var(--fg); background: transparent; font: inherit; font-size: var(--text-sm); outline: none; }
  button { display: inline-flex; align-items: center; justify-content: center; flex: none; border: 0; background: transparent; color: var(--muted); border-radius: 5px; width: var(--pane-control-size); height: var(--pane-control-size); cursor: pointer; }
  button:hover:not(:disabled) { background: var(--pane-control-hover); color: var(--fg); }
  button.enabled { background: var(--pane-control-selected); color: var(--accent); }
  button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: -2px; }
  button:disabled { opacity: .35; cursor: default; }
  .find-meta { display: flex; align-items: center; gap: 8px; margin-top: 4px; font-size: var(--text-xs); line-height: 16px; color: var(--muted); }
  .scope { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .status { flex: none; font-variant-numeric: tabular-nums; white-space: nowrap; }
</style>
