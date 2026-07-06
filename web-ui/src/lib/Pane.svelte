<script lang="ts">
  import type { PaneNode } from "./layout";
  import type { Session } from "./sessions";
  import type { DropSpot, LayoutCtrl } from "./dnd";
  import { registerPane, unregisterPane } from "./dnd";
  import { KEYS, MOD_LABEL } from "./keys";
  import PaneTabs from "./PaneTabs.svelte";
  import TerminalView from "./Terminal.svelte";
  import FileView from "./FileView.svelte";

  interface Props {
    node: PaneNode;
    focusedPaneId: string;
    /** True when this pane is rendered zoomed (fullscreen in the window). */
    zoomed?: boolean;
    /** Show the tab bar even for 0/1 tabs (any multi-pane layout). */
    forceTabs?: boolean;
    dropSpot: DropSpot | null;
    sessions: Map<string, Session>;
    names: Map<string, string>;
    fileNames: Map<string, string>;
    ctrl: LayoutCtrl;
  }

  let {
    node,
    focusedPaneId,
    zoomed = false,
    forceTabs = false,
    dropSpot,
    sessions,
    names,
    fileNames,
    ctrl,
  }: Props = $props();

  const focused = $derived(node.id === focusedPaneId);
  const activeTab = $derived(node.tabs[node.active] ?? null);
  /** Edge/center drop-zone preview for THIS pane, if a drag hovers it. */
  const zone = $derived(
    dropSpot?.kind === "zone" && dropSpot.paneId === node.id ? dropSpot.zone : null,
  );

  let rootEl = $state<HTMLElement | null>(null);
  let contentEl = $state<HTMLDivElement | null>(null);
  let tabbarEl = $state<HTMLElement | null>(null);

  // Register this pane's geometry with the dnd hit-tester.
  $effect(() => {
    const root = rootEl;
    const content = contentEl;
    if (root === null || content === null) return;
    registerPane(node.id, { root, content, tabbar: tabbarEl });
    return () => unregisterPane(node.id, root);
  });
</script>

<section
  class="pane"
  class:focused
  tabindex="-1"
  bind:this={rootEl}
  onpointerdowncapture={() => ctrl.focusPane(node.id)}
>
  {#if forceTabs || node.tabs.length > 1}
    <PaneTabs {node} {sessions} {names} {fileNames} {dropSpot} {ctrl} bind:el={tabbarEl} />
  {/if}
  <div class="content" bind:this={contentEl}>
    {#if activeTab !== null}
      {#if activeTab.surface === "terminal"}
        <TerminalView sessionId={activeTab.sessionId} {focused} />
      {:else}
        <FileView path={activeTab.path} />
      {/if}
    {:else if names.size === 0}
      <!-- No sessions to open or drag yet: point at creating one. -->
      <div class="hint">
        <span><kbd>{KEYS.newAgent}</kbd> new agent</span>
        <span class="hint-sep">·</span>
        <span><kbd>{KEYS.newTerminal}</kbd> new terminal</span>
      </div>
    {:else}
      <div class="hint">
        <span><kbd>{MOD_LABEL}1–9</kbd> opens a session</span>
        <span class="hint-sep">·</span>
        <span>drag one here</span>
      </div>
    {/if}
  </div>

  <!-- Hover control cluster: the mouse path to every pane chord. -->
  <div class="controls">
    <button
      class="ctl"
      title="split right ({KEYS.splitRight})"
      aria-label="split right"
      onclick={() => ctrl.splitPaneAt(node.id, "row")}
    >
      <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
        <rect x="2" y="3" width="12" height="10" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.3" />
        <line x1="8" y1="3" x2="8" y2="13" stroke="currentColor" stroke-width="1.3" />
      </svg>
    </button>
    <button
      class="ctl"
      title="split down ({KEYS.splitDown})"
      aria-label="split down"
      onclick={() => ctrl.splitPaneAt(node.id, "col")}
    >
      <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
        <rect x="2" y="3" width="12" height="10" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.3" />
        <line x1="2" y1="8" x2="14" y2="8" stroke="currentColor" stroke-width="1.3" />
      </svg>
    </button>
    <button
      class="ctl"
      title="{zoomed ? 'exit zoom' : 'zoom'} ({KEYS.zoom})"
      aria-label={zoomed ? "exit zoom" : "zoom"}
      onclick={() => ctrl.zoomPane(node.id)}
    >
      <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
        <path d="M9.5 2.5h4v4M6.5 13.5h-4v-4M13.5 2.5L9 7M2.5 13.5L7 9" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
      </svg>
    </button>
    <button
      class="ctl"
      title="close view ({KEYS.closeView})"
      aria-label="close view"
      onclick={() => ctrl.closeView(node.id)}
    >
      <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
        <path d="M4 4l8 8M12 4l-8 8" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
      </svg>
    </button>
  </div>

  {#if zoomed}
    <button class="zoom-badge" title="exit zoom ({KEYS.zoom})" onclick={() => ctrl.zoomPane(node.id)}
      >zoom</button
    >
  {/if}
  {#if zone !== null}
    <div class="drop drop-{zone}"></div>
  {/if}
</section>

<style>
  .pane {
    flex: 1;
    min-width: 0;
    min-height: 0;
    position: relative;
    display: flex;
    flex-direction: column;
    background: var(--term-bg);
    border: 1px solid var(--edge);
    border-radius: 10px;
    overflow: hidden;
    transition: border-color 0.12s ease;
    outline: none;
  }

  /* The focused pane is unmistakable: hairline accent instead of the edge. */
  .pane.focused {
    border-color: color-mix(in srgb, var(--accent) 62%, var(--edge));
  }

  .content {
    flex: 1;
    position: relative;
    min-height: 0;
    min-width: 0;
  }

  .hint {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.45rem;
    color: var(--muted);
    font-size: var(--text-sm);
    user-select: none;
  }

  .hint kbd {
    font-family: var(--mono);
    font-size: var(--text-xs);
    padding: 0 0.25rem;
    border: 1px solid var(--edge);
    border-radius: 4px;
    background: none;
  }

  .hint-sep {
    opacity: 0.5;
  }

  /* Hover control cluster: quiet chip, top-right, 0.12s fade. */
  .controls {
    position: absolute;
    top: 4px;
    right: 6px;
    z-index: 8;
    display: flex;
    gap: 1px;
    padding: 1px;
    border-radius: 5px;
    background: color-mix(in srgb, var(--fg) 6%, var(--term-bg));
    border: 1px solid var(--edge);
    opacity: 0;
    pointer-events: none;
    transition: opacity 0.12s ease;
  }

  .pane:hover .controls,
  .controls:focus-within {
    opacity: 1;
    pointer-events: auto;
  }

  .ctl {
    appearance: none;
    border: none;
    background: none;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 18px;
    padding: 0;
    border-radius: 4px;
    color: var(--muted);
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .ctl:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .zoom-badge {
    position: absolute;
    right: 10px;
    bottom: 8px;
    z-index: 7;
    appearance: none;
    border: none;
    font: inherit;
    font-size: var(--text-xs);
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--muted);
    background: color-mix(in srgb, var(--fg) 7%, transparent);
    padding: 1px 7px;
    border-radius: 4px;
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .zoom-badge:hover {
    background: color-mix(in srgb, var(--fg) 12%, transparent);
    color: var(--fg);
  }

  /* Translucent drop-zone preview showing exactly where the drop lands. */
  .drop {
    position: absolute;
    z-index: 6;
    margin: 3px;
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 42%, transparent);
    border-radius: 7px;
    pointer-events: none;
  }

  .drop-center {
    inset: 0;
  }
  .drop-left {
    inset: 0 50% 0 0;
  }
  .drop-right {
    inset: 0 0 0 50%;
  }
  .drop-top {
    inset: 0 0 50% 0;
  }
  .drop-bottom {
    inset: 50% 0 0 0;
  }
</style>
