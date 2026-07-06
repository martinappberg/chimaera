<script lang="ts">
  import { tabKey, type PaneNode, type Tab } from "./layout";
  import type { Session } from "./sessions";
  import { dotState, dotTitle } from "./sessions";
  import type { DropSpot, LayoutCtrl } from "./dnd";
  import { basename } from "./files";

  interface Props {
    node: PaneNode;
    sessions: Map<string, Session>;
    names: Map<string, string>;
    /** Open-file tab titles (basename, disambiguated), keyed by path. */
    fileNames: Map<string, string>;
    dropSpot: DropSpot | null;
    ctrl: LayoutCtrl;
    /** Bound by Pane so the dnd hit-tester can target this bar. */
    el?: HTMLElement | null;
  }

  let { node, sessions, names, fileNames, dropSpot, ctrl, el = $bindable(null) }: Props = $props();

  /** Insertion index while a drag hovers this tab bar, else null. */
  const insertIndex = $derived(
    dropSpot?.kind === "tab" && dropSpot.paneId === node.id ? dropSpot.index : null,
  );

  function label(tab: Tab): string {
    if (tab.surface === "terminal") {
      return names.get(tab.sessionId) ?? sessions.get(tab.sessionId)?.name ?? tab.sessionId.slice(0, 8);
    }
    return fileNames.get(tab.path) ?? basename(tab.path);
  }
</script>

<div class="tabs" role="tablist" bind:this={el}>
  {#each node.tabs as tab, i (tabKey(tab))}
    <div
      class="tab"
      class:active={i === node.active}
      class:insert={insertIndex === i}
      role="tab"
      aria-selected={i === node.active}
      tabindex="-1"
      data-tab-index={i}
      title={tab.surface === "file" ? tab.path : label(tab)}
      onpointerdowncapture={(e) => {
        // Capture-phase (directly attached, not delegated); ignore presses
        // on the close button so it stays a plain click.
        if (e.target instanceof Element && e.target.closest(".tab-close")) return;
        ctrl.dragTab(e, node.id, i, tab);
      }}
      onauxclick={(e) => {
        // Middle-click closes the tab (detaches the view, never the session).
        if (e.button === 1) {
          e.preventDefault();
          ctrl.closeTab(node.id, i);
        }
      }}
      ondblclick={() => ctrl.zoomPane(node.id)}
    >
      {#if tab.surface === "terminal"}
        {@const s = sessions.get(tab.sessionId)}
        <span class="dot {s ? dotState(s) : ''}" title={s ? dotTitle(s) : undefined}></span>
      {:else}
        <svg class="file-glyph" viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
          <path
            d="M4.5 1.75h5L12.5 4.75V14a.25.25 0 0 1-.25.25h-7.5a.25.25 0 0 1-.25-.25V2a.25.25 0 0 1 .25-.25Z"
            fill="none"
            stroke="currentColor"
            stroke-width="1.3"
            stroke-linejoin="round"
          />
        </svg>
      {/if}
      <span class="tab-name">{label(tab)}</span>
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
    height: 28px;
    overflow: hidden;
    border-bottom: 1px solid var(--edge);
    padding: 0 4px;
  }

  .tab {
    position: relative;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 0.4rem 0 0.6rem;
    max-width: 200px;
    min-width: 0;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: default;
    user-select: none;
    transition: color 0.12s ease;
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

  .file-glyph {
    flex: none;
    opacity: 0.75;
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
    font-size: var(--text-md);
    line-height: 1;
    color: var(--muted);
    cursor: pointer;
    opacity: 0;
    flex: none;
    transition:
      opacity 0.12s ease,
      color 0.12s ease;
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
