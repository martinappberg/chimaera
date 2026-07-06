<script lang="ts">
  import type { PaneNode } from "./layout";
  import type { Session } from "./sessions";
  import { dotState } from "./sessions";
  import type { DropSpot, LayoutCtrl } from "./dnd";

  interface Props {
    node: PaneNode;
    sessions: Map<string, Session>;
    names: Map<string, string>;
    dropSpot: DropSpot | null;
    ctrl: LayoutCtrl;
    /** Bound by Pane so the dnd hit-tester can target this bar. */
    el?: HTMLElement | null;
  }

  let { node, sessions, names, dropSpot, ctrl, el = $bindable(null) }: Props = $props();

  /** Insertion index while a drag hovers this tab bar, else null. */
  const insertIndex = $derived(
    dropSpot?.kind === "tab" && dropSpot.paneId === node.id ? dropSpot.index : null,
  );

  function label(sessionId: string): string {
    return names.get(sessionId) ?? sessions.get(sessionId)?.name ?? sessionId.slice(0, 8);
  }
</script>

<div class="tabs" role="tablist" bind:this={el}>
  {#each node.tabs as tab, i (tab.sessionId)}
    {@const s = sessions.get(tab.sessionId)}
    <div
      class="tab"
      class:active={i === node.active}
      class:insert={insertIndex === i}
      role="tab"
      aria-selected={i === node.active}
      tabindex="-1"
      data-tab-index={i}
      title={label(tab.sessionId)}
      onpointerdowncapture={(e) => {
        // Capture-phase (directly attached, not delegated); ignore presses
        // on the close button so it stays a plain click.
        if (e.target instanceof Element && e.target.closest(".tab-close")) return;
        ctrl.dragTab(e, node.id, i, tab.sessionId);
      }}
      onauxclick={(e) => {
        // Middle-click closes the tab (detaches the view, never the session).
        if (e.button === 1) {
          e.preventDefault();
          ctrl.closeTab(node.id, i);
        }
      }}
    >
      <span class="dot {s ? dotState(s) : ''}"></span>
      <span class="tab-name">{label(tab.sessionId)}</span>
      <button
        class="tab-close"
        aria-label="close tab"
        title="close tab"
        onclick={(e) => {
          e.stopPropagation();
          ctrl.closeTab(node.id, i);
        }}>&times;</button
      >
    </div>
  {/each}
  <div class="tab-tail" class:insert={insertIndex === node.tabs.length}></div>
</div>

<style>
  .tabs {
    flex: none;
    display: flex;
    align-items: stretch;
    height: 30px;
    overflow: hidden;
    border-bottom: 1px solid var(--edge);
    padding: 0 4px;
  }

  .tab {
    position: relative;
    display: flex;
    align-items: center;
    gap: 0.45rem;
    padding: 0 0.4rem 0 0.6rem;
    max-width: 200px;
    min-width: 0;
    font-family: var(--mono);
    font-size: 0.72rem;
    color: var(--muted);
    cursor: default;
    user-select: none;
  }

  .tab:hover {
    color: var(--fg);
  }

  .tab.active {
    color: var(--fg);
  }

  /* Quiet active marker: a short accent underline, no filled chrome. */
  .tab.active::after {
    content: "";
    position: absolute;
    left: 0.55rem;
    right: 0.55rem;
    bottom: -1px;
    height: 2px;
    border-radius: 1px;
    background: var(--accent);
  }

  /* Drag insertion caret. */
  .tab.insert::before,
  .tab-tail.insert::before {
    content: "";
    position: absolute;
    top: 5px;
    bottom: 5px;
    left: -1px;
    width: 2px;
    border-radius: 1px;
    background: var(--accent);
  }

  .tab-tail {
    position: relative;
    flex: 1;
    min-width: 8px;
  }

  .tab-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tab-close {
    appearance: none;
    border: none;
    background: none;
    padding: 0 0.15rem;
    font: inherit;
    font-size: 0.85rem;
    line-height: 1;
    color: var(--muted);
    cursor: pointer;
    opacity: 0;
    flex: none;
  }

  .tab:hover .tab-close,
  .tab.active .tab-close {
    opacity: 0.7;
  }

  .tab-close:hover {
    opacity: 1;
    color: var(--fg);
  }
</style>
