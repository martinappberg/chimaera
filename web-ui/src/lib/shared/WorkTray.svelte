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
    <span class="spark" class:still={!pulse || !visible} aria-hidden="true">{glyph}</span>
    <!-- aria-live on the summary only: the count changing is worth
         announcing, per-row churn is not. -->
    <span class="head-label" role="status" aria-live={visible ? "polite" : "off"}>{label}</span>
    <!-- The disclosure trails, as on the activity lines and the plan card:
         one glyph leads the line. -->
    <Chevron {open} />
  </button>
  {#if open}
    <div class="rows">
      {@render children()}
    </div>
  {/if}
</div>

<style>
  /* The pinned-monitor chrome: the plan card's recipe (chat/PlanCard.svelte
     — the same edge, radius and neutral tint), so the three pinned cards
     above the composer read as one stack. Colour is reserved for the live
     glyph: "alive", never "a tray". The host (ChatView) sets the width and
     the stacking margins. */
  .tray {
    flex: none;
    border: 1px solid var(--edge);
    border-radius: 8px;
    padding: 4px 8px 5px 11px;
    font-size: var(--text-sm);
    line-height: 1.45;
    max-height: 168px;
    overflow-y: auto;
    background: color-mix(in srgb, var(--fg) 2%, var(--bg));
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
    gap: 9px;
    width: 100%;
    background: none;
    border: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    padding: 2px 0;
    cursor: pointer;
    transition: color 0.12s ease;
  }
  .tray-head:hover,
  .tray-head:focus-visible {
    color: var(--fg);
  }
  .tray-head :global(.chev) {
    opacity: 0.55;
  }
  .tray-head:hover :global(.chev) {
    opacity: 1;
  }
  /* The glyph is a text character; a fixed slot keeps the label's edge where
     the plan card's marks put theirs. */
  .spark {
    width: 14px;
    text-align: center;
  }
  /* Rows start under the label, not under the glyph. */
  .rows {
    padding: 1px 0 1px 23px;
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
  /* Hidden document: nobody sees the breath — stop burning frames (the
     html.app-hidden contract; see app.css). */
  :global(html.app-hidden) .spark {
    animation-play-state: paused;
  }
  @media (prefers-reduced-motion: reduce) {
    .tray,
    .spark {
      animation: none;
    }
  }
</style>
