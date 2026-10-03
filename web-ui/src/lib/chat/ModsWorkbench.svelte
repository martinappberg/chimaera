<script lang="ts">
  import { tick, untrack } from "svelte";
  import { pageVisible } from "../shared/visibility";
  import { modsFor } from "./mods.svelte";
  import type { NativeUiTransport } from "./nativeUi";
  import ModSite from "./ModSite.svelte";
  import type { NativeComposer } from "./nativeComposer";
  let { transport, visible, canFocus, canEdit, focused, composer, running, hasSurvey }: { transport: NativeUiTransport; visible: boolean; canFocus: boolean; canEdit: boolean; focused: boolean; composer?: NativeComposer; running: boolean; hasSurvey: boolean } = $props();
  const mods = $derived(modsFor(transport));
  const active = $derived(visible && $pageVisible);
  const pane = $derived(mods.panes.find((item) => item.id === mods.shown));
  let paneBody = $state<HTMLDivElement>();
  let expanded = $state(false);
  $effect(() => { if (active) return mods.retain(); });
  $effect(() => { if (active) return mods.registerHost(() => ({ composer, canEdit, focused })); });
  $effect(() => {
    const requested = mods.focusRequested;
    if (!active || !requested) return;
    untrack(() => {
      if (!canFocus) { void mods.paneAction("ui_pane_focus", null); return; }
      void tick().then(() => {
        // Recheck after layout: a draft or dialog may have taken the keyboard meanwhile.
        if (!active || !canFocus || pane?.id !== requested) { void mods.paneAction("ui_pane_focus", null); return; }
        const site = paneBody?.querySelector<HTMLElement>(".mod-site");
        site?.focus({ preventScroll: true });
        if (site?.contains(document.activeElement)) void mods.paneAction("ui_pane_focus", requested);
      });
    });
  });
  function keydown(event: KeyboardEvent): void {
    if (event.key === "Escape" && pane?.close_on_escape) {
      event.preventDefault(); event.stopPropagation();
      void mods.paneAction("ui_close", pane.id);
    }
  }
  function paneFocusOut(event: FocusEvent): void {
    if (event.relatedTarget instanceof Node && paneBody?.contains(event.relatedTarget)) return;
    if (active && mods.attached) void mods.paneAction("ui_pane_focus", null);
  }
</script>

<div class="mods-workbench">
  {#if mods.error}
    <div class="error" role="status"><span>Claude Mods: {mods.error}</span><button onclick={() => mods.attach()}>Reconnect</button></div>
  {/if}
  {#if mods.panes.length}
    <section class="pane-shell" class:expanded aria-label="Claude Mod panes">
      <div class="pane-header">
        <span class="mod-label" title="Interactive panes from your Claude Code Mods">Mods</span>
        <div class="tabs" role="tablist" aria-label="Mod panes">
          {#each mods.panes as item, index (index)}
            <button role="tab" aria-selected={mods.shown === item.id} class:selected={mods.shown === item.id} title={`${item.title} · ${item.plugin}`} onclick={() => mods.paneAction("ui_pane_show", item.id)}>{item.title}</button>
          {/each}
        </div>
        {#if pane}
          <button class="icon-button" aria-label={expanded ? "Restore Mod pane size" : "Expand Mod pane"} title={expanded ? "Restore pane size" : "Expand pane"} aria-pressed={expanded} onclick={() => (expanded = !expanded)}>
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d={expanded ? "M4 9h5V4m6 0v5h5M4 15h5v5m6 0v-5h5" : "M9 4H4v5m11-5h5v5M4 15v5h5m6 0h5v-5"}/></svg>
          </button>
          <button class="icon-button close" aria-label={`Close ${pane.title}`} title="Close pane" onclick={() => mods.paneAction("ui_close", pane.id)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="m6 6 12 12M6 18 18 6"/></svg></button>
        {/if}
      </div>
      {#if pane}
        <!-- Focus stays inside the pane only after a deliberate choice or an idle-composer request. -->
        <div class="pane-body" role="tabpanel" aria-label={pane.title} tabindex="-1" bind:this={paneBody} onkeydown={keydown} onfocusin={() => mods.focused !== pane.id && mods.paneAction("ui_pane_focus", pane.id)} onfocusout={paneFocusOut}>
          <ModSite {mods} component="Pane" instanceId={pane.id} props={{ title: pane.title }} {active} />
        </div>
      {/if}
    </section>
  {/if}
  <div class="above-prompt"><ModSite {mods} component="AbovePrompt" instanceId="above-prompt" props={{ isWorking: running, hasSurvey }} {active} /></div>
  {#if mods.statuses.length}<div class="statuses">{#each mods.statuses as status, index (index)}<span title={status.plugin}>{status.text}</span>{/each}</div>{/if}
  {#if mods.toast && !pane?.hold_toasts}<div class="toast" role="status"><span title={mods.toast.plugin}>{mods.toast.text}</span><button aria-label="Dismiss Mod notification" onclick={() => (mods.toast = null)}>×</button></div>{/if}
</div>

<style>
  .mods-workbench { width: min(var(--chat-column), calc(100% - 40px)); margin: 0 auto; flex: 0 1 auto; min-width: 0; min-height: 0; font-size: var(--text-md); line-height: 1.45; }
  .pane-shell { border: 1px solid var(--edge); border-radius: 7px; margin: 8px 0; background: var(--term-bg); overflow: hidden; --mod-pane-height: 34vh; }
  .pane-shell.expanded { --mod-pane-height: 58vh; }
  .pane-header { display: flex; align-items: center; gap: 6px; padding: 0 6px 0 12px; min-height: 34px; background: var(--bg); border-bottom: 1px solid var(--edge); }
  .mod-label { color: var(--muted); font-family: var(--mono); font-size: var(--text-xs); padding-right: 6px; }
  .tabs { display: flex; flex: 1; gap: 4px; min-width: 0; overflow-x: auto; }
  button { font: inherit; color: var(--muted); background: none; border: 0; padding: 5px 9px; border-radius: 5px; cursor: pointer; }
  button:hover { background: var(--row-hover); color: var(--fg); }
  button.selected { color: var(--fg); box-shadow: inset 0 -2px var(--accent); }
  button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: -2px; }
  .tabs button { white-space: nowrap; font-family: var(--mono); font-size: var(--text-xs); border-radius: 0; min-height: 34px; }
  .icon-button { display: grid; place-items: center; flex: none; padding: 5px; width: 26px; height: 26px; }
  .pane-body { min-height: 42px; outline: none; }
  .pane-body:focus-visible { box-shadow: inset 0 0 0 2px var(--focus-ring); }
  .above-prompt:empty { display: none; }
  .above-prompt { font-size: var(--text-md); }
  .statuses { display: flex; flex-wrap: wrap; gap: 8px; color: var(--muted); font-size: 11px; margin: 4px 0; }
  .toast, .error { display: flex; align-items: center; gap: 8px; padding: 7px 10px; border: 1px solid var(--edge); border-radius: 7px; font-size: 12px; margin: 5px 0; }
  .toast span, .error span { flex: 1; min-width: 0; overflow-wrap: anywhere; }
  .error button { color: var(--accent); }
</style>
