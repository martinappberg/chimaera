<script lang="ts">
  /**
   * The window's Mastermind panel: the one right-hand column that carries the
   * workspace's Mastermind on EVERY view (a file, a chat, a terminal, the
   * dashboard) — one per window, never per pane. It reads as a sibling of the
   * pane cards (same radius, hairline edge, accent hairline when focused) and
   * docks beside the stage; on a window too narrow to keep the panes usable
   * it floats over the stage's right edge instead of crushing them. It only
   * ever opens on the user's own click / ⌘J (see mastermindPanelState.svelte.ts).
   */
  import MastermindDock from "./MastermindDock.svelte";
  import type { Session } from "../workspace/sessions";
  import type { LayoutCtrl } from "../layout/dnd";
  import {
    mastermindPanel,
    setMastermindPanelOpen,
    setMastermindPanelWidth,
    PANEL_DEFAULT,
    PANEL_MAX,
    PANEL_MIN,
  } from "./mastermindPanelState.svelte";

  interface Props {
    cfg: { session_id: string; mode: "ask" | "auto"; agent?: string } | null;
    session: Session | null;
    wsId: string;
    paneId: string;
    ctrl: LayoutCtrl;
    refresh: () => Promise<void>;
    /** The pane-tab meaning ("this surface is showing") — an open panel is;
     *  document visibility stays ChatView's own business, as for panes. */
    visible: boolean;
    context: { label: string; text: string; title: string } | null;
    /** The window body's width (rail + stage + this) — docked vs overlay. */
    hostWidth: number;
  }

  let { cfg, session, wsId, paneId, ctrl, refresh, visible, context, hostWidth }: Props = $props();

  /** What the panes keep beside a docked panel before it floats instead. */
  const STAGE_MIN = 560;

  let dragWidth = $state<number | null>(null);
  const preferred = $derived(dragWidth ?? mastermindPanel.width);
  const overlay = $derived(hostWidth > 0 && hostWidth - preferred < STAGE_MIN);
  const maxWidth = $derived(
    hostWidth > 0
      ? Math.max(PANEL_MIN, Math.min(PANEL_MAX, hostWidth - (overlay ? 48 : STAGE_MIN)))
      : PANEL_MAX,
  );
  const width = $derived(Math.min(preferred, maxWidth));

  function onResizeDown(e: PointerEvent): void {
    if (e.button !== 0) return;
    e.preventDefault();
    const handle = e.currentTarget as HTMLElement;
    handle.setPointerCapture(e.pointerId);
    const startX = e.clientX;
    const startW = width;
    dragWidth = startW;
    const move = (ev: PointerEvent) => {
      dragWidth = Math.min(Math.max(startW + (startX - ev.clientX), PANEL_MIN), maxWidth);
    };
    const up = () => {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", up);
      if (dragWidth !== null) setMastermindPanelWidth(dragWidth);
      dragWidth = null;
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", up);
  }

  // The accent hairline follows keyboard focus inside the panel, like a
  // focused pane — so it's always clear where typing lands.
  let focused = $state(false);
  let cardEl = $state<HTMLElement | null>(null);
  function syncFocus(): void {
    focused = cardEl !== null && cardEl.contains(document.activeElement);
  }
</script>

<aside
  class="mmpanel"
  class:overlay
  class:resizing={dragWidth !== null}
  style:width="{width + (overlay ? 16 : 8)}px"
  aria-label="Mastermind"
>
  <div
    class="resize"
    role="separator"
    aria-orientation="vertical"
    aria-label="resize the Mastermind panel"
    title="drag to resize · double-click to reset"
    onpointerdown={onResizeDown}
    ondblclick={() => setMastermindPanelWidth(PANEL_DEFAULT)}
  ></div>
  <div class="card" class:focused bind:this={cardEl} onfocusin={syncFocus} onfocusout={() => queueMicrotask(syncFocus)}>
    <MastermindDock
      {cfg}
      {session}
      {wsId}
      {paneId}
      {ctrl}
      {refresh}
      {visible}
      {context}
      onCollapse={() => setMastermindPanelOpen(false)}
    />
  </div>
</aside>

<style>
  .mmpanel {
    position: relative;
    flex: none;
    display: flex;
    min-width: 0;
    min-height: 0;
    /* The stage's own 8px padding is the gap on the left; match it on the
       other three sides so the card lines up with the pane cards. */
    padding: 8px 8px 8px 0;
    background: var(--bg);
    animation: mm-in 0.16s ease-out;
  }
  .mmpanel.overlay {
    position: absolute;
    top: 0;
    right: 0;
    bottom: 0;
    z-index: 25;
    padding: 8px;
    background: transparent;
  }

  .card {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 10px;
    overflow: hidden;
    transition: border-color 0.12s ease;
  }
  .card.focused {
    border-color: color-mix(in srgb, var(--accent) 62%, var(--edge));
  }
  .overlay .card {
    box-shadow: -14px 0 36px rgba(0, 0, 0, 0.22);
  }

  /* The resize edge: the rail-resize idiom — invisible until hovered or
     dragged, then an accent hairline in the gap beside the card. */
  .resize {
    position: absolute;
    top: 8px;
    bottom: 8px;
    left: -6px;
    width: 10px;
    cursor: col-resize;
    z-index: 2;
  }
  .overlay .resize {
    left: 2px;
  }
  .resize::after {
    content: "";
    position: absolute;
    top: 0;
    bottom: 0;
    left: 4px;
    width: 2px;
    border-radius: 1px;
    background: transparent;
    transition: background-color 0.12s ease;
  }
  .resize:hover::after,
  .resizing .resize::after {
    background: color-mix(in srgb, var(--accent) 60%, transparent);
  }
  .resizing {
    user-select: none;
  }

  @keyframes mm-in {
    from {
      opacity: 0;
      transform: translateX(10px);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .mmpanel {
      animation: none;
    }
  }
</style>
