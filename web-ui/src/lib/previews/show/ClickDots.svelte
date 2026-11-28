<!--
  A slide's click dots: a start mark (the slide before its first click),
  then one dot a click, at most DOTS of them in a window that follows the
  build, so a slide of 300 clicks keeps a short row. The dots up to the
  build are lit; the one on screen is ringed. Arrows, Home and End move
  along them (and the build with them); Escape asks for the slide as drawn
  where that exists (`canClear`). The bar and the presenter's footer draw
  the same component.
-->
<script lang="ts">
  import { tick } from "svelte";

  interface Props {
    clicks: number;
    /** The build on screen; null: the slide as drawn. */
    at: number | null;
    /** A click on the dot on screen goes back to as drawn (browsing). */
    canClear: boolean;
    /** Why the builds are approximate, for the tooltips ("" when exact):
     *  asked when the pointer or focus comes to the dots, not on every render. */
    note?: () => string;
    /** A dot was clicked. */
    onpick: (k: number) => void;
    /** The keys chose a build (null: as drawn). */
    onset: (k: number | null) => void;
  }

  let { clicks, at, canClear, note: noteOf, onpick, onset }: Props = $props();

  const DOTS = 8;
  const span = $derived.by(() => {
    if (clicks <= DOTS) return { first: 1, last: clicks };
    const first = Math.min(Math.max(1, (at ?? 0) - Math.floor(DOTS / 2) + 1), clicks - DOTS + 1);
    return { first, last: first + DOTS - 1 };
  });

  let el = $state<HTMLSpanElement | null>(null);
  /** The note, worked out only while the pointer or focus is on the dots
   *  (where a tooltip can show), and again if the slide changes under it. */
  let pointer = $state(false);
  let focused = $state(false);
  const note = $derived(pointer || focused ? (noteOf?.() ?? "") : "");
  const tip = (label: string) => (note === "" ? label : `${label} · ${note}`);

  function onkey(e: KeyboardEvent): void {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    let to: number | null;
    if (e.key === "ArrowRight") to = Math.min(clicks, (at ?? -1) + 1);
    else if (e.key === "ArrowLeft") to = Math.max(0, (at ?? 1) - 1);
    else if (e.key === "Home") to = 0;
    else if (e.key === "End") to = clicks;
    else if (e.key === "Escape" && canClear && at !== null) to = null;
    else return;
    e.preventDefault();
    e.stopPropagation();
    onset(to);
    void tick().then(() => el?.querySelector<HTMLElement>('[tabindex="0"]')?.focus());
  }
</script>

<span
  class="builds"
  bind:this={el}
  role="group"
  aria-label="builds"
  title={note || undefined}
  onpointerenter={() => (pointer = true)}
  onpointerleave={() => (pointer = false)}
  onfocusin={() => (focused = true)}
  onfocusout={(e) => (focused = e.relatedTarget instanceof Node && el?.contains(e.relatedTarget) === true)}
>
  <button
    class="pip start"
    class:on={at !== null}
    class:at={at === 0}
    tabindex={at === null || at === 0 ? 0 : -1}
    onclick={() => onpick(0)}
    onkeydown={onkey}
    title={tip(at === 0 && canClear ? "before the first click (click again: as drawn)" : "before the first click")}
    aria-label="before click 1 of {clicks}"
    aria-current={at === 0 ? "step" : undefined}
  ><span aria-hidden="true"></span></button>
  {#each { length: span.last - span.first + 1 } as _, n (n)}
    {@const k = span.first + n}
    <button
      class="pip"
      class:on={at !== null && k <= at}
      class:at={at === k}
      class:more={(n === 0 && k > 1) || (k === span.last && k < clicks)}
      tabindex={at === k ? 0 : -1}
      onclick={() => onpick(k)}
      onkeydown={onkey}
      title={tip(at === k && canClear ? `click ${k} of ${clicks} (click again: as drawn)` : `click ${k} of ${clicks}`)}
      aria-label="click {k} of {clicks}"
      aria-current={at === k ? "step" : undefined}
    ><span aria-hidden="true"></span></button>
  {/each}
</span>

<style>
  /* As wide as their dots (at most the start mark and DOTS). */
  .builds {
    flex: none;
    display: inline-flex;
    align-items: center;
    height: 18px;
  }

  .pip {
    appearance: none;
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 10px;
    height: 18px;
    padding: 0;
    border: none;
    border-radius: 3px;
    background: none;
    cursor: pointer;
  }

  .pip > span {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: color-mix(in srgb, var(--fg) 22%, transparent);
    transition:
      background-color 0.12s ease,
      box-shadow 0.12s ease;
  }

  /* The start mark: a short bar, the slide before its first click. */
  .pip.start > span {
    width: 2px;
    height: 9px;
    border-radius: 1px;
  }

  /* More clicks beyond the window's edge. */
  .pip.more > span {
    width: 3px;
    height: 3px;
  }

  .pip:hover > span {
    background: color-mix(in srgb, var(--fg) 50%, transparent);
  }

  .pip.on > span {
    background: var(--accent);
  }

  .pip.at:not(.start) > span {
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 28%, transparent);
  }

  .pip:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: -1px;
  }

  @media (prefers-reduced-motion: reduce) {
    .pip > span {
      transition: none;
    }
  }
</style>
