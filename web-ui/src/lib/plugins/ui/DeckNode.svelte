<!--
  The `slides` node: a deck of pictures a plugin rendered (a slide has one
  or more builds), drawn in the slide viewers' chrome. Browsing walks every
  slide and its builds, with the picture in the app's image viewer (zoom, the
  area tool that points an agent at part of a slide) over a thumbnail strip.
  **present** is the show: full screen, a click a build, hidden slides
  skipped. **presenter** is the speaker's side of it: this slide, the next
  click, the notes, a timer.

  A build may carry a `clip`, the animation that ends on its picture. The
  show plays it when a click moves forward onto that build and then holds
  its last frame; a move back, or a jump, shows the picture. Browsing plays
  it only from the bar's play button.

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
  import { copyTabToWindow, type UiNodeData } from "../platform";
  import { clampPlace, clockText, notesAt, parseDeck, placeOf, readSync, syncName, type DeckSync } from "./deck";
  import { str, useScreen } from "./screen";

  let { node }: { node: UiNodeData } = $props();
  const screen = useScreen();

  /** The view went: a ticket or a play() answered after that changes nothing. */
  let disposed = false;
  $effect(() => () => {
    disposed = true;
  });

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
  /** The last `start` honoured, with its `startKey`. */
  let startSeen: string | undefined;

  // The deck changed under us (a re-render), or the plugin names a slide to
  // open on (`start`; the same slide again when `startKey` changes): find
  // our place again.
  $effect(() => {
    const all = slides;
    const start = typeof node.start === "string" ? node.start : null;
    const asked = JSON.stringify([start, node.startKey ?? null]);
    untrack(() => {
      let next: Place | null = null;
      if (asked !== startSeen) {
        startSeen = asked;
        if (start !== null) next = placeOf(all, start, "last");
      }
      next ??= placeOf(all, standing, at.build, at.slide) ?? clampPlace(all, at);
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

  /** The build whose clip is on screen (playing, or held on its last
   *  frame), and the picture that was there before it, kept under the video
   *  until its first frame is drawn. */
  let playing = $state<{ slide: number; build: number; under: string | null; ended: boolean } | null>(null);

  /** Clicks closer together than this flick through the show: each lands on
   *  its build's finished picture, as in Keynote and PowerPoint, instead of
   *  starting an animation the next click cuts off. A click after a pause
   *  plays its animation again. */
  const FLICK_MS = 400;
  let lastClick = 0;

  function go(place: Place, tell = true, animate = false, auto = false): void {
    // An auto-advance (a clip that moves on when it ends) is not a click:
    // it neither flicks nor makes the click after it one.
    if (animate && !auto) {
      const t = performance.now();
      if (t - lastClick < FLICK_MS) animate = false;
      lastClick = t;
    }
    const next = clampPlace(slides, place);
    const under = build?.src ?? null;
    const moved = next.slide !== at.slide || next.build !== at.build;
    at = next;
    standing = slides[next.slide]?.title ?? "";
    // Only a move ends or starts a clip: another window naming the place we
    // already stand on must not cut ours off.
    if (moved) playing = animate && slides[next.slide]?.builds[next.build]?.clip != null ? { ...next, under, ended: false } : null;
    if (tell && mode !== "browse") say({ kind: "at", from: me, title: standing, slide: next.slide, build: next.build });
  }

  /** One click on or back: a show skips hidden slides, browsing doesn't.
   *  A click on, in the show, plays the build it lands on. */
  function step(delta: 1 | -1, auto = false): void {
    go(deckStep(buildCounts, hidden, at, delta, inShow), true, delta === 1 && mode === "present", auto);
  }

  /** Play the clip of the build we stand on (browsing's play button, and
   *  the show's first build). */
  function play(): void {
    if (build?.clip == null) return;
    playing = { ...at, under: null, ended: false };
  }

  const clip = $derived(playing !== null && playing.slide === at.slide && playing.build === at.build ? (build?.clip ?? null) : null);

  function clipEnded(): void {
    if (playing === null || clip === null) return;
    if (mode !== "present") {
      // Browsing goes back to the picture, with its zoom and area tool.
      playing = null;
      return;
    }
    playing.ended = true;
    if (clip.auto) step(1, true);
  }

  /** The clip's video could not load (its file went, its ticket lapsed):
   *  the build's picture shows, and the clip is asked for again. */
  function clipFailed(src: string): void {
    if (clip?.src === src) playing = null;
    broken(src);
  }

  function clipReady(e: Event): void {
    const v = e.currentTarget as HTMLVideoElement;
    const mine = clip?.src;
    if (clip !== null) v.playbackRate = clip.rate;
    void v.play().catch(() => {
      // Refused (a web view that will not autoplay): the picture stays. A
      // video the next click replaced rejects too, and is not ours to end.
      if (!disposed && clip?.src === mine) playing = null;
    });
  }

  function edge(end: "first" | "last"): void {
    const i = inShow ? showEdge(hidden, end) : end === "first" ? 0 : slides.length - 1;
    go({ slide: i, build: end === "first" ? 0 : Math.max(0, (slides[i]?.builds.length ?? 1) - 1) });
  }

  const syncKey = $derived(syncName(screen.ws, screen.plugin, str(node.id, screen.view)));

  // Two windows on one deck keep step while this one is in the show.
  $effect(() => {
    if (mode === "browse" || typeof BroadcastChannel === "undefined") return;
    const ch = new BroadcastChannel(syncKey);
    ch.onmessage = (e: MessageEvent) => {
      const msg = readSync(e.data);
      if (msg === null || msg.from === me) return;
      if (msg.kind === "where") {
        ch.postMessage({ kind: "at", from: me, title: standing, slide: at.slide, build: at.build } satisfies DeckSync);
        return;
      }
      const place = placeOf(slides, msg.title, msg.build, msg.slide);
      if (place === null) return;
      // The show follows the presenter's clicker with the same animation:
      // one click on plays, a jump shows the picture.
      const click = nextPlace !== null && nextPlace.slide === place.slide && nextPlace.build === place.build;
      go(place, false, mode === "present" && click);
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
  /** When each `src` was last asked for. The daemon's ticket lives about
   *  ten minutes from the last ask and answers with the same URL while the
   *  file is unchanged, so asking again only keeps it alive. */
  const asked = new Map<string, number>();
  const TICKET_KEEP_MS = 4 * 60 * 1000;
  let pictureError = $state<string | null>(null);

  function want(src: string | undefined, again = false): void {
    if (src === undefined || src === "") return;
    const last = asked.get(src);
    if (!again && last !== undefined && Date.now() - last < TICKET_KEEP_MS) return;
    asked.set(src, Date.now());
    screen
      .resolve(src)
      .then((abs) => {
        if (disposed) return "";
        paths[src] = abs;
        const hit = lastRawTicketUrl(abs);
        if (hit !== null) urls[src] = hit;
        return rawTicketUrl(abs);
      })
      .then(
        (url) => {
          if (!disposed && url !== "") urls[src] = url;
        },
        (e) => {
          if (disposed) return;
          asked.delete(src);
          if (src === build?.src) pictureError = e instanceof Error ? e.message : String(e);
          // A clip that can't be had: its build's picture, not a show left
          // waiting on the picture before it.
          if (src === clip?.src) playing = null;
        },
      );
  }

  /** A thumbnail's own URL for a picture (the daemon reads only the path).
   *  WebKit keeps one decoded copy per URL, and a picture drawn first at a
   *  thumbnail's size came back at that size in the slide viewer (192×108
   *  for a 1280×720 slide: the last build, the one the strip shows). Each
   *  size a picture is drawn at gets its own URL (`tag`); the price is one
   *  more fetch and decoded copy of a picture per size, all local. */
  function thumbUrl(url: string, tag = "thumb"): string {
    return `${url}${url.includes("?") ? "&" : "?"}${tag}`;
  }

  /** A ticket that lapsed under a long-open deck: mint it again, though
   *  not in a loop for a file that is really gone. */
  const retried = new Map<string, number>();
  function broken(src: string): void {
    if (Date.now() - (retried.get(src) ?? 0) < 30_000) return;
    retried.set(src, Date.now());
    delete urls[src];
    want(src, true);
  }

  // A talk can stand on one build for longer than a ticket lives: while in
  // the show, keep alive what is on screen and what the next click needs.
  $effect(() => {
    if (mode === "browse" || !$pageVisible) return;
    const keep = () => {
      for (const src of [build?.src, build?.clip?.src, nextBuild?.src, nextBuild?.clip?.src]) want(src, true);
    };
    untrack(keep);
    const id = setInterval(keep, TICKET_KEEP_MS);
    return () => clearInterval(id);
  });

  const nextBuild = $derived(nextPlace === null ? undefined : slides[nextPlace.slide]?.builds[nextPlace.build]);
  const AHEAD = 6;
  const ahead = $derived.by(() => {
    const out: string[] = [];
    let p: Place = at;
    for (let i = 0; i < AHEAD; i++) {
      const n = deckStep(buildCounts, hidden, p, 1, inShow);
      if (n.slide === p.slide && n.build === p.build) break;
      const src = slides[n.slide]?.builds[n.build]?.src;
      if (src !== undefined) out.push(src);
      p = n;
    }
    return out;
  });
  $effect(() => {
    pictureError = null;
    want(build?.src);
    want(build?.clip?.src);
    // The next click's picture is in the cache before it is asked for.
    want(nextBuild?.src);
    want(nextBuild?.clip?.src);
    // A run of clicks outruns one: the pictures a few clicks on are minted
    // too (a flick lands on pictures, not clips).
    for (const src of ahead) want(src);
  });

  // Entries for builds the deck no longer has go with them.
  $effect(() => {
    const live = new Set(slides.flatMap((s) => s.builds.flatMap((b) => (b.clip === null ? [b.src] : [b.src, b.clip.src]))));
    untrack(() => {
      for (const src of Object.keys(urls)) {
        if (!live.has(src)) {
          delete urls[src];
          delete paths[src];
          asked.delete(src);
          retried.delete(src);
        }
      }
    });
  });

  /** Pictures decoded ahead of their click. A show that swapped an `<img>`
   *  to a picture not yet decoded painted a blank frame between slides:
   *  every swap waits for its decode, and the next click's is done before
   *  it is asked for. The show needs three at once (shown, now, next): a 4K
   *  picture is ~33 MB decoded, so only a few are kept. */
  const decoded = new Map<string, { img: HTMLImageElement; ready: Promise<void> }>();
  const DECODED_KEEP = 4;
  function decode(url: string): Promise<void> {
    let hit = decoded.get(url);
    if (hit === undefined) {
      const img = new Image();
      img.src = url;
      hit = { img, ready: img.decode().catch(() => {}) };
      decoded.set(url, hit);
      while (decoded.size > DECODED_KEEP) decoded.delete(decoded.keys().next().value as string);
    } else {
      // Most recently wanted last: what is evicted is what was shown longest ago.
      decoded.delete(url);
      decoded.set(url, hit);
    }
    return hit.ready;
  }

  /** A build's picture, swapped in only once its URL is minted and the
   *  picture decoded: until then the picture before it stays on screen, so
   *  a fast run of clicks never shows a blank frame or a half-drawn one. */
  function steady(get: () => string | null, tag?: string): { readonly url: string | null; readonly src: string | null } {
    let shown = $state<{ src: string; url: string } | null>(null);
    $effect(() => {
      const src = get();
      if (src === null) {
        // Out of the show (or a clip with nothing under it): a later show
        // must not open on the picture this one ended on.
        shown = null;
        return;
      }
      const minted = urls[src];
      if (minted === undefined) return;
      const url = tag === undefined ? minted : thumbUrl(minted, tag);
      if (untrack(() => shown?.url) === url) return;
      let live = true;
      void decode(url).then(() => {
        if (live && !disposed) shown = { src, url };
      });
      return () => {
        live = false;
      };
    });
    return {
      get url() {
        return shown?.url ?? null;
      },
      get src() {
        return shown?.src ?? null;
      },
    };
  }

  /** The show's picture: under a clip that is still playing, the picture
   *  that was there before it. */
  const still = $derived(build === undefined ? null : clip !== null && playing !== null && !playing.ended ? playing.under : build.src);
  const showPicture = steady(() => (mode === "present" ? still : null));
  // The presenter's panels draw pictures smaller than the show and the
  // viewer: their own URLs, for the same reason as the strip's thumbUrl.
  const nowPicture = steady(() => (mode === "presenter" && build !== undefined ? build.src : null), "now");
  const nextPicture = steady(() => (mode === "presenter" && nextBuild !== undefined ? nextBuild.src : null), "next");
  // Browsing's viewer is handed the deck's own decoded picture: a run of
  // arrow keys swaps it in place, never a fresh ticket and load per slide.
  const browsePicture = steady(() => (mode === "browse" && build !== undefined ? build.src : null));
  // The next click's picture is decoded before the click.
  $effect(() => {
    if (nextBuild === undefined) return;
    const url = urls[nextBuild.src];
    if (url !== undefined) void decode(url);
  });

  let ImageView = $state<Component<{ path: string; src?: string; pixels?: boolean; onbroken?: () => void }> | null>(null);
  $effect(() => {
    if (ImageView === null && mode === "browse") {
      void import("../../previews/ImageView.svelte").then((m) => (ImageView = m.default));
    }
  });

  // --- the strip ---------------------------------------------------------------

  let strip = $state<HTMLDivElement | null>(null);
  /** Thumbnails load as they come near the strip's window. */
  function nearby(el: HTMLElement, src: string | undefined): { update(src: string | undefined): void; destroy(): void } {
    let io: IntersectionObserver | null = null;
    // A re-render gives an edited slide a new picture: watch again for it.
    const watch = (s: string | undefined): void => {
      io?.disconnect();
      io = null;
      if (s === undefined) return;
      if (typeof IntersectionObserver === "undefined") {
        want(s);
        return;
      }
      io = new IntersectionObserver(
        (entries) => {
          if (!entries.some((e) => e.isIntersecting)) return;
          want(s);
          io?.disconnect();
          io = null;
        },
        { root: strip, rootMargin: "0px 240px" },
      );
      io.observe(el);
    };
    watch(src);
    return { update: watch, destroy: () => io?.disconnect() };
  }

  // Keep the current thumbnail in the strip's window, moving only the strip
  // (scrollIntoView would scroll the pane too).
  $effect(() => {
    const i = at.slide;
    const el = strip;
    if (el === null || mode === "present") return;
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

  /** The show took the screen (as opposed to covering the window). */
  let wentFull = false;

  async function present(): Promise<void> {
    mode = "present";
    // A show opens a slide as a click would have: at its first build
    // (browsing stands on a slide's last, its whole picture).
    go({ slide: at.slide, build: 0 }, false);
    play();
    await tick();
    const el = showEl;
    if (el === null) return;
    try {
      // Without full screen (a web view that refuses it) the show still
      // covers the window: `.show:not(:fullscreen)`.
      await el.requestFullscreen?.();
      wentFull = document.fullscreenElement === el;
    } catch {
      // Refused: the fixed overlay is the show.
    }
    el.focus();
  }

  async function leave(): Promise<void> {
    wentFull = false;
    if (document.fullscreenElement !== null) await document.exitFullscreen().catch(() => {});
    playing = null;
    mode = "browse";
    await tick();
    root?.focus();
  }

  // Leaving full screen by the browser's own key ends the show.
  $effect(() => {
    const onChange = () => {
      // Only a show that had the screen ends with it: one covering the
      // window is not ended by something else leaving full screen.
      if (document.fullscreenElement === null && mode === "present" && wentFull) {
        wentFull = false;
        playing = null;
        mode = "browse";
      }
    };
    document.addEventListener("fullscreenchange", onChange);
    return () => document.removeEventListener("fullscreenchange", onChange);
  });

  let started = $state(Date.now());
  let now = $state(Date.now());
  function presenter(): void {
    playing = null;
    if (mode === "presenter") {
      mode = "browse";
      return;
    }
    started = Date.now();
    now = started;
    mode = "presenter";
    void tick().then(() => root?.focus());
  }
  // --- the show in its own window ---------------------------------------------

  /** The presenter's view here, the show in a new window (the projector's):
   *  the window opens this tab again and finds an invite (localStorage, read
   *  once, only while fresh) that starts its show; the channel lines it up.
   *  Only a document created after the invite takes it: the same deck open
   *  in another pane of an existing window never starts a show by itself. */
  const INVITE = "chimaera.deck.show-window";
  const INVITE_MS = 60_000;
  let windowError = $state<string | null>(null);

  let opening = $state(false);

  async function showInWindow(): Promise<void> {
    windowError = null;
    if (root === null || opening) return;
    try {
      localStorage.setItem(INVITE, JSON.stringify({ sync: syncKey, at: Date.now() }));
    } catch {
      // No storage: the window opens on the deck, and F starts its show.
    }
    opening = true;
    const opened = await copyTabToWindow(root).catch(() => false);
    opening = false;
    if (!opened) {
      // The layer flashed why (popup blocked, daemon unreachable); a view
      // with no layer to ask says so here.
      windowError = "The show's window didn't open.";
      try {
        localStorage.removeItem(INVITE);
      } catch {
        // Nothing was stored.
      }
      return;
    }
    if (!disposed && mode !== "presenter") presenter();
  }

  function invited(): boolean {
    try {
      const raw = localStorage.getItem(INVITE);
      if (raw === null) return false;
      const v = JSON.parse(raw) as { sync?: unknown; at?: unknown };
      if (v.sync !== syncKey || typeof v.at !== "number" || Date.now() - v.at > INVITE_MS) return false;
      // A window that was already open (another pane on this deck) is not
      // the one the presenter opened.
      if (performance.timeOrigin < v.at) return false;
      localStorage.removeItem(INVITE);
      return true;
    } catch {
      return false;
    }
  }

  // The window a presenter opened takes the show as soon as it has slides.
  let inviteChecked = false;
  $effect(() => {
    if (slides.length === 0 || inviteChecked) return;
    inviteChecked = true;
    untrack(() => {
      if (invited()) void present();
    });
  });

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

<!-- Every slide, hidden ones dimmed. Browsing opens a slide whole (its last
     build); the presenter jumps the show to its first, hidden or not, and a
     show window follows. -->
{#snippet slideStrip()}
  <div class="strip" bind:this={strip}>
    {#each slides as s, i (i)}
      {@const last = s.builds[s.builds.length - 1]}
      <button
        class="thumb"
        class:on={i === at.slide}
        class:dim={s.hidden}
        onclick={() => go({ slide: i, build: mode === "presenter" ? 0 : Math.max(0, s.builds.length - 1) })}
        title={s.hidden ? `${s.title} (hidden)` : s.title}
        aria-label="slide {i + 1}: {s.title}{s.hidden ? ' (hidden)' : ''}"
        aria-current={i === at.slide ? "true" : undefined}
        use:nearby={last?.src}
      >
        {#if last !== undefined && urls[last.src] !== undefined}
          <img src={thumbUrl(urls[last.src] ?? "")} alt="" draggable="false" onerror={() => broken(last.src)} />
        {/if}
        <span class="num">{i + 1}</span>
      </button>
    {/each}
  </div>
{/snippet}

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
      {#if build?.clip != null && mode === "browse"}
        <button class="bbtn icon" onclick={play} title="play this build's animation" aria-label="play this build's animation">
          <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="M5 3.2v9.6L12.5 8z" fill="currentColor" /></svg>
        </button>
      {/if}
      {#if slide?.file != null}
        <button class="bbtn" onclick={askAgent} disabled={screen.busy} title="point an agent at this slide's code">@</button>
      {/if}
      {#if windowError !== null}<span class="tag" role="status">{windowError}</span>{/if}
      <button class="bbtn" disabled={opening} onclick={() => void showInWindow()} title="the show in a new window (drag it to the projector); this one shows your notes, the next click and every slide">show in window</button>
      <button class="bbtn" class:on={mode === "presenter"} onclick={presenter} aria-pressed={mode === "presenter"} title="presenter view: this slide, the next, your notes, a timer (P)">presenter</button>
      <button class="bbtn" onclick={() => void present()} title="present full screen; hidden slides are skipped (F)">present</button>
    </div>

    {#if mode === "presenter"}
      <div class="console">
        <div class="now">
          {#if build !== undefined && nowPicture.url !== null}
            <img src={nowPicture.url} alt={pictureAlt} onerror={() => broken(build.src)} />
          {:else}
            <p class="none">{slide?.title} has no picture.</p>
          {/if}
        </div>
        <div class="side">
          <div class="next">
            <span class="cap">Next</span>
            {#if nextPlace === null}
              <p class="end">End of the show</p>
            {:else if nextBuild !== undefined && nextPicture.url !== null}
              <img src={nextPicture.url} alt="next: {slides[nextPlace.slide]?.title ?? ''}" onerror={() => broken(nextBuild.src)} />
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
      {@render slideStrip()}
    {:else}
      <div class="stage">
        {#if pictureError !== null}
          <p class="none bad">{pictureError}</p>
        {:else if build === undefined}
          <p class="none">{slide?.title} has no picture.</p>
        {:else if clip !== null && urls[clip.src] !== undefined}
          {#key clip.src}
            <!-- svelte-ignore a11y_media_has_caption — a slide's animation: silent, and its picture carries the alt text -->
            <video class="clip" src={urls[clip.src]} muted playsinline aria-label={pictureAlt} onloadeddata={clipReady} onended={clipEnded} onerror={() => clipFailed(clip.src)}></video>
          {/key}
        {:else if ImageView !== null && browsePicture.src !== null && paths[browsePicture.src] !== undefined}
          {@const src = browsePicture.src}
          <ImageView path={paths[src] ?? ""} src={browsePicture.url ?? undefined} pixels={false} onbroken={() => broken(src)} />
        {/if}
      </div>
      {#if notes !== ""}
        <div class="notes" aria-label="speaker notes">{notes}</div>
      {/if}
      {@render slideStrip()}
    {/if}

    {#if mode === "present"}
      <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions — the show: a click is the presenter's clicker, the keys are handled beside it -->
      <div class="show" role="document" aria-label="{label}: the show" tabindex="0" bind:this={showEl} onclick={() => step(1)} onkeydown={onKey}>
        {#if build !== undefined}
          {#if showPicture.url !== null}
            <img src={showPicture.url} alt={pictureAlt} draggable="false" onerror={() => still !== null && broken(still)} />
          {/if}
          {#if clip !== null && urls[clip.src] !== undefined}
            {#key clip.src}
              <!-- svelte-ignore a11y_media_has_caption — a slide's animation: silent, and its picture carries the alt text -->
              <video
                class:done={playing?.ended === true && showPicture.src === build.src}
                src={urls[clip.src]}
                muted
                playsinline
                loop={clip.loop}
                onloadeddata={clipReady}
                onplaying={(e) => e.currentTarget.classList.add("drawn")}
                onended={clipEnded}
                onerror={() => clipFailed(clip.src)}
              ></video>
            {/key}
          {/if}
        {/if}
        {#if nextBuild?.clip != null && urls[nextBuild.clip.src] !== undefined}
          <!-- The next click's animation is fetched before it is asked for. -->
          <video class="warm" src={urls[nextBuild.clip.src]} muted preload="auto" aria-hidden="true" tabindex="-1"></video>
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
  .show img,
  .show video {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: contain;
    user-select: none;
  }
  /* A video frame is a touch softer and its colours a touch off the
     picture's (compressed, another colour pipeline): it fades in over the
     picture under it and out once its build's own picture is drawn, so
     neither the start nor the end of an animation nor the next click pops. */
  .show video {
    position: absolute;
    inset: 0;
    opacity: 0;
    transition: opacity 0.12s ease;
  }
  /* Set on the element by its own first `playing`: a replayed clip is a new
     element that has no frame yet, and a video with none paints black. */
  .show video:global(.drawn) {
    opacity: 1;
  }
  .show video.done:global(.drawn) {
    opacity: 0;
  }
  @media (prefers-reduced-motion: reduce) {
    .show video {
      transition: none;
    }
  }
  .show video.warm {
    width: 1px;
    height: 1px;
    opacity: 0;
    pointer-events: none;
  }
  .stage .clip {
    width: 100%;
    object-fit: contain;
    background: #000;
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
    font-size: var(--text-xs);
    font-weight: 600;
    color: color-mix(in srgb, var(--fg) 72%, var(--muted));
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
