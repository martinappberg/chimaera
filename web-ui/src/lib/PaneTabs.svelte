<script lang="ts">
  /**
   * The pane's always-present top bar (~26px): type glyph + tab name per
   * tab (active emphasized by WEIGHT, not color), pane controls at the
   * right edge (fade in on bar hover; the zoom badge stays persistent
   * while zoomed). The bar's empty area is a drag handle for the active
   * tab, so every pane can always be re-tiled by its bar.
   *
   * Surface parity: terminal and file tabs share one anatomy — glyph +
   * name + close, same drag, same middle-click close, same dblclick zoom.
   * A terminal's glyph carries its session-state color.
   */
  import { tabKey, type PaneNode, type Tab } from "./layout";
  import type { Session } from "./sessions";
  import { dotState, dotTitle } from "./sessions";
  import type { DropSpot, LayoutCtrl } from "./dnd";
  import { basename } from "./files";
  import { dirtyFiles } from "./editing";
  import { KEYS } from "./keys";
  import FileIcon from "./FileIcon.svelte";

  interface Props {
    node: PaneNode;
    /** True while this pane is rendered zoomed (fullscreen in the window). */
    zoomed?: boolean;
    sessions: Map<string, Session>;
    names: Map<string, string>;
    /** Open-file tab titles (basename, disambiguated), keyed by path. */
    fileNames: Map<string, string>;
    dropSpot: DropSpot | null;
    ctrl: LayoutCtrl;
    /** Bound by Pane so the dnd hit-tester can target this bar. */
    el?: HTMLElement | null;
  }

  let {
    node,
    zoomed = false,
    sessions,
    names,
    fileNames,
    dropSpot,
    ctrl,
    el = $bindable(null),
  }: Props = $props();

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

  /** Empty bar area drags the pane's ACTIVE tab (capture runs before the
   *  tabs' own handlers; anything inside a tab or a button is theirs). */
  function onBarPointerDown(e: PointerEvent): void {
    if (!(e.target instanceof Element)) return;
    if (e.target.closest("[data-tab-index], button") !== null) return;
    const active = node.tabs[node.active];
    if (active !== undefined) ctrl.dragTab(e, node.id, node.active, active);
  }
</script>

<div class="bar" bind:this={el} onpointerdowncapture={onBarPointerDown}>
  <div class="tabs" role="tablist">
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
          <svg class="glyph {s ? dotState(s) : ''}" viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
            <title>{s ? dotTitle(s) : "terminal"}</title>
            <path
              d="M3 4.5L6.5 8 3 11.5M8.5 12h4.5"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        {:else if $dirtyFiles.has(tab.path)}
          <!-- Dirty dot replaces the type glyph in its slot (unsaved edits). -->
          <span class="dirty-dot" title="unsaved changes"></span>
        {:else}
          <span class="tab-glyph" class:on={i === node.active}>
            <FileIcon path={tab.path} size={13} plain={i === node.active} />
          </span>
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

  <!-- Pane controls at the bar's right edge: the mouse path to every pane
       chord (tooltips teach the chords). Faded in on bar hover; the zoom
       badge stays persistent while zoomed. -->
  <div class="bar-right">
    {#if zoomed}
      <button class="zoom-badge" title="exit zoom ({KEYS.zoom})" onclick={() => ctrl.zoomPane(node.id)}
        >zoom</button
      >
    {/if}
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
      {#if !zoomed}
        <button class="ctl" title="zoom ({KEYS.zoom})" aria-label="zoom" onclick={() => ctrl.zoomPane(node.id)}>
          <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
            <path d="M9.5 2.5h4v4M6.5 13.5h-4v-4M13.5 2.5L9 7M2.5 13.5L7 9" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
          </svg>
        </button>
      {/if}
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
  </div>
</div>

<style>
  .bar {
    flex: none;
    display: flex;
    align-items: stretch;
    height: 26px;
    overflow: hidden;
    border-bottom: 1px solid var(--edge);
    padding: 0 4px;
    user-select: none;
  }

  .tabs {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: stretch;
    overflow: hidden;
  }

  .tab {
    position: relative;
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 0 0.4rem 0 0.55rem;
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

  /* Active-tab emphasis via weight, not color — the bar stays quiet. */
  .tab.active {
    color: var(--fg);
    font-weight: 600;
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

  /* Type glyph, shared slot for both surfaces; a terminal's glyph carries
     its session-state color (same palette as the rail dots). */
  .glyph {
    flex: none;
    color: var(--muted);
    opacity: 0.8;
  }

  .glyph.alive {
    color: var(--accent);
    opacity: 1;
  }

  .glyph.attn {
    color: var(--warn);
    opacity: 1;
  }

  .glyph.err {
    color: var(--err);
    opacity: 1;
  }

  .glyph.rate {
    color: var(--rate);
    opacity: 1;
  }

  .glyph.done {
    color: var(--fg);
    opacity: 0.6;
  }

  .glyph.starting {
    opacity: 0.9;
  }

  /* File type glyph slot; quiet by default, lifts to full strength on the
     active tab (parity with the weight-based active emphasis). */
  .tab-glyph {
    flex: none;
    display: flex;
    align-items: center;
    opacity: 0.85;
  }

  .tab-glyph.on {
    opacity: 1;
  }

  /* Unsaved-edits marker: sits in the glyph slot, same footprint as a glyph. */
  .dirty-dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--accent);
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
    font-weight: 400;
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

  /* --- controls at the bar's right edge --- */

  .bar-right {
    flex: none;
    display: flex;
    align-items: center;
    gap: 4px;
    padding-left: 4px;
  }

  .controls {
    display: flex;
    align-items: center;
    gap: 1px;
    opacity: 0;
    pointer-events: none;
    transition: opacity 0.12s ease;
  }

  .bar:hover .controls,
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

  /* Persistent while zoomed — the always-visible mouse exit from zoom. */
  .zoom-badge {
    appearance: none;
    border: none;
    font: inherit;
    font-size: var(--text-xs);
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--muted);
    background: color-mix(in srgb, var(--fg) 7%, transparent);
    padding: 0 7px;
    height: 18px;
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
</style>
