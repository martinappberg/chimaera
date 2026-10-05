<script lang="ts">
  import { toolbarPopover } from "../shared/toolbarPopover";
  import { customModelSelection, isRealModel, modelChoice, type ModelChoice } from "./modelPicker";

  let { choices, currentId, allowCustomModel = false, onPick, onClose }: {
    choices: ModelChoice[];
    currentId: string | null;
    allowCustomModel?: boolean;
    onPick: (id: string) => boolean;
    onClose: () => void;
  } = $props();
  const uid = $props.id();
  let custom = $state(false);
  let draft = $state("");
  let error = $state<string | null>(null);
  let invalid = $state(false);
  const selected = $derived(modelChoice(choices, currentId));
  const unlisted = $derived(selected === undefined && isRealModel(currentId) ? currentId : null);

  function choose(id: string): void {
    if (onPick(id)) onClose();
    else error = "Model change was not sent. Try again when connected.";
  }
  function openCustom(): void {
    if (!draft && unlisted) draft = unlisted;
    error = null;
    custom = true;
  }
  function submit(event: SubmitEvent): void {
    event.preventDefault();
    if (!allowCustomModel) return;
    const selection = customModelSelection(draft);
    invalid = selection.id === null;
    if (selection.id === null) { error = selection.error; return; }
    choose(selection.id);
  }
</script>

{#key custom && allowCustomModel}
  <div class="overlay-surface model-menu" class:custom={custom && allowCustomModel} use:toolbarPopover={{ onClose, initialFocus: custom && allowCustomModel ? "input" : undefined }} role={custom && allowCustomModel ? "dialog" : "menu"} aria-label={custom && allowCustomModel ? "Custom model" : "Model"}>
    {#if custom && allowCustomModel}
      <form onsubmit={submit}>
        <div class="form-title">Custom model</div>
        <label for={`${uid}-model`}>Model ID</label>
        <input id={`${uid}-model`} bind:value={draft} type="text" spellcheck="false" autocapitalize="off" autocomplete="off" placeholder="Enter the exact model ID" aria-describedby={`${uid}-hint${error ? ` ${uid}-error` : ""}`} aria-invalid={invalid} oninput={() => { error = null; invalid = false; }} />
        <p id={`${uid}-hint`} class="hint">Uses this agent’s configured provider.</p>
        {#if error}<p id={`${uid}-error`} class="error" role="status">{error}</p>{/if}
        <div class="actions">
          <button class="opt quiet" type="button" onclick={() => { custom = false; error = null; }}>Back</button>
          <button class="opt primary" type="submit">Use model</button>
        </div>
      </form>
    {:else}
      {#if unlisted !== null}
        <button class="overlay-row model-row current" role="menuitemradio" aria-checked="true" onclick={onClose}>
          <span class="model-copy"><span class="raw-id">{unlisted}</span><span class="description">Current model</span></span>
          <span class="check" aria-hidden="true">✓</span>
        </button>
      {/if}
      {#if choices.length === 0 && unlisted === null}<span class="empty">No known models</span>{/if}
      {#each choices as model (model.id)}
        <button class="overlay-row model-row" class:current={selected?.id === model.id} role="menuitemradio" aria-checked={selected?.id === model.id} title={model.description ?? undefined} onclick={() => choose(model.id)}>
          <span class="model-copy"><span>{model.label}</span>{#if model.description}<span class="description">{model.description}</span>{/if}</span>
          {#if selected?.id === model.id}<span class="check" aria-hidden="true">✓</span>{/if}
        </button>
      {/each}
      {#if allowCustomModel}
        <div class="separator" role="separator"></div>
        <button class="overlay-row custom-entry" role="menuitem" onclick={openCustom}>Custom model…</button>
      {/if}
      {#if error}<p class="error menu-error" role="status">{error}</p>{/if}
    {/if}
  </div>
{/key}

<style>
  .model-menu { width: min(360px, calc(100vw - 32px)); --toolbar-menu-height: 480px; min-width: 180px; z-index: 20; overscroll-behavior: contain; white-space: normal; }
  .model-row { display: flex; align-items: flex-start; gap: 12px; white-space: normal; }
  .model-row.current, .check { color: var(--accent); }
  .model-copy { display: flex; flex: 1; min-width: 0; flex-direction: column; gap: 3px; text-align: left; overflow-wrap: anywhere; }
  .description { color: var(--muted); font-size: var(--text-xs); line-height: 1.4; }
  .raw-id { font-family: var(--mono); font-size: var(--text-xs); }
  .empty { display: block; padding: 6px 12px; color: var(--muted); font-size: var(--text-sm); }
  .separator { height: 1px; margin: 4px 8px; background: var(--edge); }
  .custom-entry { color: var(--muted); }
  .custom-entry:hover, .custom-entry:focus-visible { color: var(--fg); }
  .custom { padding: 14px; }
  form { display: flex; flex-direction: column; gap: 7px; }
  .form-title { margin-bottom: 5px; font-size: var(--text-sm); font-weight: 600; color: var(--fg); }
  label { font-size: var(--text-xs); color: var(--muted); }
  input { box-sizing: border-box; width: 100%; min-width: 0; padding: 7px 9px; border: 1px solid var(--edge); border-radius: 5px; background: var(--term-bg); color: var(--fg); font: inherit; font-family: var(--mono); font-size: var(--text-sm); }
  input::placeholder { color: var(--muted); font-family: var(--ui-font, sans-serif); }
  input[aria-invalid="true"] { border-color: var(--err); }
  .hint, .error { margin: 0; font-size: var(--text-xs); line-height: 1.5; overflow-wrap: anywhere; }
  .hint { color: var(--muted); }
  .error { color: var(--err); }
  .menu-error { padding: 6px 12px; }
  .actions { display: flex; justify-content: flex-end; gap: 7px; margin-top: 5px; }
  .actions button { min-height: 28px; }
  button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: -2px; }
  input:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 1px; }
</style>
