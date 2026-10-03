<script lang="ts">
  import type { UiNode, UiRecord } from "./nativeUi";
  let { node, disabled = false, onAction }: { node: UiNode; disabled?: boolean; onAction: (node: UiNode, event: UiRecord) => Promise<void> } = $props();
  let value = $state("");
  let pending = $state(false);
  let focused = $state(false);
  let error = $state<string | null>(null);
  let chain = Promise.resolve();
  let actions = 0;
  let editTimer = $state<ReturnType<typeof setTimeout> | null>(null);
  let serverValue = $state("");
  let serverRevision = $state(0);
  let editedRevision = $state(-1);
  let lastServerValue: string | undefined;
  let revision = 0;
  $effect(() => {
    const next = String(node.props.value ?? "");
    if (next !== lastServerValue) { lastServerValue = next; serverValue = next; serverRevision = ++revision; }
  });
  // A skipped server rewrite still applies when the field yields focus or its
  // last request settles. An old render never overwrites an unsent local edit.
  $effect(() => { if (!focused && !pending && editTimer === null && serverRevision > editedRevision) value = serverValue; });
  $effect(() => () => { if (editTimer) clearTimeout(editTimer); });
  function action(event: UiRecord): Promise<void> {
    if (actions >= 16) { error = "This control is busy. Wait for it to finish, then try again."; return Promise.resolve(); }
    actions++;
    pending = true;
    error = null;
    chain = chain.then(() => onAction(node, event)).catch((reason) => { error = reason instanceof Error ? reason.message : String(reason); }).finally(() => { actions--; pending = actions > 0; });
    return chain;
  }
  function input(next: string): void {
    value = next.slice(0, 16_000);
    editedRevision = serverRevision;
    const edited = value;
    if (editTimer) clearTimeout(editTimer);
    editTimer = setTimeout(() => { editTimer = null; void action({ type: "input", kind: "change", value: edited }); }, 80);
  }
  function flushInput(): void {
    if (editTimer) { clearTimeout(editTimer); editTimer = null; void action({ type: "input", kind: "change", value }); }
  }
  function submit(event: SubmitEvent): void {
    event.preventDefault();
    flushInput();
    void action({ type: "input", kind: "submit", value });
  }
  const options = $derived(Array.isArray(node.props.options) ? node.props.options.filter((option): option is { value: string; label?: string } => typeof option === "object" && option !== null && typeof option.value === "string") : []);
</script>

<div class="control" data-mod-key={String(node.props.key ?? "")} data-mod-plugin={node.press?.plugin}>
  {#if node.type === "Button"}
    <button class:primary={node.props.variant === "primary"} class:plain={node.props.plain === true} {disabled} aria-busy={pending} onclick={() => !pending && action({ type: "press" })}>
      {String(node.props.label ?? "Button")}
      {#if typeof node.props.hotkey === "string"}<kbd>{node.props.hotkey}</kbd>{/if}
    </button>
  {:else if node.type === "Input"}
    <form onsubmit={submit}>
      <label>
        {#if node.props.label}<span>{String(node.props.label)}</span>{/if}
        <input {value} {disabled} maxlength="16000" placeholder={String(node.props.placeholder ?? "")} aria-label={String(node.props.label ?? node.props.placeholder ?? node.props.key ?? "Mod input")} onfocus={() => (focused = true)} onblur={() => { flushInput(); focused = false; }} oninput={(event) => input(event.currentTarget.value)} />
      </label>
      {#if node.props.submitLabel}<button type="submit" disabled={disabled || pending}>{String(node.props.submitLabel)}</button>{/if}
    </form>
  {:else}
    <label>
      {#if node.props.label}<span>{String(node.props.label)}</span>{/if}
      <select value={String(node.props.value ?? "")} disabled={disabled || pending} aria-label={String(node.props.label ?? node.props.key ?? "Mod selection")} onchange={(event) => action({ type: "select", value: event.currentTarget.value })}>
        {#each options as option, i (i)}<option value={option.value}>{option.label ?? option.value}</option>{/each}
      </select>
    </label>
  {/if}
  {#if error}<span class="error" role="status">{error}</span>{/if}
</div>

<style>
  .control { min-width: 0; max-width: 100%; white-space: normal; }
  button, input, select { font: inherit; font-size: var(--text-sm); line-height: 1.4; color: var(--fg); background: var(--term-bg); border: 1px solid var(--edge); border-radius: 5px; padding: 5px 8px; min-height: 28px; max-width: 100%; }
  button { cursor: pointer; display: inline-flex; align-items: center; gap: 8px; background: var(--bg); }
  button:hover:not(:disabled) { background: var(--row-hover); border-color: color-mix(in srgb, var(--accent) 40%, var(--edge)); }
  button.primary { color: var(--accent); background: color-mix(in srgb, var(--accent) 8%, var(--term-bg)); border-color: color-mix(in srgb, var(--accent) 40%, var(--edge)); }
  button.plain { border-color: transparent; background: transparent; }
  button:disabled, input:disabled, select:disabled { opacity: .5; cursor: default; }
  :is(button,input,select):focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
  label { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
  label > span { font-size: var(--text-xs); color: var(--muted); }
  form { display: flex; align-items: end; gap: .5em; }
  form label { flex: 1; }
  kbd { font: inherit; font-size: .85em; color: var(--muted); }
  .error { display: block; margin-top: .3em; font-size: .9em; color: var(--err); }
</style>
