<script lang="ts">
  import type { PaneNode } from "./layout";
  import type { Session } from "./sessions";
  import type { DropSpot, LayoutCtrl } from "./dnd";
  import { MOD_LABEL, registerPane, unregisterPane } from "./dnd";
  import PaneTabs from "./PaneTabs.svelte";
  import TerminalView from "./Terminal.svelte";

  interface Props {
    node: PaneNode;
    focusedPaneId: string;
    /** True when this pane is rendered zoomed (fullscreen in the window). */
    zoomed?: boolean;
    dropSpot: DropSpot | null;
    sessions: Map<string, Session>;
    names: Map<string, string>;
    ctrl: LayoutCtrl;
  }

  let { node, focusedPaneId, zoomed = false, dropSpot, sessions, names, ctrl }: Props = $props();

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
  bind:this={rootEl}
  onpointerdowncapture={() => ctrl.focusPane(node.id)}
>
  {#if node.tabs.length > 1}
    <PaneTabs {node} {sessions} {names} {dropSpot} {ctrl} bind:el={tabbarEl} />
  {/if}
  <div class="content" bind:this={contentEl}>
    {#if activeTab !== null}
      <TerminalView sessionId={activeTab.sessionId} {focused} />
    {:else}
      <div class="hint">
        <span><kbd>{MOD_LABEL}1–9</kbd> opens a session</span>
        <span class="hint-sep">·</span>
        <span>drag one here</span>
      </div>
    {/if}
  </div>
  {#if zoomed}
    <span class="zoom-badge">zoom</span>
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
    font-size: 0.78rem;
    user-select: none;
  }

  .hint kbd {
    font-family: var(--mono);
    font-size: 0.72rem;
    padding: 0 0.25rem;
    border: 1px solid var(--edge);
    border-radius: 4px;
    background: none;
  }

  .hint-sep {
    opacity: 0.5;
  }

  .zoom-badge {
    position: absolute;
    right: 10px;
    bottom: 8px;
    z-index: 7;
    font-size: 0.6rem;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--muted);
    background: color-mix(in srgb, var(--fg) 7%, transparent);
    padding: 1px 7px;
    border-radius: 4px;
    pointer-events: none;
  }

  /* Translucent drop-zone preview showing exactly where the drop lands. */
  .drop {
    position: absolute;
    z-index: 6;
    margin: 3px;
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 42%, transparent);
    border-radius: 8px;
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
