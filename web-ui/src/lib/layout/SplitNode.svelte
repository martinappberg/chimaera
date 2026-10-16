<script lang="ts">
  import type { LayoutNode } from "./layout";
  import { MIN_RATIO } from "./layout";
  import type { Session } from "../workspace/sessions";
  import type { DropSpot, LayoutCtrl } from "./dnd";
  import type { LinkCtrl } from "../workspace/agentLinks";
  import type { DashCtx } from "../dashboard/dash";
  import Pane from "./Pane.svelte";
  import Self from "./SplitNode.svelte";
  import { startResizeDrag } from "../shared/resizeDrag";

  interface Props {
    node: LayoutNode;
    focusedPaneId: string;
    dropSpot: DropSpot | null;
    sessions: Map<string, Session>;
    names: Map<string, string>;
    fileNames: Map<string, string>;
    links: Map<string, string>;
    linkCtrl: LinkCtrl;
    /** Active workspace root (touched-files paths relativize against it). */
    wsRoot: string | null;
    /** Active workspace id (the git surfaces query the daemon with it). */
    wsId: string | null;
    /** Panes whose bottom band is armed for the current drag. */
    bandPanes: ReadonlySet<string>;
    /** True when the window has exactly one pane (hides the move-pane grip). */
    /** App-level context for the dashboard surface. */
    dash: DashCtx;
    ctrl: LayoutCtrl;
  }

  let {
    node,
    focusedPaneId,
    dropSpot,
    sessions,
    names,
    fileNames,
    links,
    linkCtrl,
    wsRoot,
    wsId,
    bandPanes,
    dash,
    ctrl,
  }: Props = $props();

  const MIN_PANE_PX = 120;
  const DIVIDER_PX = 8;

  let el = $state<HTMLDivElement | null>(null);
  let dividerActive = $state(false);

  /**
   * Divider resize (`shared/resizeDrag`: pointer capture, rAF-throttled ratio
   * updates, Escape and pointercancel restore the starting ratio) with a 120px
   * minimum pane size. Terminal refits are suppressed for the whole drag
   * via ctrl.dividerDrag and flushed once at pointer-up.
   */
  function onDividerDown(e: PointerEvent) {
    if (node.type !== "split" || el === null || e.button !== 0) return;
    e.preventDefault();
    const split = node;
    const rect = el.getBoundingClientRect();
    const horizontal = split.dir === "row";
    const total = (horizontal ? rect.width : rect.height) - DIVIDER_PX;
    if (total <= 0) return;
    const min = Math.max(Math.min(MIN_PANE_PX / total, 0.5), MIN_RATIO);

    dividerActive = true;
    ctrl.dividerDrag(true);
    startResizeDrag(e, {
      axis: horizontal ? "x" : "y",
      initial: split.ratio,
      compute: (at) => {
        const pos = (horizontal ? at - rect.left : at - rect.top) - DIVIDER_PX / 2;
        return Math.min(Math.max(pos / total, min), 1 - min);
      },
      apply: (ratio) => ctrl.setRatio(split.id, ratio),
      onEnd: () => {
        dividerActive = false;
        ctrl.dividerDrag(false);
      },
    });
  }
</script>

{#if node.type === "pane"}
  <Pane {node} {focusedPaneId} {dropSpot} {sessions} {names} {fileNames} {links} {linkCtrl} {wsRoot} {wsId} {bandPanes} {dash} {ctrl} />
{:else}
  <div class="split" class:col={node.dir === "col"} bind:this={el}>
    <div class="cell" style:flex-grow={node.ratio}>
      <!-- Inside a split there are ≥2 panes, so children are never solo. -->
      <Self node={node.a} {focusedPaneId} {dropSpot} {sessions} {names} {fileNames} {links} {linkCtrl} {wsRoot} {wsId} {bandPanes} {dash} {ctrl} />
    </div>
    <div
      class="divider"
      class:active={dividerActive}
      role="separator"
      aria-orientation={node.dir === "row" ? "vertical" : "horizontal"}
      title="drag to resize · double-click for 50/50"
      onpointerdowncapture={onDividerDown}
      ondblclick={() => {
        if (node.type === "split") ctrl.setRatio(node.id, 0.5);
      }}
    ></div>
    <div class="cell" style:flex-grow={1 - node.ratio}>
      <Self node={node.b} {focusedPaneId} {dropSpot} {sessions} {names} {fileNames} {links} {linkCtrl} {wsRoot} {wsId} {bandPanes} {dash} {ctrl} />
    </div>
  </div>
{/if}

<style>
  .split {
    flex: 1;
    display: flex;
    flex-direction: row;
    min-width: 0;
    min-height: 0;
  }

  .split.col {
    flex-direction: column;
  }

  .cell {
    display: flex;
    flex-basis: 0;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }

  /* The divider doubles as the visual gap between pane cards; a slim line
     appears on hover/drag so the affordance stays quiet. */
  .divider {
    flex: 0 0 8px;
    position: relative;
    cursor: col-resize;
    touch-action: none;
  }

  .split.col > .divider {
    cursor: row-resize;
  }

  .divider::after {
    content: "";
    position: absolute;
    inset: 12px 3px;
    border-radius: 1px;
    background: transparent;
    transition: background-color 0.12s ease;
  }

  .split.col > .divider::after {
    inset: 3px 12px;
  }

  .divider:hover::after {
    background: var(--edge);
  }

  .divider.active::after {
    background: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }
</style>
