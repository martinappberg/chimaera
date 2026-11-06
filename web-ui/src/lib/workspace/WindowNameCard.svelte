<script lang="ts">
  /**
   * The window's name, large, while macOS Mission Control (or App Exposé)
   * shows the window as a thumbnail among others. The shell watches for it
   * (`onMissionControl`; the page gets no sign of it on its own) and App
   * passes `shown`. The thumbnail is the window's live surface, so the card
   * shows inside it.
   *
   * Only a workspace's main window shows it (App decides). Anatomy, read at
   * a quarter of the window's size: the chimaera mark on an app tile (so a Chimaera window reads as one among other apps' windows),
   * the name sized to fit one line, a host pill for a remote window, and a
   * Dock-style badge on the tile when chats in this window wait on the
   * person — the moment you pick a window is when that matters most.
   *
   * `data-overlay` makes pane views (HTML previews, the browser pane: native
   * views above this page) step aside while it is up, as under any overlay.
   * Mounted only while shown. In: at once, on a solid ground (no fade, no
   * backdrop blur) — it has to be up while the windows are still zooming
   * out. Out: a fade as long as the zoom back, which the shell reports from
   * its start, so the window settles into itself instead of flickering.
   */
  import { cubicOut } from "svelte/easing";
  import { fade } from "svelte/transition";
  import BrandMark from "../shared/BrandMark.svelte";

  interface Props {
    shown: boolean;
    /** The workspace. */
    name: string;
    /** A remote window's host (with its compute node, if any). */
    host?: string | null;
    /** Chats in this window waiting on the person. */
    waiting?: number;
  }

  let { shown, name, host = null, waiting = 0 }: Props = $props();

  const reduced = typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;

  // The name's size follows its length so a long workspace name still sits
  // on one line; CSS clamps both ends.
  const chars = $derived(Math.max(6, [...name].length));
</script>

{#if shown}
  <div
    class="window-name-card"
    data-overlay
    aria-hidden="true"
    out:fade={{ duration: reduced ? 0 : 260, easing: cubicOut }}
  >
    <div class="card-body">
      <div class="tile" class:waiting={waiting > 0}>
        <BrandMark size={240} />
        {#if waiting > 0}
          <span class="badge">{waiting > 99 ? "99+" : waiting}</span>
        {/if}
      </div>
      <div class="name" style="--chars: {chars}">{name}</div>
      {#if host}
        <div class="host">
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <rect x="2" y="2.5" width="12" height="4.5" rx="1.2" />
            <rect x="2" y="9" width="12" height="4.5" rx="1.2" />
            <circle cx="4.6" cy="4.75" r="0.6" class="led" />
            <circle cx="4.6" cy="11.25" r="0.6" class="led" />
          </svg>
          <span>{host}</span>
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .window-name-card {
    position: fixed;
    inset: 0;
    /* Above every overlay the window can hold: whatever was up is still
       there, just veiled. */
    z-index: 2147483000;
    pointer-events: none;
    display: grid;
    place-items: center;
    padding: 6vmin;
    /* Solid, lit softly from the middle where the name sits. */
    background:
      radial-gradient(ellipse 70% 60% at 50% 46%, var(--bg), transparent 75%),
      color-mix(in srgb, var(--bg) 94%, var(--fg));
  }
  .card-body {
    display: flex;
    flex-direction: column;
    align-items: center;
    max-width: 100%;
    text-align: center;
    font-family: var(--ui-font);
  }

  /* The app tile: the mark the Dock shows, at the scale a thumbnail needs. */
  .tile {
    position: relative;
    width: clamp(64px, 17vmin, 168px);
    aspect-ratio: 1;
    border-radius: 23%;
    display: grid;
    place-items: center;
    background: linear-gradient(
      160deg,
      color-mix(in srgb, var(--overlay-bg) 96%, var(--fg)),
      color-mix(in srgb, var(--overlay-bg) 86%, var(--fg))
    );
    border: 1px solid color-mix(in srgb, var(--fg) 9%, transparent);
    box-shadow:
      0 0.6vmin 2.4vmin color-mix(in srgb, #000 16%, transparent),
      inset 0 1px 0 color-mix(in srgb, #fff 30%, transparent);
    margin-bottom: 3.4vmin;
  }
  .tile :global(svg) {
    width: 72%;
    height: 72%;
  }
  .tile.waiting {
    box-shadow:
      0 0 0 0.5vmin color-mix(in srgb, var(--warn) 55%, transparent),
      0 0 4vmin color-mix(in srgb, var(--warn) 30%, transparent),
      0 0.6vmin 2.4vmin color-mix(in srgb, #000 16%, transparent);
  }
  /* A Dock badge: the count of chats waiting on the person. */
  .badge {
    position: absolute;
    top: -14%;
    right: -14%;
    min-width: 38%;
    height: 38%;
    padding: 0 9%;
    box-sizing: border-box;
    border-radius: 999px;
    display: grid;
    place-items: center;
    font-size: clamp(14px, 3.6vmin, 36px);
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    color: #fff;
    background: linear-gradient(180deg, #ff5b4f, #e5372b);
    box-shadow:
      0 0 0 0.45vmin var(--bg),
      0 0.4vmin 1.2vmin color-mix(in srgb, #000 22%, transparent);
  }

  /* Sized to the window and to its own length: one line at a quarter of the
     window's size, whether "atlas" or a 30-character worktree name. */
  .name {
    max-width: 100%;
    font-size: clamp(28px, min(11vmin, 150vw / var(--chars)), 150px);
    font-weight: 680;
    letter-spacing: -0.03em;
    line-height: 1.05;
    color: var(--fg);
    overflow-wrap: anywhere;
    text-wrap: balance;
  }
  /* A remote window's host: what tells the same project on two hosts apart,
     so it is the second-largest thing on the card. */
  .host {
    margin-top: 3vmin;
    display: inline-flex;
    align-items: center;
    gap: 0.45em;
    max-width: 100%;
    padding: 0.32em 0.85em 0.32em 0.7em;
    border-radius: 999px;
    font-size: clamp(18px, 5.4vmin, 64px);
    font-weight: 650;
    letter-spacing: -0.01em;
    color: var(--accent-text);
    background: color-mix(in srgb, var(--accent) 13%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 28%, transparent);
  }
  .host span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .host svg {
    width: 1em;
    height: 1em;
    flex: none;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.3;
  }
  .host svg .led {
    fill: currentColor;
    stroke: none;
  }
</style>
