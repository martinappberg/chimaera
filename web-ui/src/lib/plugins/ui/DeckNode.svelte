<!--
  The `slides` node: a deck of pictures a plugin rendered (a slide has one
  or more builds), drawn in the slide viewers' chrome. Browsing walks every
  slide and its builds, with the picture in the app's image viewer (zoom, the
  area tool that points an agent at part of a slide) over a thumbnail strip.
  **present** is the show: full screen, a click a build, hidden slides
  skipped. **presenter** is the speaker's side of it: this slide, the next
  click, the notes, a timer.

  Two windows on the same deck keep step while either is presenting or in
  the presenter's view (a BroadcastChannel named for the workspace, plugin
  and the node's `id`): the show on the projector, the presenter's view on
  the laptop. A window that is only browsing neither follows nor leads.

  The place is kept by the slide's title across a re-render (the plugin draws
  again whenever the deck changes): a slide that moved is followed, one that
  went leaves the place where it was. Data and messages: `deck.ts`; paging:
  `previews/slideShow.ts`.
-->
<script lang="ts">
  import { tick, untrack, type Component } from "svelte";
  import { lastRawTicketUrl, rawTicketUrl } from "../../previews/files";
  import { deckStep, showEdge, showPosition, type Place } from "../../previews/slideShow";
  import { pageVisible } from "../../shared/visibility";
  import type { UiNodeData } from "../platform";
  import { clampPlace, clockText, notesAt, parseDeck, placeOf, readSync, syncName, type DeckSync } from "./deck";
  import { str, useScreen } from "./screen";

  let { node }: { node: UiNodeData } = $props();
  const screen = useScreen();

  const slides = $derived(parseDeck(node.slides));
  const hidden = $derived(slides.map((s) => s.hidden));
  const buildCounts = $derived(slides.map((s) => s.builds.length));
  const ratio = $derived.by(() => {
    const w = typeof node.width === "number" && node.width > 0 ? node.width : 16;
    const h = typeof node.height === "number" && node.height > 0 ? node.height : 9;
    return w / h;
  });

  type Mode = "browse" | "present" | "presenter";
  let mode = $state<Mode>("browse");
  let at = $state<Place>({ slide: 0, build: 0 });
  /** The slide we stand on, by name: what a re-render is matched against. */
  let standing = "";
  let startSeen: string | null | undefined;

  // The deck changed under us (a re-render), or the plugin names another
  // slide to open on (`start`): find our place again.
  $effect(() => {
    const all = slides;
    const start = typeof node.start === "string" ? node.start : null;
    untrack(() => {
      let next: Place | null = null;
      if (start !== startSeen) {
        startSeen = start;
        if (start !== null) next = placeOf(all, start, "last");
      }
      next ??= placeOf(all, standing, at.build) ?? clampPlace(all, at);
      at = next;
      standing = all[next.slide]?.title ?? "";
    });
  });

  const slide = $derived(slides[at.slide]);
  const build = $derived(slide?.builds[at.build] ?? slide?.builds[slide.builds.length - 1]);
  const position = $derived(showPosition(hidden, at.slide));
  const notes = $derived(notesAt(slides, at));
  const inShow = $derived(mode !== "browse");
  /** The next click of the show from here: null at its end. */
  const nextPlace = $derived.by(() => {
    const n = deckStep(buildCounts, hidden, at, 1, true);
    return n.slide === at.slide && n.build === at.build ? null : n;
  });

  // --- moving ------------------------------------------------------------------

  const me = Math.random().toString(36).slice(2);
  let channel: BroadcastChannel | null = null;

  function say(msg: DeckSync): void {
    try {
      channel?.postMessage(msg);
    } catch {
      // A closed channel: the other window simply stops following.
    }
  }

  function go(place: Place, tell = true): void {
    const next = clampPlace(slides, place);
    at = next;
    standing = slides[next.slide]?.title ?? "";
    if (tell && mode !== "browse") say({ kind: "at", from: me, title: standing, build: next.build });
  }

  /** One click on or back: a show skips hidden slides, browsing doesn't. */
  function step(delta: 1 | -1): void {
    go(deckStep(buildCounts, hidden, at, delta, inShow));
  }

  function edge(end: "first" | "last"): void {
    const i = inShow ? showEdge(hidden, end) : end === "first" ? 0 : slides.length - 1;
    go({ slide: i, build: end === "first" ? 0 : Math.max(0, (slides[i]?.builds.length ?? 1) - 1) });
  }

  // Two windows on one deck keep step while this one is in the show.
  $effect(() => {
    if (mode === "browse" || typeof BroadcastChannel === "undefined") return;
    const ch = new BroadcastChannel(syncName(screen.ws, screen.plugin, str(node.id, screen.view)));
    ch.onmessage = (e: MessageEvent) => {
      const msg = readSync(e.data);
      if (msg === null || msg.from === me) return;
      if (msg.kind === "where") {
        ch.postMessage({ kind: "at", from: me, title: standing, build: at.build } satisfies DeckSync);
        return;
      }
      const place = placeOf(slides, msg.title, msg.build);
      if (place !== null) go(place, false);
    };
    channel = ch;
    // Joining late: line up with whoever is already in the show.
    ch.postMessage({ kind: "where", from: me } satisfies DeckSync);
    return () => {
      ch.close();
      if (channel === ch) channel = null;
    };
  });

  // --- pictures ----------------------------------------------------------------

  /** A picture's `/raw/` URL by its `src`, minted as it is first wanted. The
   *  daemon keeps a file's ticket while the file is unchanged, and a changed
   *  build has a new `src`, so an entry never goes stale. */
  let urls = $state<Record<string, string>>({});
  let paths = $state<Record<string, string>>({});
  const asked = new Set<string>();
  let pictureError = $state<string | null>(null);

  function want(src: string | undefined): void {
    if (src === undefined || src === "" || asked.has(src)) return;
    asked.add(src);
    screen
      .resolve(src)
      .then((abs) => {
        paths[src] = abs;
        const hit = lastRawTicketUrl(abs);
        if (hit !== null) urls[src] = hit;
        return rawTicketUrl(abs);
      })
      .then(
        (url) => {
          urls[src] = url;
        },
        (e) => {
          asked.delete(src);
          if (src === build?.src) pictureError = e instanceof Error ? e.message : String(e);
        },
      );
  }

  /** A ticket that expired under a long-open deck: mint it again, once. */
  const retried = new Set<string>();
  function broken(src: string): void {
    if (retried.has(src)) return;
    retried.add(src);
    asked.delete(src);
    delete urls[src];
    want(src);
  }

  const nextBuild = $derived(nextPlace === null ? undefined : slides[nextPlace.slide]?.builds[nextPlace.build]);
  $effect(() => {
    pictureError = null;
    want(build?.src);
    // The next click's picture is in the cache before it is asked for.
    want(nextBuild?.src);
  });

  // Entries for builds the deck no longer has go with them.
  $effect(() => {
    const live = new Set(slides.flatMap((s) => s.builds.map((b) => b.src)));
    untrack(() => {
      for (const src of Object.keys(urls)) {
        if (!live.has(src)) {
          delete urls[src];
          delete paths[src];
          asked.delete(src);
        }
      }
    });
  });

  let ImageView = $state<Component<{ path: string }> | null>(null);
  $effect(() => {
    if (ImageView === null && mode === "browse") {
      void import("../../previews/ImageView.svelte").then((m) => (ImageView = m.default));
    }
  });

  // --- the strip ---------------------------------------------------------------

  let strip = $state<HTMLDivElement | null>(null);
  /** Thumbnails load as they come near the strip's window. */
  function nearby(el: HTMLElement, src: string | undefined): { destroy(): void } {
    if (src === undefined || typeof IntersectionObserver === "undefined") {
      want(src);
      return { destroy() {} };
    }
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          want(src);
          io.disconnect();
        }
      },
      { root: strip, rootMargin: "0px 240px" },
    );
    io.observe(el);
    return { destroy: () => io.disconnect() };
  }

  // Keep the current thumbnail in the strip's window, moving only the strip
  // (scrollIntoView would scroll the pane too).
  $effect(() => {
    const i = at.slide;
    const el = strip;
    if (el === null || mode !== "browse") return;
    const thumb = el.children[i] as HTMLElement | undefined;
    if (thumb === undefined) return;
    const left = thumb.offsetLeft - 10;
    const right = thumb.offsetLeft + thumb.offsetWidth + 10;
    if (left < el.scrollLeft) el.scrollLeft = left;
    else if (right > el.scrollLeft + el.clientWidth) el.scrollLeft = right - el.clientWidth;
  });

  // --- the show ----------------------------------------------------------------

  let root = $state<HTMLDivElement | null>(null);
  let showEl = $state<HTMLDivElement | null>(null);

  async function present(): Promise<void> {
    mode = "present";
    await tick();
    const el = showEl;
    if (el === null) return;
    try {
      // Without full screen (a web view that refuses it) the show still
      // covers the window: `.show:not(:fullscreen)`.
      await el.requestFullscreen?.();
    } catch {
      // Refused: the fixed overlay is the show.
    }
    el.focus();
  }

  async function leave(): Promise<void> {
    if (document.fullscreenElement !== null) await document.exitFullscreen().catch(() => {});
    mode = "browse";
    await tick();
    root?.focus();
  }

  // Leaving full screen by the browser's own key ends the show.
  $effect(() => {
    const onChange = () => {
      if (document.fullscreenElement === null && mode === "present") mode = "browse";
    };
    document.addEventListener("fullscreenchange", onChange);
    return () => document.removeEventListener("fullscreenchange", onChange);
  });

  let started = $state(Date.now());
  let now = $state(Date.now());
  function presenter(): void {
    if (mode === "presenter") {
      mode = "browse";
      return;
    }
    started = Date.now();
    now = started;
    mode = "presenter";
    void tick().then(() => root?.focus());
  }
  // The timer ticks only while someone can see it.
  $effect(() => {
    if (mode !== "presenter" || !$pageVisible) return;
    now = Date.now();
    const id = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(id);
  });
  const wallClock = $derived(new Date(now).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }));

  function onKey(e: KeyboardEvent): void {
    if (e.metaKey || e.ctrlKey || e.altKey || slides.length === 0) return;
    const target = e.target as HTMLElement | null;
    // A control inside the viewer keeps its own Enter and Space.
    if ((e.key === "Enter" || e.key === " ") && target !== e.currentTarget && target?.closest("button, a, input, select, textarea") != null) return;
    switch (e.key) {
      case "ArrowRight":
      case "ArrowDown":
      case "PageDown":
      case "Enter":
        step(1);
        break;
      case " ":
        step(e.shiftKey ? -1 : 1);
        break;
      case "ArrowLeft":
      case "ArrowUp":
      case "PageUp":
      case "Backspace":
        step(-1);
        break;
      case "Home":
        edge("first");
        break;
      case "End":
        edge("last");
        break;
      case "f":
      case "F5":
        void (mode === "present" ? leave() : present());
        break;
      case "p":
        if (mode === "present") return;
        presenter();
        break;
      case "Escape":
        if (mode === "browse") return;
        void leave();
        break;
      default:
        return;
    }
    e.preventDefault();
    e.stopPropagation();
  }

  function askAgent(): void {
    const s = slide;
    if (s === undefined || s.file === null) return;
    void screen.act("ask-agent", { file: s.file, line: s.line ?? undefined, text: `On the slide ${s.title}: ` });
  }

  const label = $derived(str(node.label, "Slides"));
  const buildLabel = $derived(slide !== undefined && slide.builds.length > 1 ? `build ${at.build + 1} of ${slide.builds.length}` : "");
  const pictureAlt = $derived([slide?.title ?? "", buildLabel].filter((p) => p !== "").join(", "));
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions — the deck pages by key from its own focus, as the slide viewers' stages do -->
<div
  class="deck"
  class:presenter={mode === "presenter"}
  role="group"
  aria-label={label}
  tabindex="0"
  bind:this={root}
  onkeydown={onKey}
  style:--deck-ratio={ratio}
>
  {#if slides.length === 0}
    <p class="none">No slides yet.</p>
  {:else}
    <div class="bar">
      <button class="bbtn icon" onclick={() => step(-1)} title="previous (←)" aria-label="previous build or slide">
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="M10 3.5 5.5 8 10 12.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
      </button>
      <span class="count" title="slide {at.slide + 1} of {slides.length}">
        {#if inShow}{position.n} / {position.of}{:else}{at.slide + 1} / {slides.length}{/if}
      </span>
      <button class="bbtn icon" onclick={() => step(1)} title="next (→)" aria-label="next build or slide">
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="M6 3.5 10.5 8 6 12.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
      </button>
      <span class="title" title={slide?.title}>{slide?.title}</span>
      {#if slide !== undefined && slide.builds.length > 1}
        <span class="builds" role="group" aria-label="builds">
          {#each slide.builds as _, b (b)}
            <button
              class="dot"
              class:on={b === at.build}
              onclick={() => go({ slide: at.slide, build: b })}
              title="build {b + 1} of {slide.builds.length}"
              aria-label="build {b + 1} of {slide.builds.length}"
              aria-current={b === at.build ? "step" : undefined}
            >{b + 1}</button>
          {/each}
        </span>
      {/if}
      {#if slide?.hidden}<span class="tag" title="a show skips this slide">hidden</span>{/if}
      {#if slide?.tag != null}<span class="tag">{slide.tag}</span>{/if}
      <span class="grow"></span>
      {#if slide?.file != null}
        <button class="bbtn" onclick={askAgent} disabled={screen.busy} title="point an agent at this slide's code">@</button>
      {/if}
      <button class="bbtn" class:on={mode === "presenter"} onclick={presenter} aria-pressed={mode === "presenter"} title="presenter view: this slide, the next, your notes, a timer (P)">presenter</button>
      <button class="bbtn" onclick={() => void present()} title="present full screen; hidden slides are skipped (F)">present</button>
    </div>

    {#if mode === "presenter"}
      <div class="console">
        <div class="now">
          {#if build !== undefined && urls[build.src] !== undefined}
            <img src={urls[build.src]} alt={pictureAlt} onerror={() => broken(build.src)} />
          {:else}
            <p class="none">{slide?.title} has no picture.</p>
          {/if}
        </div>
        <div class="side">
          <div class="next">
            <span class="cap">Next</span>
            {#if nextPlace === null}
              <p class="end">End of the show</p>
            {:else if nextBuild !== undefined && urls[nextBuild.src] !== undefined}
              <img src={urls[nextBuild.src]} alt="next: {slides[nextPlace.slide]?.title ?? ''}" onerror={() => broken(nextBuild.src)} />
            {:else}
              <p class="end">{slides[nextPlace.slide]?.title}</p>
            {/if}
          </div>
          <div class="speak" aria-label="speaker notes">
            {#if notes !== ""}{notes}{:else}<span class="quiet">No notes for this slide.</span>{/if}
          </div>
        </div>
        <div class="foot">
          <button class="timer" onclick={() => (started = Date.now())} title="elapsed; click to start over">{clockText(now - started)}</button>
          <span class="meta">{position.n} / {position.of}{buildLabel === "" ? "" : ` · ${buildLabel}`}</span>
          <span class="grow"></span>
          <span class="meta">{wallClock}</span>
        </div>
      </div>
    {:else}
      <div class="stage">
        {#if pictureError !== null}
          <p class="none bad">{pictureError}</p>
        {:else if build === undefined}
          <p class="none">{slide?.title} has no picture.</p>
        {:else if ImageView !== null && paths[build.src] !== undefined}
          <ImageView path={paths[build.src] ?? ""} />
        {/if}
      </div>
      {#if notes !== ""}
        <div class="notes" aria-label="speaker notes">{notes}</div>
      {/if}
      <div class="strip" bind:this={strip}>
        {#each slides as s, i (i)}
          {@const last = s.builds[s.builds.length - 1]}
          <button
            class="thumb"
            class:on={i === at.slide}
            class:dim={s.hidden}
            onclick={() => go({ slide: i, build: Math.max(0, s.builds.length - 1) })}
            title={s.hidden ? `${s.title} (hidden)` : s.title}
            aria-label="slide {i + 1}: {s.title}{s.hidden ? ' (hidden)' : ''}"
            aria-current={i === at.slide ? "true" : undefined}
            use:nearby={last?.src}
          >
            {#if last !== undefined && urls[last.src] !== undefined}
              <img src={urls[last.src]} alt="" draggable="false" onerror={() => broken(last.src)} />
            {/if}
            <span class="num">{i + 1}</span>
          </button>
        {/each}
      </div>
    {/if}

    {#if mode === "present"}
      <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions, a11y_click_events_have_key_events — the show: a click is the presenter's clicker, the keys are handled beside it -->
      <div class="show" role="document" aria-label="{label}: the show" tabindex="0" bind:this={showEl} onclick={() => step(1)} onkeydown={onKey}>
        {#if build !== undefined && urls[build.src] !== undefined}
          <img src={urls[build.src]} alt={pictureAlt} draggable="false" onerror={() => broken(build.src)} />
        {/if}
        <span class="show-count">{position.n} / {position.of}</span>
      </div>
    {/if}
  {/if}
</div>

<style>
  .deck {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    outline: none;
    background: var(--term-bg);
  }
  .deck:focus-visible {
    box-shadow: inset 0 0 0 1px var(--focus-ring);
  }
  .none {
    margin: auto;
    padding: 1rem;
    color: var(--muted);
    font-size: var(--text-sm);
    text-align: center;
  }
  .none.bad {
    color: var(--err);
  }

  .bar {
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
    white-space: nowrap;
  }
  .title {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg);
  }
  .grow {
    flex: 1;
  }
  .tag {
    flex: none;
    padding: 0 0.4rem;
    border: 1px solid var(--edge);
    border-radius: 4px;
    font-size: 10px;
    letter-spacing: 0.03em;
    white-space: nowrap;
  }
  .bbtn,
  .dot {
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
  .bbtn.icon {
    display: inline-flex;
    align-items: center;
    padding: 0.2rem 0.3rem;
  }
  .bbtn:hover:not(:disabled),
  .dot:hover {
    background: var(--row-hover);
    color: var(--fg);
  }
  .bbtn.on {
    color: var(--accent);
  }
  .bbtn:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .bbtn:focus-visible,
  .dot:focus-visible,
  .timer:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }
  .builds {
    flex: none;
    display: inline-flex;
    gap: 1px;
  }
  .dot {
    min-width: 1.5em;
    padding: 0.05rem 0.3rem;
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
  }
  .dot.on {
    color: var(--fg);
    background: var(--row-hover);
  }

  .stage {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .stage > :global(*) {
    flex: 1;
    min-height: 0;
  }
  .notes {
    flex: none;
    max-height: 22%;
    overflow: auto;
    scrollbar-width: thin;
    padding: 0.5rem 0.9rem;
    border-top: 1px solid var(--edge);
    color: var(--fg);
    font-size: var(--text-sm);
    line-height: 1.5;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .strip {
    flex: none;
    display: flex;
    gap: 8px;
    overflow-x: auto;
    overflow-y: hidden;
    scrollbar-width: thin;
    overscroll-behavior-x: contain;
    padding: 8px 10px;
    border-top: 1px solid var(--edge);
  }
  .thumb {
    position: relative;
    flex: none;
    height: 54px;
    aspect-ratio: var(--deck-ratio);
    appearance: none;
    padding: 0;
    border: 0;
    border-radius: 3px;
    overflow: hidden;
    background: color-mix(in srgb, var(--fg) 8%, var(--term-bg));
    cursor: pointer;
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--fg) 12%, transparent);
    transition: box-shadow 0.12s ease;
  }
  .thumb img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: contain;
    pointer-events: none;
  }
  .thumb.dim img {
    opacity: 0.35;
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

  /* The show: the slide and nothing else. Without full screen it still
     covers the window. */
  .show {
    display: flex;
    align-items: center;
    justify-content: center;
    background: #000;
    outline: none;
    cursor: default;
  }
  .show:not(:fullscreen) {
    position: fixed;
    inset: 0;
    z-index: 1000;
  }
  .show img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: contain;
    user-select: none;
  }
  .show-count {
    position: absolute;
    right: 14px;
    bottom: 10px;
    font-family: var(--mono);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: #fff;
    opacity: 0.45;
  }

  /* The presenter's view. */
  .console {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: minmax(0, 1.9fr) minmax(0, 1fr);
    grid-template-rows: minmax(0, 1fr) auto;
    gap: 10px;
    padding: 10px;
  }
  .now,
  .next {
    min-width: 0;
    min-height: 0;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .now img,
  .next img {
    display: block;
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
    border-radius: 3px;
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--fg) 12%, transparent);
  }
  .side {
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .next {
    flex: none;
    position: relative;
    flex-direction: column;
    align-items: stretch;
    gap: 4px;
    max-height: 40%;
  }
  .next img {
    align-self: center;
    min-height: 0;
  }
  .cap {
    font-size: 10px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .end {
    margin: 0;
    padding: 0.8rem 0;
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .speak {
    flex: 1;
    min-height: 0;
    overflow: auto;
    scrollbar-width: thin;
    padding: 0.6rem 0.8rem;
    border: 1px solid var(--edge);
    border-radius: 6px;
    color: var(--fg);
    font-size: 1.05rem;
    line-height: 1.55;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .quiet {
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .foot {
    grid-column: 1 / -1;
    display: flex;
    align-items: baseline;
    gap: 0.9rem;
    min-width: 0;
  }
  .timer {
    appearance: none;
    border: none;
    background: none;
    padding: 0 0.2rem;
    border-radius: 4px;
    font-family: var(--mono);
    font-size: 1.6rem;
    font-variant-numeric: tabular-nums;
    color: var(--fg);
    cursor: pointer;
  }
  .timer:hover {
    background: var(--row-hover);
  }
  .meta {
    font-family: var(--mono);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    color: var(--muted);
    white-space: nowrap;
  }

  @container plugin-screen (max-width: 560px) {
    .console {
      grid-template-columns: 1fr;
      grid-template-rows: minmax(0, 1.4fr) minmax(0, 1fr) auto;
    }
    .next {
      display: none;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .bbtn,
    .dot,
    .thumb {
      transition: none;
    }
  }
</style>
