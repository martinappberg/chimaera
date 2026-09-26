<script lang="ts">
  /**
   * A PowerPoint deck (.pptx) drawn in the browser by pptx-renderer, in the
   * slides chrome the Marp viewer uses: one slide on the stage, a thumbnail
   * strip, arrow-key paging, a full-screen present mode, `#slide=N` reveals,
   * and the speaker notes under the stage when the deck has any.
   *
   * Slides render lazily: the stage keeps the current slide and its
   * neighbours, and the strip mounts only the thumbnails near its visible
   * span. The package is loaded with the renderer's zip limits for untrusted
   * input and every external load target stripped first (`pptxDeck.ts`), so
   * a deck never fetches anything; web links in it open through the app.
   * Animations and transitions aren't played — the bar says so quietly.
   */
  import { untrack } from "svelte";
  import type { SlideHandle } from "@aiden0z/pptx-renderer";
  import { renderSlide } from "@aiden0z/pptx-renderer";
  import { basename, fsDownload } from "./files";
  import { retain, release, type FileEntry } from "./fileStore.svelte";
  import { fetchRawBytes, TooLargeError } from "./rawBytes";
  import { loadDeck, loadNotes, type Deck } from "./pptxDeck";
  import { sanitizeRendered } from "./officeSafety";
  import { revealRequest, takeReveal } from "../shared/reveal";
  import { activateUrl } from "../shared/urlOpen";
  import { isRemoteHost } from "../net/api";
  import Spinner from "./Spinner.svelte";

  interface Props {
    path: string;
  }

  let { path }: Props = $props();

  const MAX_PPTX_BYTES = 100 * 1024 * 1024;
  const THUMB_H = 62;
  const THUMB_GAP = 8;
  /** Rendered stage slides kept for instant paging back and forth. */
  const STAGE_KEEP = 4;
  /** Thumbnails mounted beyond the strip's visible span, per side. */
  const THUMB_OVERSCAN = 6;

  let entry = $state<FileEntry | null>(null);
  $effect(() => {
    const e = retain(path);
    entry = e;
    void e.ensureMtime();
    return () => release(path);
  });

  let deck = $state.raw<Deck | null>(null);
  let error = $state<string | null>(null);
  let tooLarge = $state(false);
  let current = $state(0);
  let showThumbs = $state(true);
  let showNotes = $state(true);
  let hasNotes = $state(false);
  let presenting = $state(false);
  let nodeErrors = $state(0);
  let pendingSlide: number | null = null;
  /** Bumped when a new deck lands, so the mount effects start over. */
  let deckGen = $state(0);

  /** Blob URLs for the deck's media, shared by every slide render. */
  let mediaUrls = new Map<string, string>();

  function revokeMedia(): void {
    for (const u of mediaUrls.values()) URL.revokeObjectURL(u);
    mediaUrls = new Map();
  }

  let seen: string | null | undefined;
  let gen = 0;
  $effect(() => {
    const m = entry?.mtime ?? null;
    if (seen !== undefined && (m === null || m === seen || seen === null)) {
      if (m !== null) seen = m;
      return;
    }
    seen = m;
    const mine = ++gen;
    void (async () => {
      const bytes = await fetchRawBytes(path, MAX_PPTX_BYTES);
      let d: Deck;
      try {
        d = await loadDeck(bytes);
      } catch (e) {
        const msg = e instanceof Error ? e.message : "";
        throw new Error(
          /limit exceeded/i.test(msg)
            ? `this deck is past the viewer's safety limits (${msg.replace(/^.*limit exceeded:\s*/i, "")})`
            : "this file couldn't be read as a PowerPoint deck (legacy .ppt files aren't supported)",
        );
      }
      if (d.presentation.slides.length === 0) throw new Error("this deck has no slides");
      return { d, bytes };
    })().then(
      ({ d, bytes }) => {
        if (mine !== gen) return;
        disposeAll();
        revokeMedia();
        deck = d;
        deckGen += 1;
        error = null;
        tooLarge = false;
        nodeErrors = 0;
        hasNotes = false;
        const want = pendingSlide ?? untrack(() => current);
        pendingSlide = null;
        current = Math.min(Math.max(0, want), d.presentation.slides.length - 1);
        void loadNotes(bytes, d).then(
          (any) => {
            if (mine === gen) hasNotes = any;
          },
          () => {},
        );
      },
      (e: unknown) => {
        if (mine !== gen) return;
        tooLarge = e instanceof TooLargeError;
        error = e instanceof Error ? e.message : "the deck could not be opened";
      },
    );
  });

  $effect(() => () => {
    disposeAll();
    revokeMedia();
  });

  const count = $derived(deck?.presentation.slides.length ?? 0);
  const w = $derived(deck?.presentation.width ?? 960);
  const h = $derived(deck?.presentation.height ?? 540);

  // --- rendering ------------------------------------------------------------------

  function render(i: number): SlideHandle | null {
    const d = deck;
    if (d === null) return null;
    const slide = d.presentation.slides[i];
    if (slide === undefined) return null;
    try {
      const handle = renderSlide(d.presentation, slide, {
        mediaUrlCache: mediaUrls,
        pdfjs: false,
        onNodeError: () => (nodeErrors += 1),
        onNavigate: (t) => {
          if (t.slideIndex !== undefined) go(t.slideIndex);
          else if (t.url !== undefined) activateUrl(t.url);
        },
      });
      // Defense in depth: the package was stripped of remote targets, and
      // this makes sure nothing in the laid-out slide still points out.
      sanitizeRendered([handle.element]);
      return handle;
    } catch {
      nodeErrors += 1;
      return null;
    }
  }

  /** Stage handles by slide index, most recently used last. */
  const stageHandles = new Map<number, SlideHandle>();
  const thumbHandles = new Map<number, SlideHandle>();

  function disposeAll(): void {
    for (const h of [...stageHandles.values(), ...thumbHandles.values()]) {
      h.dispose();
      h.element.remove();
    }
    stageHandles.clear();
    thumbHandles.clear();
  }

  /** A slide for the stage, rendered into the slot (hidden) if it isn't
   *  there yet. Slides must stay attached: the renderer finishes text layout
   *  asynchronously (fonts, autofit), and a detached slide measures as empty
   *  and loses its text. */
  function stageHandle(i: number, into: HTMLElement): SlideHandle | null {
    const hit = stageHandles.get(i);
    if (hit !== undefined) {
      stageHandles.delete(i);
      stageHandles.set(i, hit);
      return hit;
    }
    const made = render(i);
    if (made === null) return null;
    made.element.classList.add("stage-slide");
    made.element.style.visibility = "hidden";
    into.appendChild(made.element);
    stageHandles.set(i, made);
    while (stageHandles.size > STAGE_KEEP) {
      const [old, handle] = stageHandles.entries().next().value as [number, SlideHandle];
      if (old === i) break;
      handle.dispose();
      handle.element.remove();
      stageHandles.delete(old);
    }
    return made;
  }

  let slot = $state<HTMLDivElement | null>(null);

  // Show the current slide; then warm its neighbour in the idle time after.
  $effect(() => {
    const el = slot;
    const i = current;
    void deckGen;
    if (el === null || untrack(() => deck) === null) return;
    const handle = untrack(() => stageHandle(i, el));
    for (const [j, h] of stageHandles) h.element.style.visibility = j === i ? "visible" : "hidden";
    if (handle === null) return;
    const warm = setTimeout(() => {
      if (i + 1 < untrack(() => count)) untrack(() => stageHandle(i + 1, el));
    }, 120);
    return () => clearTimeout(warm);
  });

  // `#slide=N`: on mount and whenever a reveal for this deck arrives.
  $effect(() => {
    void $revealRequest;
    const req = takeReveal(path);
    if (req?.slide === undefined) return;
    const i = req.slide - 1;
    if (untrack(() => deck) === null) pendingSlide = i;
    else go(i);
  });

  function go(i: number): void {
    if (deck === null) return;
    current = Math.min(Math.max(0, i), count - 1);
  }

  let stageW = $state(0);
  let stageH = $state(0);
  const pad = $derived(presenting ? 0 : 20);
  const scale = $derived(
    stageW > 0 && stageH > 0 ? Math.max(0.05, Math.min((stageW - pad * 2) / w, (stageH - pad * 2) / h)) : 0,
  );
  const thumbW = $derived(Math.round((THUMB_H * w) / h));
  const thumbScale = $derived(THUMB_H / h);

  let stageEl = $state<HTMLDivElement | null>(null);
  let stripEl = $state<HTMLDivElement | null>(null);

  // --- thumbnails -------------------------------------------------------------------

  let stripLeft = $state(0);
  let stripWidth = $state(0);
  const thumbRange = $derived.by(() => {
    const span = thumbW + THUMB_GAP;
    const first = Math.max(0, Math.floor(stripLeft / span) - THUMB_OVERSCAN);
    const last = Math.min(count - 1, Math.ceil((stripLeft + stripWidth) / span) + THUMB_OVERSCAN);
    return { first, last };
  });

  /** Drop the thumbnails out of range, then mount a couple of the missing
   *  ones; true when more are still missing. */
  function syncThumbs(): boolean {
    const strip = stripEl;
    if (strip === null || deck === null) return false;
    const { first, last } = thumbRange;
    for (const [i, handle] of thumbHandles) {
      if (i < first || i > last) {
        handle.dispose();
        handle.element.remove();
        thumbHandles.delete(i);
      }
    }
    // Nearest the current slide first; two per tick so paging stays smooth.
    const missing: number[] = [];
    for (let i = first; i <= last; i++) if (!thumbHandles.has(i)) missing.push(i);
    missing.sort((a, b) => Math.abs(a - current) - Math.abs(b - current));
    for (const i of missing.slice(0, 2)) {
      const box = strip.querySelector<HTMLElement>(`[data-thumb="${i}"] .thumb-slide`);
      if (box === null) continue;
      const handle = render(i);
      if (handle === null) continue;
      thumbHandles.set(i, handle);
      box.replaceChildren(handle.element);
    }
    return missing.length > 2;
  }

  $effect(() => {
    void thumbRange;
    void deckGen;
    if (!showThumbs || stripEl === null) return;
    // After the stage: thumbnails never hold up the slide being read.
    let t = setTimeout(function pump() {
      if (untrack(syncThumbs)) t = setTimeout(pump, 16);
    }, 60);
    return () => clearTimeout(t);
  });

  $effect(() => {
    const strip = stripEl;
    if (strip === null) return;
    const ro = new ResizeObserver(() => (stripWidth = strip.clientWidth));
    ro.observe(strip);
    stripWidth = strip.clientWidth;
    return () => ro.disconnect();
  });

  $effect(() => {
    if (!showThumbs) {
      for (const handle of thumbHandles.values()) handle.dispose();
      thumbHandles.clear();
    }
  });

  // Keep the current thumbnail in view as the deck is paged.
  $effect(() => {
    const strip = stripEl;
    const i = current;
    if (strip === null || !showThumbs) return;
    const left = i * (thumbW + THUMB_GAP);
    const right = left + thumbW;
    if (left < strip.scrollLeft + 8) strip.scrollTo({ left: Math.max(0, left - 24), behavior: "smooth" });
    else if (right > strip.scrollLeft + strip.clientWidth - 8) {
      strip.scrollTo({ left: right - strip.clientWidth + 24, behavior: "smooth" });
    }
  });

  // --- keys, present -------------------------------------------------------------

  function onKey(e: KeyboardEvent): void {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    if (deck === null) return;
    switch (e.key) {
      case "ArrowRight":
      case "ArrowDown":
      case "PageDown":
      case " ":
      case "Enter":
        go(current + (e.shiftKey && e.key === " " ? -1 : 1));
        break;
      case "ArrowLeft":
      case "ArrowUp":
      case "PageUp":
      case "Backspace":
        go(current - 1);
        break;
      case "Home":
        go(0);
        break;
      case "End":
        go(count - 1);
        break;
      case "f":
      case "F5":
        void togglePresent();
        break;
      case "Escape":
        if (!presenting) return;
        void togglePresent();
        break;
      default:
        return;
    }
    e.preventDefault();
  }

  async function togglePresent(): Promise<void> {
    const el = stageEl;
    if (el === null) return;
    if (presenting) {
      if (document.fullscreenElement !== null) await document.exitFullscreen().catch(() => {});
      presenting = false;
      return;
    }
    presenting = true;
    el.focus();
    try {
      await el.requestFullscreen?.();
    } catch {
      // Not allowed here (some webviews): the window-covering stage stands in.
    }
  }

  $effect(() => {
    const onChange = () => {
      if (document.fullscreenElement === null && presenting && stageEl !== null) presenting = false;
    };
    document.addEventListener("fullscreenchange", onChange);
    return () => document.removeEventListener("fullscreenchange", onChange);
  });

  /** Web links inside a slide open through the app, never in this page. */
  function onStageClick(e: MouseEvent): void {
    const a = (e.target as Element | null)?.closest?.("a[href]");
    if (a === null || a === undefined) return;
    e.preventDefault();
    const href = a.getAttribute("href") ?? "";
    if (/^https?:/i.test(href)) activateUrl(href, e.metaKey || e.ctrlKey);
  }

  const remote = isRemoteHost();
  let downloadError = $state<string | null>(null);
  async function download(): Promise<void> {
    downloadError = null;
    try {
      await fsDownload(path);
    } catch (e) {
      downloadError = e instanceof Error ? e.message : "download failed";
    }
  }

  const note = $derived(deck !== null && hasNotes ? (deck.notes[current] ?? "") : "");
  /** This slide's notes part was over the reader's cap and never inflated. */
  const noteSkipped = $derived(deck !== null && hasNotes && deck.notesSkipped.has(current));
  const hiddenSlide = $derived(deck?.presentation.slides[current]?.hidden === true);
</script>

<div class="pptx-view">
  <div class="slides-bar">
    {#if deck !== null}
      <button class="bbtn icon" onclick={() => go(current - 1)} disabled={current === 0} title="previous slide (←)" aria-label="previous slide">
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="M10 3.5 5.5 8 10 12.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
      </button>
      <span class="count" aria-live="polite">{current + 1} / {count}</span>
      <button class="bbtn icon" onclick={() => go(current + 1)} disabled={current >= count - 1} title="next slide (→)" aria-label="next slide">
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="M6 3.5 10.5 8 6 12.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
      </button>
      {#if hiddenSlide}<span class="tag" title="this slide is hidden in the slide show">hidden</span>{/if}
      <span
        class="limits"
        title="drawn in the browser: no animations or transitions, and fonts the deck doesn't embed fall back to this system's{deck.blocked > 0
          ? `; ${deck.blocked} linked outside ${deck.blocked === 1 ? 'resource was' : 'resources were'} not loaded`
          : ''}{nodeErrors > 0 ? `; ${nodeErrors} ${nodeErrors === 1 ? 'shape' : 'shapes'} couldn't be drawn` : ''}"
        aria-label="rendering limits"
      >
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"
          ><circle cx="8" cy="8" r="6.2" fill="none" stroke="currentColor" stroke-width="1.3" /><path d="M8 7.2v4M8 4.9v.1" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" /></svg
        >
        {#if nodeErrors > 0 || deck.blocked > 0}<span>approximate</span>{/if}
      </span>
    {/if}
    {#if downloadError !== null}<span class="bar-err">{downloadError}</span>{/if}
    <span class="spacer"></span>
    {#if deck !== null}
      {#if hasNotes}
        <button class="bbtn" class:on={showNotes} onclick={() => (showNotes = !showNotes)} title="show or hide the speaker notes">notes</button>
      {/if}
      <button class="bbtn" class:on={showThumbs} onclick={() => (showThumbs = !showThumbs)} title="show or hide the thumbnail strip">thumbnails</button>
      <button class="bbtn" onclick={() => void togglePresent()} title="present full screen (F)">present</button>
    {/if}
    {#if remote}
      <button class="bbtn" onclick={() => void download()} title="download {basename(path)} to open it in PowerPoint">download</button>
    {/if}
  </div>

  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="stage"
    class:presenting
    bind:this={stageEl}
    bind:clientWidth={stageW}
    bind:clientHeight={stageH}
    tabindex="0"
    role="region"
    aria-label="slides"
    onkeydown={onKey}
    onclick={onStageClick}
  >
    {#if deck !== null && scale > 0}
      <div class="slide-box" style:width="{w * scale}px" style:height="{h * scale}px">
        <div class="slide-scale" style:width="{w}px" style:height="{h}px" style:transform="scale({scale})" bind:this={slot}></div>
      </div>
      {#if presenting}
        <span class="present-count">{current + 1} / {count}</span>
      {/if}
    {:else if error !== null}
      <div class="slides-error">
        <span>{error}</span>
        {#if tooLarge && remote}<button class="opt" onclick={() => void download()}>download</button>{/if}
      </div>
    {:else if deck === null}
      <Spinner label="reading the deck" />
    {/if}
    {#if error !== null && deck !== null}
      <div class="stale-note" role="status">{error} — showing the last good render</div>
    {/if}
  </div>

  {#if deck !== null && showNotes && (note !== "" || noteSkipped) && !presenting}
    {#if noteSkipped}
      <div class="notes skipped" role="note" aria-label="speaker notes">this slide's notes are too large to show here</div>
    {:else}
      <div class="notes" role="note" aria-label="speaker notes">{note}</div>
    {/if}
  {/if}

  {#if deck !== null && showThumbs && count > 1}
    <div class="strip" bind:this={stripEl} onscroll={() => stripEl !== null && (stripLeft = stripEl.scrollLeft)}>
      <div class="strip-inner" style:width="{count * (thumbW + THUMB_GAP) - THUMB_GAP}px" style:height="{THUMB_H}px">
        {#each { length: count } as _, i (i)}
          <button
            class="thumb"
            class:on={i === current}
            class:dim={deck.presentation.slides[i]?.hidden === true}
            data-thumb={i}
            style:left="{i * (thumbW + THUMB_GAP)}px"
            style:width="{thumbW}px"
            onclick={() => {
              go(i);
              stageEl?.focus();
            }}
            aria-label="slide {i + 1}"
            aria-current={i === current ? "true" : undefined}
          >
            <span class="thumb-slide" style:width="{w}px" style:height="{h}px" style:transform="scale({thumbScale})"></span>
            <span class="num">{i + 1}</span>
          </button>
        {/each}
      </div>
    </div>
  {/if}
</div>

<style>
  .pptx-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  .slides-bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.35rem;
    height: 26px;
    padding: 0 0.5rem 0 0.4rem;
    border-bottom: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
    min-width: 0;
  }

  .count {
    min-width: 4.5ch;
    text-align: center;
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
    color: var(--fg);
  }

  .tag {
    padding: 0 0.4rem;
    border: 1px solid var(--edge);
    border-radius: 4px;
    font-size: 10px;
    letter-spacing: 0.03em;
  }

  .limits {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    margin-left: 0.3rem;
    color: var(--muted);
    opacity: 0.8;
    cursor: help;
  }

  .bar-err {
    color: var(--err);
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .spacer {
    flex: 1;
  }

  .bbtn,
  .opt {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    padding: 0.1rem 0.45rem;
    border-radius: 4px;
    white-space: nowrap;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .opt {
    border: 1px solid var(--edge);
    padding: 0.2rem 0.7rem;
  }

  .bbtn.icon {
    display: inline-flex;
    align-items: center;
    padding: 0.2rem 0.3rem;
  }

  .bbtn:hover:not(:disabled),
  .opt:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .bbtn.on {
    color: var(--fg);
  }

  .bbtn:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .stage {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    overflow: hidden;
    background: color-mix(in srgb, var(--fg) 4%, var(--term-bg));
    outline: none;
  }

  .stage:focus-visible {
    box-shadow: inset 0 0 0 1px var(--focus-ring);
  }

  .stage.presenting {
    background: #000;
  }

  .stage.presenting:not(:fullscreen) {
    position: fixed;
    inset: 0;
    z-index: 1000;
  }

  .slide-box {
    position: relative;
    flex: none;
    overflow: hidden;
    border-radius: 3px;
    background: #fff;
    box-shadow:
      0 0 0 1px color-mix(in srgb, var(--fg) 10%, transparent),
      0 2px 12px color-mix(in srgb, var(--fg) 14%, transparent);
  }

  .presenting .slide-box {
    border-radius: 0;
    box-shadow: none;
  }

  /* Slides are authored on white in their own light scheme, and text the
     deck leaves unstyled is black — never the theme's inherited text color
     (light in a dark theme), font or spacing. */
  .slide-scale,
  .thumb-slide {
    color-scheme: light;
    color: #000;
    font: initial;
    line-height: normal;
    letter-spacing: normal;
    text-align: left;
    text-transform: none;
  }

  .slide-scale {
    position: relative;
    transform-origin: 0 0;
  }

  /* Kept slides stack in the slot; only the current one is visible. */
  .slide-scale :global(.stage-slide) {
    position: absolute !important;
    left: 0;
    top: 0;
  }

  .present-count {
    position: absolute;
    right: 14px;
    bottom: 10px;
    font-family: var(--mono);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: #fff;
    opacity: 0.45;
  }

  .slides-error {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.5rem;
    color: var(--muted);
    font-size: var(--text-md);
    padding: 1rem;
    text-align: center;
  }

  .stale-note {
    position: absolute;
    left: 50%;
    bottom: 10px;
    transform: translateX(-50%);
    max-width: calc(100% - 20px);
    padding: 0.25rem 0.6rem;
    border: 1px solid color-mix(in srgb, var(--warn) 40%, var(--edge));
    border-radius: 6px;
    background: var(--overlay-bg);
    color: var(--warn);
    font-size: var(--text-xs);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .notes {
    flex: none;
    max-height: 22%;
    overflow: auto;
    scrollbar-width: thin;
    padding: 0.5rem 0.9rem;
    border-top: 1px solid var(--edge);
    background: var(--term-bg);
    color: var(--fg);
    font-size: var(--text-sm);
    line-height: 1.5;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .notes.skipped {
    color: var(--muted);
    font-style: italic;
  }

  .strip {
    flex: none;
    overflow-x: auto;
    overflow-y: hidden;
    scrollbar-width: thin;
    overscroll-behavior-x: contain;
    padding: 8px 10px;
    border-top: 1px solid var(--edge);
    background: var(--term-bg);
  }

  .strip-inner {
    position: relative;
    margin: 0 auto;
  }

  .thumb {
    position: absolute;
    top: 0;
    bottom: 0;
    appearance: none;
    padding: 0;
    border: 0;
    border-radius: 3px;
    overflow: hidden;
    background: #fff;
    cursor: pointer;
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--fg) 12%, transparent);
    transition: box-shadow 0.12s ease;
  }

  .thumb.dim {
    opacity: 0.55;
  }

  .thumb:hover {
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--fg) 35%, transparent);
  }

  .thumb.on {
    box-shadow:
      0 0 0 2px var(--accent),
      0 0 0 3px color-mix(in srgb, var(--accent) 25%, transparent);
  }

  .thumb:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 2px;
  }

  .thumb-slide {
    position: absolute;
    left: 0;
    top: 0;
    display: block;
    transform-origin: 0 0;
    pointer-events: none;
  }

  .num {
    position: absolute;
    right: 3px;
    bottom: 2px;
    padding: 0 3px;
    border-radius: 3px;
    background: color-mix(in srgb, var(--term-bg) 80%, transparent);
    color: var(--muted);
    font-family: var(--mono);
    font-size: 9px;
    line-height: 1.4;
    font-variant-numeric: tabular-nums;
  }

  .thumb.on .num {
    color: var(--accent);
  }

  @media (prefers-reduced-motion: reduce) {
    .bbtn,
    .opt,
    .thumb {
      transition: none;
    }
  }
</style>
