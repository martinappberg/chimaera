<script lang="ts">
  /**
   * A Marp deck as slides: one slide on the stage, a thumbnail strip under
   * it, arrow-key navigation, a full-screen present mode, and print (the
   * browser's print-to-PDF gets one slide per page).
   *
   * Marp renders in the browser (`html: false`, no script) and the slides are
   * drawn in sandboxed iframes with no scripts at all. The stage iframe holds
   * the whole deck stacked at native size; showing a slide just moves and
   * scales that frame, so paging is instant and every slide keeps its exact
   * layout at any pane size (drawing at native size also sidesteps WebKit's
   * foreignObject scaling bug without Marp's script polyfill). Relative images
   * resolve through `/raw` tickets before rendering. A disk change re-renders
   * in place and keeps the current slide.
   */
  import { untrack, type Snippet } from "svelte";
  import { fsFile, humanSize, rawTicketUrl, resolveDocPath, safeDecodeUri } from "./files";
  import { retain, release, type FileEntry } from "./fileStore.svelte";
  import { relativeImages, rewriteImages, slideCount, slideDocument, slideSize } from "./marp";
  import { revealRequest, takeReveal } from "../shared/reveal";
  import Spinner from "./Spinner.svelte";

  interface Props {
    path: string;
    /** The host's view switch (slides ⇄ markdown), drawn at the bar's end. */
    switcher?: Snippet;
  }

  let { path, switcher }: Props = $props();

  /** Largest deck source rendered; one `fs/file` read. */
  const MAX_DECK_BYTES = 2 * 1024 * 1024;
  /** Most distinct relative images resolved per render (one ticket each). */
  const MAX_IMAGES = 200;
  /** Thumbnail height (px) and the gap between thumbnails (px). */
  const THUMB_H = 62;
  const THUMB_GAP = 8;

  interface Deck {
    css: string;
    html: string;
    size: { w: number; h: number };
    count: number;
  }

  let entry = $state<FileEntry | null>(null);
  $effect(() => {
    const e = retain(path);
    entry = e;
    void e.ensureMtime();
    return () => release(path);
  });

  let deck = $state<Deck | null>(null);
  let error = $state<string | null>(null);
  let current = $state(0);
  let showThumbs = $state(true);
  let presenting = $state(false);
  /** A `#slide=N` that arrived before the deck rendered (0-based). */
  let pendingSlide: number | null = null;

  type MarpModule = typeof import("@marp-team/marp-core");
  let marpLoad: Promise<MarpModule> | null = null;

  async function build(p: string): Promise<Deck> {
    const chunk = await fsFile(p, 0, MAX_DECK_BYTES);
    if (chunk.truncated) {
      throw new Error(`this deck is over ${humanSize(MAX_DECK_BYTES)} — open it as markdown`);
    }
    let source = new TextDecoder().decode(chunk.bytes);
    const urls = new Map<string, string>();
    await Promise.all(
      relativeImages(source)
        .slice(0, MAX_IMAGES)
        .map(async (target) => {
          try {
            urls.set(target, await rawTicketUrl(resolveDocPath(p, safeDecodeUri(target))));
          } catch {
            // A missing image draws as a broken one, like any markdown view.
          }
        }),
    );
    source = rewriteImages(source, urls);
    marpLoad ??= import("@marp-team/marp-core");
    let mod: MarpModule;
    try {
      mod = await marpLoad;
    } catch (e) {
      marpLoad = null;
      throw e;
    }
    // html:false escapes raw HTML in the deck; script:false drops Marp's
    // runtime helper (the frames could not run it anyway). Math is KaTeX as
    // MathML (the app's policy: no font files, nothing to fetch), and emoji
    // stay text — Marp's defaults would load both from a CDN.
    const marp = new mod.Marp({
      html: false,
      script: false,
      inlineSVG: true,
      math: { lib: "katex", katexOption: { output: "mathml", trust: false, maxExpand: 1000 }, katexFontPath: false },
      emoji: { shortcode: true, unicode: false },
    });
    const { html, css } = marp.render(source);
    return {
      // KaTeX's @font-face rules would only 404 against the daemon: MathML
      // output draws with the system's math font.
      css: css.replace(/@font-face\s*\{[^}]*KaTeX[^}]*\}/g, ""),
      html,
      size: slideSize(html),
      count: Math.max(1, slideCount(html)),
    };
  }

  // (Re)render on open and on every on-disk version (the token arriving for
  // the first time is not a change). The previous deck stays on screen until
  // the new one is ready — an agent rewriting the deck never flashes a
  // spinner over it.
  let seen: string | null | undefined;
  /** Only the newest render lands (a slow one never overwrites a later one). */
  let renderGen = 0;
  $effect(() => {
    const m = entry?.mtime ?? null;
    if (seen !== undefined && (m === null || m === seen || seen === null)) {
      if (m !== null) seen = m;
      return;
    }
    seen = m;
    const gen = ++renderGen;
    void build(path).then(
      (d) => {
        if (gen !== renderGen) return;
        deck = d;
        error = null;
        const want = pendingSlide ?? untrack(() => current);
        pendingSlide = null;
        current = Math.min(Math.max(0, want), d.count - 1);
      },
      (e: unknown) => {
        if (gen !== renderGen) return;
        error = e instanceof Error ? e.message : "failed to render the deck";
      },
    );
  });

  // `#slide=N`: on mount and whenever a reveal for this deck arrives.
  $effect(() => {
    void $revealRequest;
    const req = takeReveal(path);
    if (req?.slide === undefined) return;
    const i = req.slide - 1;
    const d = untrack(() => deck);
    if (d === null) pendingSlide = i;
    else go(i);
  });

  function go(i: number): void {
    const d = deck;
    if (d === null) return;
    current = Math.min(Math.max(0, i), d.count - 1);
  }

  const w = $derived(deck?.size.w ?? 1280);
  const h = $derived(deck?.size.h ?? 720);
  const count = $derived(deck?.count ?? 0);

  // Both documents are rebuilt only when the render changes, never on a
  // slide change, so paging never reloads a frame.
  const stageDoc = $derived(deck === null ? "" : slideDocument(deck.css, deck.html, deck.size, "column"));
  const thumbGap = $derived(Math.round((THUMB_GAP * h) / THUMB_H));
  const stripDoc = $derived(
    deck === null ? "" : slideDocument(deck.css, deck.html, deck.size, "row", thumbGap),
  );

  let stageW = $state(0);
  let stageH = $state(0);
  const pad = $derived(presenting ? 0 : 20);
  const scale = $derived(
    stageW > 0 && stageH > 0 ? Math.max(0.05, Math.min((stageW - pad * 2) / w, (stageH - pad * 2) / h)) : 0,
  );
  const thumbScale = $derived(THUMB_H / h);

  let stageEl = $state<HTMLDivElement | null>(null);
  let stripEl = $state<HTMLDivElement | null>(null);

  // Keep the current thumbnail in view as the deck is paged.
  $effect(() => {
    const strip = stripEl;
    const i = current;
    const k = thumbScale;
    if (strip === null || !showThumbs) return;
    const left = i * (w + thumbGap) * k;
    const right = left + w * k;
    if (left < strip.scrollLeft + 8) strip.scrollTo({ left: Math.max(0, left - 24), behavior: "smooth" });
    else if (right > strip.scrollLeft + strip.clientWidth - 8) {
      strip.scrollTo({ left: right - strip.clientWidth + 24, behavior: "smooth" });
    }
  });

  function onKey(e: KeyboardEvent): void {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    const d = deck;
    if (d === null) return;
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
        go(d.count - 1);
        break;
      case "f":
      case "F5":
        void togglePresent();
        break;
      case "Escape":
        // Browsers usually take Esc to leave full screen themselves; where
        // the key reaches the page instead, leave it here.
        if (!presenting) return;
        void togglePresent();
        break;
      default:
        return;
    }
    e.preventDefault();
  }

  /** Full screen when the webview allows it; otherwise the stage covers the
   *  window (same look, Esc leaves either way). */
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
      if (document.fullscreenElement === null && presenting && stageEl !== null) {
        // Leaving browser full screen (Esc) ends the presentation too.
        presenting = false;
      }
    };
    document.addEventListener("fullscreenchange", onChange);
    return () => document.removeEventListener("fullscreenchange", onChange);
  });

  /** Print through the browser's dialog (save as PDF there): a one-off frame
   *  with one slide per page. It must be same-origin to be told to print,
   *  so it stays script-free — the sandbox grants the print dialog only. */
  let printing = $state(false);
  function print(): void {
    const d = deck;
    if (d === null || printing) return;
    printing = true;
    const frame = document.createElement("iframe");
    frame.setAttribute("sandbox", "allow-same-origin allow-modals");
    frame.setAttribute("aria-hidden", "true");
    frame.tabIndex = -1;
    frame.style.cssText = "position:fixed;right:0;bottom:0;width:0;height:0;border:0;opacity:0";
    frame.srcdoc = slideDocument(d.css, d.html, d.size, "print");
    const done = () => {
      frame.remove();
      printing = false;
    };
    frame.addEventListener(
      "load",
      () => {
        const win = frame.contentWindow;
        // Images inside the slides decode after load; give them a beat.
        setTimeout(() => {
          try {
            win?.focus();
            win?.print();
          } finally {
            setTimeout(done, 1000);
          }
        }, 300);
      },
      { once: true },
    );
    document.body.appendChild(frame);
  }
</script>

<div class="slides-view">
  <div class="slides-bar">
    {#if deck !== null}
      <button class="bbtn icon" onclick={() => go(current - 1)} disabled={current === 0} title="previous slide (←)" aria-label="previous slide">
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="M10 3.5 5.5 8 10 12.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
      </button>
      <span class="count" aria-live="polite">{current + 1} / {count}</span>
      <button class="bbtn icon" onclick={() => go(current + 1)} disabled={current >= count - 1} title="next slide (→)" aria-label="next slide">
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="M6 3.5 10.5 8 6 12.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
      </button>
    {/if}
    <span class="spacer"></span>
    {#if deck !== null}
      <button class="bbtn" class:on={showThumbs} onclick={() => (showThumbs = !showThumbs)} title="show or hide the thumbnail strip">thumbnails</button>
      <button class="bbtn" onclick={print} disabled={printing} title="print, or save as PDF from the print dialog">print</button>
      <button class="bbtn" onclick={() => void togglePresent()} title="present full screen (F)">present</button>
    {/if}
    {@render switcher?.()}
  </div>

  <!-- Keyboard paging lives on the stage; the frames never take focus
       (pointer-events off), so the arrows always reach it. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
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
  >
    {#if deck !== null && scale > 0}
      <div class="slide-box" style:width="{w * scale}px" style:height="{h * scale}px">
        <iframe
          title="slide {current + 1} of {count}"
          sandbox=""
          srcdoc={stageDoc}
          tabindex="-1"
          style:width="{w}px"
          style:height="{h * count}px"
          style:transform="scale({scale}) translateY({-current * h}px)"
        ></iframe>
      </div>
      {#if presenting}
        <span class="present-count">{current + 1} / {count}</span>
      {/if}
    {:else if error !== null}
      <div class="slides-error">{error}</div>
    {:else if deck === null}
      <Spinner label="rendering slides" />
    {/if}
    {#if error !== null && deck !== null}
      <div class="stale-note" role="status">{error} — showing the last good render</div>
    {/if}
  </div>

  {#if deck !== null && showThumbs && count > 1}
    <div class="strip" bind:this={stripEl}>
      <div
        class="strip-inner"
        style:width="{(count * (w + thumbGap) - thumbGap) * thumbScale}px"
        style:height="{THUMB_H}px"
      >
        <iframe
          title="thumbnails"
          aria-hidden="true"
          sandbox=""
          srcdoc={stripDoc}
          tabindex="-1"
          style:width="{count * (w + thumbGap) - thumbGap}px"
          style:height="{h}px"
          style:transform="scale({thumbScale})"
        ></iframe>
        {#each { length: count } as _, i (i)}
          <button
            class="thumb"
            class:on={i === current}
            style:left="{i * (w + thumbGap) * thumbScale}px"
            style:width="{w * thumbScale}px"
            onclick={() => {
              go(i);
              stageEl?.focus();
            }}
            aria-label="slide {i + 1}"
            aria-current={i === current ? "true" : undefined}
          ><span class="num">{i + 1}</span></button>
        {/each}
      </div>
    </div>
  {/if}
</div>

<style>
  .slides-view {
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
  }

  .count {
    min-width: 4.5ch;
    text-align: center;
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
    color: var(--fg);
  }

  .spacer {
    flex: 1;
  }

  .bbtn {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    padding: 0.1rem 0.45rem;
    border-radius: 4px;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .bbtn.icon {
    display: inline-flex;
    align-items: center;
    padding: 0.2rem 0.3rem;
  }

  .bbtn:hover:not(:disabled) {
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

  /* Present: the whole screen (Fullscreen API) or, where a webview refuses
     it, the whole window. Slides sit on black whatever the theme. */
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
    box-shadow:
      0 0 0 1px color-mix(in srgb, var(--fg) 10%, transparent),
      0 2px 12px color-mix(in srgb, var(--fg) 14%, transparent);
  }

  .presenting .slide-box {
    border-radius: 0;
    box-shadow: none;
  }

  iframe {
    display: block;
    border: 0;
    background: transparent;
    transform-origin: 0 0;
    pointer-events: none;
    /* Matches the decks' own (light) scheme: a frame whose scheme differs
       from its document gets an opaque backdrop, which would fill the gaps
       between thumbnails in the dark theme. */
    color-scheme: light;
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
    background: transparent;
    cursor: pointer;
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--fg) 12%, transparent);
    transition: box-shadow 0.12s ease;
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
    .thumb {
      transition: none;
    }
  }
</style>
