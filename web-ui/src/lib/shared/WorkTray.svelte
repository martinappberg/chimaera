<script lang="ts">
  import type { Snippet } from "svelte";
  import Chevron from "./Chevron.svelte";

  /**
   * The shared shell for the pinned work trays above the composer
   * (subagents, background tasks), and the dashboard agent card's work
   * drop-down: one home for the quiet chrome — the pinned card (app.css
   * `.pinned-card`, the plan card's recipe), the one-line collapsible
   * header with its breathing glyph leading and its chevron trailing, the
   * bounded row scroll — so the sibling trays read as one family and a
   * chrome fix lands once instead of being hand-mirrored. Collapsed by
   * default: the header count is the glance; the rows are a click away.
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

<div class="tray pinned-card" class:visible>
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
  /* Chrome (edge, radius, tint, entrance) is the shared .pinned-card recipe
     in app.css. Colour is reserved for the live glyph: "alive", never "a
     tray". The host sets the width and the stacking margins. The right
     padding puts the trailing chevron where the plan card's toggle puts
     its own (12px from the edge). */
  .tray {
    padding: 4px 12px 5px 11px;
    font-size: var(--text-sm);
    line-height: 1.45;
    max-height: 168px;
    overflow-y: auto;
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
  /* Hot (pointer or keyboard): the line and its chevron rise to the
     foreground together, as the plan card's toggle does. */
  .tray-head:hover,
  .tray-head:focus-visible,
  .tray-head:hover :global(.chev),
  .tray-head:focus-visible :global(.chev) {
    color: var(--fg);
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
  /* The glyph is a text character; a fixed slot keeps the label's edge where
     the plan card's marks put theirs. */
  .spark {
    flex: none;
    width: 14px;
    text-align: center;
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
    .spark {
      animation: none;
    }
  }
</style>
