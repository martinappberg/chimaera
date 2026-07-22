<script lang="ts">
  import type { Snippet } from "svelte";
  import Chevron from "./Chevron.svelte";

  /**
   * The shared shell for the pinned work-tray strips above the composer
   * (subagents, background tasks): one home for the quiet chrome — the
   * border-top strip, the one-line collapsible header with its breathing
   * glyph, the bounded row scroll — so the sibling trays read as one family
   * and a chrome fix lands once instead of being hand-mirrored. Collapsed
   * by default: the header count is the glance; the rows are a click away.
   */
  interface Props {
    /** Header glyph (✳ subagents, ⧖ background) — the tray's identity. */
    glyph: string;
    /** The one-line collapsed summary ("2 subagents working"). */
    label: string;
    /** Expanded state — bindable so a tray can gate its own work (e.g. a
     *  1 Hz elapsed clock) on whether the rows are actually visible. */
    open?: boolean;
    /** Whether the glyph breathes. The animation means "work is happening
     *  right now", so a strip that can sit idle (the plan, once every step is
     *  finished or merely waiting) must be able to go still — a permanent
     *  pulse over nothing is the exact noise these strips exist to avoid. */
    pulse?: boolean;
    /** False while the retained owner is hidden. Keep the tray mounted so its
     *  expanded state survives tab switches, but do not animate invisible
     *  work or announce background count churn. */
    visible?: boolean;
    /** The expanded row list (typically WorkTrayRow children). */
    children: Snippet;
  }
  let {
    glyph,
    label,
    open = $bindable(false),
    pulse = true,
    visible = true,
    children,
  }: Props = $props();
</script>

<div class="tray" class:visible>
  <button
    class="tray-head"
    aria-expanded={open}
    onclick={() => (open = !open)}
    title={open ? "collapse" : "expand"}
  >
    <Chevron {open} />
    <span class="spark" class:still={!pulse || !visible} aria-hidden="true">{glyph}</span>
    <!-- aria-live on the summary only: the count changing is worth
         announcing, per-row churn is not. -->
    <span class="head-label" role="status" aria-live={visible ? "polite" : "off"}>{label}</span>
  </button>
  {#if open}
    <div class="rows">
      {@render children()}
    </div>
  {/if}
</div>

<style>
  /* The pinned-monitor chrome, sibling to the plan panel — quiet
     (border-top, muted, theme tokens) so the work trays and the plan read
     as one strip family. */
  .tray {
    flex: none;
    border-top: 1px solid var(--edge);
    padding: 5px 14px 6px;
    font-size: var(--text-sm);
    max-height: 168px;
    overflow-y: auto;
    background: color-mix(in srgb, var(--accent) 4%, transparent);
    animation: rise 0.15s ease; /* shared keyframe in app.css */
  }
  .tray:not(.visible) {
    animation: none;
  }
  /* The whole header is the collapse toggle (button reset), so the one-line
     summary is the click target — like the ToolGroup summary. */
  .tray-head {
    display: flex;
    align-items: center;
    gap: 7px;
    width: 100%;
    background: none;
    border: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    padding: 1px 0 3px;
    cursor: pointer;
  }
  .head-label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .spark {
    flex: none;
    color: var(--accent);
    /* Presence, not alarm — shared keyframe in app.css. */
    animation: tray-breathe 1.8s ease-in-out infinite;
  }
  .spark.still {
    animation: none;
    color: var(--muted);
  }
  @media (prefers-reduced-motion: reduce) {
    .tray,
    .spark {
      animation: none;
    }
  }
</style>
