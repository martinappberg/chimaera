<script lang="ts">
  /**
   * The Slurm queue popover, opened from the daemon bar's compute chip.
   * Orientation, not a dashboard: your jobs (state dot · id · name · partition
   * · time left · nodes), one partitions line, and a refresh that forces the
   * daemon to re-detect the scheduler (`?refresh=true` — the "I just
   * module-loaded slurm" path). Overlay language matches the launcher: any
   * outside press or Escape closes.
   */
  import { onMount } from "svelte";
  import { computeStatus, refreshCompute } from "./compute";

  interface Props {
    /** The compute chip's rect; the popover hangs above it (fixed). */
    anchor: DOMRect;
    onClose: () => void;
  }

  let { anchor, onClose }: Props = $props();

  let rootEl = $state<HTMLElement | null>(null);

  onMount(() => {
    rootEl?.focus();
    // Presses on the chip are its own toggle semantics — not "outside"
    // (closing here would race the chip's click handler and reopen).
    const onDown = (e: PointerEvent) => {
      if (rootEl === null || !(e.target instanceof Node)) return;
      if (rootEl.contains(e.target)) return;
      if (e.target instanceof Element && e.target.closest(".daemon-compute") !== null) return;
      onClose();
    };
    window.addEventListener("pointerdown", onDown, true);
    return () => window.removeEventListener("pointerdown", onDown, true);
  });

  /** Fixed position: above the anchor, clamped into the viewport.
   *  (clientWidth/Height fallbacks: embedded webviews can report
   *  window.inner* as 0.) */
  const pos = $derived.by(() => {
    const viewW = window.innerWidth || document.documentElement.clientWidth || 1280;
    const viewH = window.innerHeight || document.documentElement.clientHeight || 800;
    const width = 320;
    const left = Math.max(8, Math.min(anchor.left, viewW - width - 8));
    const bottom = viewH - anchor.top + 6;
    const maxH = Math.max(140, anchor.top - 20);
    return { left, bottom, width, maxH };
  });

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
    }
  }
</script>

<div
  class="compute"
  role="dialog"
  aria-label="slurm queue"
  tabindex="-1"
  bind:this={rootEl}
  style:left="{pos.left}px"
  style:bottom="{pos.bottom}px"
  style:width="{pos.width}px"
  style:max-height="{pos.maxH}px"
  onkeydown={onKeydown}
>
  <div class="head">
    <span class="title">slurm</span>
    <button
      class="refresh"
      title="refresh — re-detect the scheduler and refetch the queue"
      aria-label="refresh"
      onclick={() => refreshCompute()}
    >
      <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
        <path
          d="M13.2 8a5.2 5.2 0 1 1-1.6-3.75M13.2 1.8v2.8h-2.8"
          fill="none"
          stroke="currentColor"
          stroke-width="1.4"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
      </svg>
    </button>
  </div>
  {#if $computeStatus !== null && $computeStatus.jobs.length > 0}
    <div class="jobs">
      {#each $computeStatus.jobs as j (j.id)}
        <div class="job" title="{j.state} · {j.id} {j.name}">
          <span
            class="dot"
            class:run={j.state === "RUNNING"}
            class:pend={j.state === "PENDING"}
          ></span>
          <span class="jid">{j.id}</span>
          <span class="jname">{j.name}</span>
          <span class="jpart">{j.partition}</span>
          <span class="jtime">{j.time_left}</span>
          <span class="jnodes">{j.nodes}</span>
        </div>
      {/each}
    </div>
    {#if $computeStatus.truncated}
      <div class="more">+ more</div>
    {/if}
  {:else}
    <div class="empty">no jobs in the queue</div>
  {/if}
  {#if $computeStatus !== null && $computeStatus.partitions.length > 0}
    <div class="parts">
      {#each $computeStatus.partitions as p, i (p.name)}
        {#if i > 0}<span class="sep">·</span>{/if}
        <span
          class="part"
          class:down={!p.avail}
          title="{p.name} — {p.nodes} nodes{p.default ? ' · default partition' : ''}{p.avail
            ? ''
            : ' · unavailable'}"
          >{p.name}{#if p.default}<span class="def">*</span>{/if}</span
        >
      {/each}
    </div>
  {/if}
</div>

<style>
  .compute {
    position: fixed;
    z-index: 120;
    display: flex;
    flex-direction: column;
    padding: 5px;
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 10px;
    box-shadow:
      0 1px 2px rgba(0, 0, 0, 0.08),
      0 12px 32px rgba(0, 0, 0, 0.22);
    outline: none;
    animation: pop 0.12s ease-out;
  }

  @keyframes pop {
    from {
      opacity: 0;
      transform: translateY(3px) scale(0.985);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .compute {
      animation: none;
    }
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 3px 6px 5px;
  }

  .title {
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.04em;
    color: var(--muted);
  }

  .refresh {
    appearance: none;
    border: none;
    background: none;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 18px;
    padding: 0;
    border-radius: 5px;
    color: var(--muted);
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .refresh:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .jobs {
    overflow-y: auto;
    min-height: 0;
  }

  .job {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 3px 6px;
    border-radius: 5px;
    font-family: var(--mono);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--fg);
    white-space: nowrap;
  }

  .job:hover {
    background: var(--row-hover);
  }

  .dot {
    flex: none;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    /* Neutral (COMPLETING, CANCELLED, …): present but quiet. */
    background: color-mix(in srgb, var(--muted) 45%, transparent);
  }

  .dot.run {
    background: var(--accent);
  }

  .dot.pend {
    background: var(--muted);
  }

  .jid {
    flex: none;
    color: var(--muted);
  }

  .jname {
    flex: 1;
    min-width: 40px;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .jpart,
  .jnodes {
    flex: none;
    max-width: 72px;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--muted);
  }

  .jtime {
    flex: none;
    color: var(--muted);
  }

  .empty {
    padding: 6px;
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .more {
    padding: 2px 6px 3px;
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .parts {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: 3px;
    padding: 5px 6px 3px;
    border-top: 1px solid var(--edge);
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    /* Orientation only — long partition lists stay one quiet block. */
    max-height: 58px;
    overflow: hidden;
  }

  .part.down {
    opacity: 0.5;
  }

  .def {
    color: var(--accent);
  }

  .sep {
    opacity: 0.5;
  }
</style>
