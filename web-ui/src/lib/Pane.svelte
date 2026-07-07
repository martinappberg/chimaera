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
    dropSpot: DropSpot | null;
    sessions: Map<string, Session>;
    names: Map<string, string>;
    fileNames: Map<string, string>;
    /** Active workspace root (touched-files paths relativize against it). */
    wsRoot: string | null;
    ctrl: LayoutCtrl;
  }

  let {
    node,
    focusedPaneId,
    zoomed = false,
    dropSpot,
    sessions,
    names,
    fileNames,
    wsRoot,
    ctrl,
  }: Props = $props();

  const focused = $derived(node.id === focusedPaneId);
  const activeTab = $derived(node.tabs[node.active] ?? null);
  /** Edge/center drop-zone preview for THIS pane, if a drag hovers it. */
  const zone = $derived(
    dropSpot?.kind === "zone" && dropSpot.paneId === node.id ? dropSpot.zone : null,
  );
  /** Context bridge: the "@ reference" band hovers over this pane's bottom. */
  const refBand = $derived(dropSpot?.kind === "ref" && dropSpot.paneId === node.id);

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
  <!-- Every pane always has its top bar — orientation, drag handle, and the
       mouse home for zoom/split/close, even single-pane single-tab. -->
  <PaneTabs {node} {zoomed} {sessions} {names} {fileNames} {wsRoot} {dropSpot} {ctrl} bind:el={tabbarEl} />
  <div class="content" bind:this={contentEl}>
    {#if activeTab !== null}
      {#if activeTab.surface === "terminal"}
        <TerminalView sessionId={activeTab.sessionId} {focused} fontSize={node.fontSize} />
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

  {#if zone !== null}
    <div class="drop drop-{zone}"></div>
  {/if}

  {#if refBand}
    <!-- Drag-to-reference: types the path into this session's input, never
         opens a tab, never submits. Visibly distinct from the adopt zone. -->
    <div class="drop-ref">
      <span class="drop-ref-label"><span class="drop-ref-at">@</span> reference</span>
    </div>
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

  /* Drag-to-reference band over the input area (~22%, matching the dnd
     hit-test): dashed + labeled so it can't be mistaken for adopt-as-tab. */
  .drop-ref {
    position: absolute;
    z-index: 7;
    inset: 78% 0 0 0;
    margin: 3px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: color-mix(in srgb, var(--accent) 18%, transparent);
    border: 1px dashed color-mix(in srgb, var(--accent) 60%, transparent);
    border-radius: 7px;
    pointer-events: none;
  }

  .drop-ref-label {
    font-family: var(--mono);
    font-size: var(--text-xs);
    letter-spacing: 0.06em;
    color: var(--fg);
    background: color-mix(in srgb, var(--term-bg) 82%, transparent);
    border-radius: 4px;
    padding: 2px 8px;
    user-select: none;
  }

  .drop-ref-at {
    color: var(--accent);
    font-weight: 600;
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
