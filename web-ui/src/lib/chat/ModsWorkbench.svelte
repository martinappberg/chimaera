<script lang="ts">
  import { tick, untrack } from "svelte";
  import { pageVisible } from "../shared/visibility";
  import { tabNavigation } from "../shared/tabNavigation";
  import ExtensionsGlyph from "../plugins/ExtensionsGlyph.svelte";
  import { modsFor } from "./mods.svelte";
  import type { NativeUiTransport } from "./nativeUi";
  import ModSite from "./ModSite.svelte";
  import type { NativeComposer } from "./nativeComposer";
  let { transport, visible, canFocus, canEdit, focused, composer, running, hasSurvey, host, onDockWidth }: { transport: NativeUiTransport; visible: boolean; canFocus: boolean; canEdit: boolean; focused: boolean; composer?: NativeComposer; running: boolean; hasSurvey: boolean; host: HTMLElement | null; onDockWidth: (width: number) => void } = $props();
  const uid = $props.id();
  const mods = $derived(modsFor(transport));
  const active = $derived(visible && $pageVisible);
  const pane = $derived(mods.panes.find((item) => item.id === mods.shown));
  let paneBody = $state<HTMLDivElement>();
  let expanded = $state(false);
  let minimized = $state(false);
  let hostWidth = $state(0);
  let hostHeight = $state(0);
  let lastPane: string | null = null;
  const docked = $derived(hostWidth >= 960 && !!pane && !minimized);
  const dockWidth = $derived(docked ? Math.round(Math.min(460, Math.max(320, hostWidth * .34))) : 0);
  const inlineHeight = $derived(Math.max(48, Math.floor(hostHeight * (expanded ? .58 : .34))));
  $effect(() => {
    if (!host || !active) return;
    const measure = () => {
      // Padding reserves the dock's space; its content-box must not set the breakpoint.
      const box = host!.getBoundingClientRect();
      hostWidth = box.width; hostHeight = box.height;
    };
    const observer = new ResizeObserver(measure);
    observer.observe(host, { box: "border-box" }); measure();
    return () => observer.disconnect();
  });
  $effect(() => { onDockWidth(dockWidth); });
  $effect(() => {
    const id = pane?.id ?? null;
    if (id !== lastPane) { lastPane = id; minimized = false; }
  });
  $effect(() => { if (active) return mods.retain(); });
  $effect(() => { if (active) return mods.registerHost(() => ({ composer, canEdit, focused })); });
  $effect(() => {
    const requested = mods.focusRequested;
    if (!active || !mods.attached || !requested) return;
    untrack(() => {
      if (!canFocus) { void mods.paneAction("ui_pane_focus", null); return; }
      minimized = false;
      void tick().then(() => {
        // Recheck after layout: a draft or dialog may have taken the keyboard meanwhile.
        if (!active || !mods.attached || !canFocus || pane?.id !== requested || mods.focusRequested !== requested) { void mods.paneAction("ui_pane_focus", null); return; }
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
  function choosePane(id: string): void {
    minimized = false;
    if (mods.shown !== id) void mods.paneAction("ui_pane_show", id);
  }
  function minimize(): void {
    // The native protocol has close, but no hide. Keep its pane and state alive.
    minimized = true;
    void mods.paneAction("ui_pane_focus", null);
  }
</script>

<div class="mods-workbench">
  {#if mods.error}
    <div class="error" role="status"><span>Claude Mods: {mods.error}</span><button onclick={() => mods.attach()}>Reconnect</button></div>
  {/if}
  {#if mods.panes.length}
    <section class="pane-shell" class:docked class:minimized style:width={docked ? `${dockWidth}px` : undefined} style:--mod-pane-height={`${inlineHeight}px`} aria-label="Claude Mod panes">
      <div class="pane-header">
        <span class="mod-label" title="Claude Code Mods"><ExtensionsGlyph /></span>
        <div class="tabs" role="tablist" aria-label="Mod panes" use:tabNavigation>
          {#each mods.panes as item, index (index)}
            <button role="tab" id={`${uid}-tab-${index}`} aria-controls={`${uid}-body`} aria-selected={mods.shown === item.id} tabindex={mods.shown === item.id || (!pane && index === 0) ? 0 : -1} data-tab-index={index} class:selected={mods.shown === item.id} title={`${item.title} · ${item.plugin}`} onclick={() => choosePane(item.id)}>{item.title}</button>
          {/each}
        </div>
        {#if pane}
          <button class="icon-button" aria-label={minimized ? "Restore Mod pane" : "Minimize Mod pane"} title={minimized ? "Restore pane" : "Minimize · keep this Mod open"} aria-expanded={!minimized} aria-controls={`${uid}-body`} onclick={() => minimized ? (minimized = false) : minimize()}>
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d={minimized ? "m6 15 6-6 6 6" : "m6 9 6 6 6-6"}/></svg>
          </button>
          {#if !docked && !minimized}
          <button class="icon-button" aria-label={expanded ? "Restore Mod pane size" : "Expand Mod pane"} title={expanded ? "Restore pane size" : "Expand pane"} aria-pressed={expanded} onclick={() => (expanded = !expanded)}>
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d={expanded ? "M4 9h5V4m6 0v5h5M4 15h5v5m6 0v-5h5" : "M9 4H4v5m11-5h5v5M4 15v5h5m6 0h5v-5"}/></svg>
          </button>
          {/if}
          <button class="icon-button close" aria-label={`Close ${pane.title}`} title="Close pane" onclick={() => mods.paneAction("ui_close", pane.id)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="m6 6 12 12M6 18 18 6"/></svg></button>
        {/if}
      </div>
      {#if pane}
        <!-- Focus stays inside the pane only after a deliberate choice or an idle-composer request. -->
        <div class="pane-body" id={`${uid}-body`} hidden={minimized} role="tabpanel" aria-labelledby={`${uid}-tab-${mods.panes.indexOf(pane)}`} tabindex="-1" bind:this={paneBody} onkeydown={keydown} onfocusin={() => mods.focused !== pane.id && mods.paneAction("ui_pane_focus", pane.id)} onfocusout={paneFocusOut}>
          <ModSite {mods} component="Pane" instanceId={pane.id} props={{ title: pane.title, placement: docked ? "dock" : "inline" }} {active} suspended={minimized} />
        </div>
      {/if}
    </section>
  {/if}
  <div class="above-prompt"><ModSite {mods} component="AbovePrompt" instanceId="above-prompt" props={{ isWorking: running, hasSurvey }} {active} /></div>
  {#if mods.statuses.length}<div class="statuses">{#each mods.statuses as status, index (index)}<span title={status.plugin}>{status.text}</span>{/each}</div>{/if}
  {#if mods.toast && (minimized || !pane?.hold_toasts)}<div class="toast" role="status"><span title={mods.toast.plugin}>{mods.toast.text}</span><button aria-label="Dismiss Mod notification" onclick={() => (mods.toast = null)}>×</button></div>{/if}
</div>

<style>
  .mods-workbench { display: flex; flex-direction: column; width: min(var(--chat-column), calc(100% - 40px)); margin: 0 auto; flex: 0 1 auto; min-width: 0; min-height: 0; font-size: var(--text-md); line-height: 1.45; }
  .pane-shell { display: flex; flex-direction: column; min-height: 0; border: 1px solid var(--edge); border-radius: 7px; margin: 8px 0; background: var(--term-bg); overflow: hidden; }
  .pane-shell.docked { position: absolute; top: 0; right: 0; bottom: 0; margin: 0; border-width: 0 0 0 1px; border-radius: 0; display: flex; flex-direction: column; }
  .pane-header { display: flex; flex: none; align-items: center; gap: 6px; padding: 0 6px 0 12px; min-height: 34px; background: var(--bg); border-bottom: 1px solid var(--edge); }
  .mod-label { color: var(--muted); display: grid; place-items: center; flex: none; }
  .tabs { display: flex; flex: 1; gap: 4px; min-width: 0; overflow-x: auto; }
  button { font: inherit; color: var(--muted); background: none; border: 0; padding: 5px 9px; border-radius: 5px; cursor: pointer; }
  button:hover { background: var(--row-hover); color: var(--fg); }
  button.selected { color: var(--fg); box-shadow: inset 0 -2px var(--accent); }
  button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: -2px; }
  .tabs button { white-space: nowrap; font-family: var(--mono); font-size: var(--text-xs); border-radius: 0; min-height: 34px; }
  .icon-button { display: grid; place-items: center; flex: none; padding: 5px; width: 26px; height: 26px; }
  .pane-body { display: flex; min-height: 0; outline: none; }
  .pane-body :global(.mod-site.pane) { flex: 1; min-height: 0; box-sizing: border-box; }
  .pane-body[hidden] { display: none; }
  .docked .pane-body { display: flex; flex: 1; min-height: 0; }
  .docked .pane-body :global(.mod-site.pane) { flex: 1; min-height: 0; max-height: none; padding: 16px; }
  .minimized .pane-header { border-bottom: 0; }
  .pane-body:focus-visible { box-shadow: inset 0 0 0 2px var(--focus-ring); }
  .above-prompt:empty { display: none; }
  .above-prompt { flex: none; font-size: var(--text-md); }
  .statuses { display: flex; flex: none; flex-wrap: wrap; gap: 8px; color: var(--muted); font-size: 11px; margin: 4px 0; }
  .toast, .error { display: flex; align-items: center; gap: 8px; padding: 7px 10px; border: 1px solid var(--edge); border-radius: 7px; font-size: 12px; margin: 5px 0; }
  .toast span, .error span { flex: 1; min-width: 0; overflow-wrap: anywhere; }
  .error button { color: var(--accent); }
</style>
