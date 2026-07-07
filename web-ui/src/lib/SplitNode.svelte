<script lang="ts">
  import type { LayoutNode } from "./layout";
  import { MIN_RATIO } from "./layout";
  import type { Session } from "./sessions";
  import type { DropSpot, LayoutCtrl } from "./dnd";
  import type { LinkCtrl } from "./agentLinks";
  import Pane from "./Pane.svelte";
  import Self from "./SplitNode.svelte";

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
    ctrl,
  }: Props = $props();

  const MIN_PANE_PX = 120;
  const DIVIDER_PX = 8;

  let el = $state<HTMLDivElement | null>(null);
  let dividerActive = $state(false);

  /**
   * Divider resize with pointer capture: rAF-throttled ratio updates (60fps,
   * no synchronous layout thrash), 120px minimum pane size, Escape restores
   * the starting ratio. Terminal refits are suppressed for the whole drag
   * via ctrl.dividerDrag and flushed once at pointer-up.
   */
  function onDividerDown(e: PointerEvent) {
    if (node.type !== "split" || el === null || e.button !== 0) return;
    e.preventDefault();
    const split = node;
    const divider = e.currentTarget as HTMLElement;
    const rect = el.getBoundingClientRect();
    const horizontal = split.dir === "row";
    const total = (horizontal ? rect.width : rect.height) - DIVIDER_PX;
    if (total <= 0) return;
    const startRatio = split.ratio;
    const min = Math.max(Math.min(MIN_PANE_PX / total, 0.5), MIN_RATIO);
    const pointerId = e.pointerId;
    let raf = 0;
    let last = horizontal ? e.clientX : e.clientY;
    let done = false;

    try {
      divider.setPointerCapture(pointerId);
    } catch {
      // capture unavailable; window-level listeners still track the drag
    }
    dividerActive = true;
    ctrl.dividerDrag(true);

    const apply = () => {
      raf = 0;
      const pos = (horizontal ? last - rect.left : last - rect.top) - DIVIDER_PX / 2;
      const ratio = Math.min(Math.max(pos / total, min), 1 - min);
      ctrl.setRatio(split.id, ratio);
    };

    const onMove = (ev: PointerEvent) => {
      if (ev.pointerId !== pointerId) return;
      last = horizontal ? ev.clientX : ev.clientY;
      if (raf === 0) raf = requestAnimationFrame(apply);
    };

    const finish = (cancel: boolean) => {
      if (done) return;
      done = true;
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onCancel);
      window.removeEventListener("keydown", onKey, true);
      if (raf !== 0) cancelAnimationFrame(raf);
      if (cancel) ctrl.setRatio(split.id, startRatio);
      dividerActive = false;
      ctrl.dividerDrag(false);
    };

    const onUp = (ev: PointerEvent) => {
      if (ev.pointerId === pointerId) finish(false);
    };
    const onCancel = (ev: PointerEvent) => {
      if (ev.pointerId === pointerId) finish(true);
    };
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") {
        ev.preventDefault();
        ev.stopPropagation();
        finish(true);
      }
    };

    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onCancel);
    window.addEventListener("keydown", onKey, true);
  }
</script>

{#if node.type === "pane"}
  <Pane {node} {focusedPaneId} {dropSpot} {sessions} {names} {fileNames} {links} {linkCtrl} {wsRoot} {ctrl} />
{:else}
  <div class="split" class:col={node.dir === "col"} bind:this={el}>
    <div class="cell" style:flex-grow={node.ratio}>
      <Self node={node.a} {focusedPaneId} {dropSpot} {sessions} {names} {fileNames} {links} {linkCtrl} {wsRoot} {ctrl} />
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
      <Self node={node.b} {focusedPaneId} {dropSpot} {sessions} {names} {fileNames} {links} {linkCtrl} {wsRoot} {ctrl} />
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
