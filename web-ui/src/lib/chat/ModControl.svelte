<script lang="ts">
  import type { UiNode, UiRecord } from "./nativeUi";
  let { node, disabled = false, onAction }: { node: UiNode; disabled?: boolean; onAction: (node: UiNode, event: UiRecord) => Promise<void> } = $props();
  const controlId = $props.id();
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

<div class="control" class:has-error={error !== null} data-mod-key={String(node.props.key ?? "")} data-mod-plugin={node.press?.plugin}>
  {#if node.type === "Button"}
    <button type="button" class="opt" class:primary={node.props.variant === "primary"} class:plain={node.props.plain === true} class:quiet={node.props.dimColor === true} {disabled} aria-disabled={pending || disabled} aria-busy={pending} aria-describedby={error ? `${controlId}-error` : undefined} onclick={() => !pending && action({ type: "press" })}>
      <span class="button-label">{String(node.props.label ?? "Button")}</span>
      {#if pending}<span class="pending-mark" aria-hidden="true">…</span>{:else if typeof node.props.hotkey === "string"}<kbd>{node.props.hotkey}</kbd>{/if}
    </button>
  {:else if node.type === "Input"}
    <form onsubmit={submit}>
      <label>
        {#if node.props.label}<span>{String(node.props.label)}</span>{/if}
        <input {value} {disabled} maxlength="16000" placeholder={String(node.props.placeholder ?? "")} aria-label={String(node.props.label ?? node.props.placeholder ?? node.props.key ?? "Mod input")} aria-describedby={error ? `${controlId}-error` : undefined} aria-busy={pending} onfocus={() => (focused = true)} onblur={() => { flushInput(); focused = false; }} oninput={(event) => input(event.currentTarget.value)} />
      </label>
      {#if node.props.submitLabel}<button class="opt submit" type="submit" disabled={disabled || pending} aria-busy={pending}><span class="button-label">{String(node.props.submitLabel)}</span><span class="submit-key" aria-hidden="true">{pending ? "…" : "↵"}</span></button>{/if}
    </form>
  {:else}
    <label>
      {#if node.props.label}<span>{String(node.props.label)}</span>{/if}
      <select value={String(node.props.value ?? "")} disabled={disabled || pending} aria-label={String(node.props.label ?? node.props.key ?? "Mod selection")} aria-describedby={error ? `${controlId}-error` : undefined} aria-busy={pending} onchange={(event) => action({ type: "select", value: event.currentTarget.value })}>
        {#each options as option, i (i)}<option value={option.value}>{option.label ?? option.value}</option>{/each}
      </select>
    </label>
  {/if}
  {#if error}<span id={`${controlId}-error`} class="error" role="status">{error}</span>{/if}
</div>

<style>
  .control { min-width: 0; max-width: 100%; flex-shrink: 0; white-space: normal; }
  button, input, select { box-sizing: border-box; font: inherit; font-size: var(--text-sm); line-height: 1.4; min-height: 30px; max-width: 100%; }
  input, select { min-width: 0; width: 100%; color: var(--fg); background: color-mix(in srgb, var(--fg) 3%, var(--term-bg)); border: 1px solid var(--edge); border-radius: 5px; padding: 5px 8px; }
  input::placeholder { color: var(--muted); opacity: .8; }
  input:hover:not(:disabled), select:hover:not(:disabled) { border-color: color-mix(in srgb, var(--fg) 24%, var(--edge)); }
  input:focus, select:focus { border-color: var(--accent); background: var(--term-bg); }
  button { display: inline-flex; align-items: center; justify-content: center; gap: 8px; padding: 5px 10px; text-align: start; }
  button:hover:not(:disabled):not([aria-disabled="true"]) { background: var(--row-hover); border-color: color-mix(in srgb, var(--accent) 40%, var(--edge)); }
  button.primary:hover:not(:disabled):not([aria-disabled="true"]) { background: color-mix(in srgb, var(--accent) 24%, transparent); }
  button:active:not(:disabled):not([aria-disabled="true"]) { background: var(--row-active); }
  button.plain { border-color: transparent; background: transparent; padding-inline: 5px; }
  button[aria-busy="true"] { cursor: progress; }
  button:disabled, input:disabled, select:disabled { opacity: .5; cursor: default; }
  :is(button,input,select):focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
  label { display: flex; flex-direction: column; gap: 5px; min-width: 0; }
  label > span { font-size: var(--text-xs); font-weight: 500; line-height: 1.4; color: var(--muted); overflow-wrap: anywhere; }
  label:focus-within > span { color: var(--fg); }
  form { display: flex; flex-wrap: wrap; align-items: end; gap: 6px; }
  form label { flex: 1 1 12ch; }
  .submit { flex: 0 1 auto; }
  .button-label { min-width: 0; overflow-wrap: anywhere; }
  kbd { flex: none; font: inherit; font-size: var(--text-xs); line-height: 1; color: var(--muted); border: 1px solid color-mix(in srgb, var(--fg) 14%, transparent); border-radius: 3px; padding: 2px 4px; }
  .pending-mark, .submit-key { flex: none; color: var(--muted); }
  .has-error input, .has-error select { border-color: color-mix(in srgb, var(--err) 55%, var(--edge)); }
  .error { display: block; margin-top: 5px; font-size: var(--text-xs); line-height: 1.5; overflow-wrap: anywhere; color: var(--err); }
</style>
