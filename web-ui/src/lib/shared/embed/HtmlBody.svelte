<script lang="ts" module>
  let seq = 0;
</script>

<script lang="ts">
  /**
   * An HTML report in a card: the daemon-served page, addressed through its
   * ticket's folder so its relative scripts, styles and figures load. A
   * fixed height with an expand toggle; nothing loads until the card is
   * near.
   *
   * In the macOS app the page runs in a web view of its own laid over the
   * card (`shared/paneView.ts`, embedded): an iframe here shares the
   * window's web process, and one 50 MB report took that process past 8 GB
   * and froze the window, chat included (2026-11-04). The view is cropped to
   * the card's scroller, and the wheel over it scrolls the document until
   * the card is clicked into (`liveViews`: one active card, the rest
   * passive). Once the page has shown, its snapshot is kept as the card's
   * still: it stands in while the document scrolls (the view would trail
   * the scroll by a frame), under a menu, and for a card that gave its view
   * up to the `LIVE_MAX` budget. Elsewhere — a browser tab, Linux, Windows,
   * an app build without pane views — it is the sandboxed frame
   * (`allow-scripts`, never same-origin; the response carries the same
   * CSP).
   */
  import { untrack } from "svelte";
  import SlowLoad from "../SlowLoad.svelte";
  import { PaneViewControl, paneView, paneViewsPossible } from "../paneView";
  import { liveViews, type Still } from "./liveViews.svelte";
  import { paneFrame } from "../paneFrame";
  import { withPageApi } from "../paneViewBridge";
  import { backgroundBehind, pageTheme, pageThemeOn } from "../pageTheme";
  import { pageMessage } from "../pageMessage";

  interface Props {
    /** The page's own `/raw` URL, as the card resolved it. */
    url: string | null;
    title: string;
    compact: boolean;
    /** Near its scroller's viewport: may load. */
    active: boolean;
    /** On screen now (the app's view shows only while it is). */
    visible?: boolean;
    /** The chat whose answer shows this card: a page may send to it
     *  (`shared/pageMessage.ts`); null anywhere else. */
    chat?: string | null;
    /** When the page's file was last written (epoch ms): only a page written
     *  during its chat may send. */
    writtenAtMs?: number | null;
    /** Part of the answer (`#inline`): the page's own height, the app's
     *  background, no expand; an open button on hover. */
    inline?: boolean;
    /** Open the page in a pane (inline mode's only control). */
    onOpen?: () => void;
  }

  let {
    url: pageUrl,
    title,
    compact,
    active,
    visible = true,
    chat = null,
    writtenAtMs = null,
    inline = false,
    onOpen,
  }: Props = $props();

  /** An inline page's height as it reports it (the page API's `page_size`),
   *  kept between these bounds: past the top it scrolls inside itself. */
  const INLINE_MIN = 48;
  const INLINE_MAX = 720;
  const INLINE_START = 160;
  /** An inline page's open button shows while the pointer is over the page
   *  or the button, and lingers this long after (the pointer travels from
   *  one to the other). The page reports its hover (the bridge's `hover`):
   *  the window can't see the pointer over a view or a site-isolated frame. */
  const OPEN_LINGER_MS = 600;
  let pageHover = $state(false);
  let openHover = $state(false);
  let lingering = $state(false);
  let lingerTimer: ReturnType<typeof setTimeout> | null = null;
  const onHover = (inside: boolean): void => {
    pageHover = inside;
    if (lingerTimer !== null) clearTimeout(lingerTimer);
    lingering = true;
    lingerTimer = setTimeout(() => {
      lingering = false;
      lingerTimer = null;
    }, OPEN_LINGER_MS);
  };
  $effect(() => () => {
    if (lingerTimer !== null) clearTimeout(lingerTimer);
  });
  const openShown = $derived(pageHover || openHover || lingering);
  let reported = $state<number | null>(null);
  const onPageSize = (h: number): void => {
    reported = Math.min(INLINE_MAX, Math.max(INLINE_MIN, Math.ceil(h)));
  };

  // The page as Chimaera shows it (`withPageApi`): in the app, its URL
  // carries the theme (a new theme loads it again); framed, the theme it
  // loaded with, and later ones as messages, so the frame never reloads.
  /** The card's own box, to read what is painted behind it. */
  let box = $state<HTMLElement | null>(null);
  /** The theme this page gets: the window's, and for an inline page the
   *  colour behind its card as `bg` (a chat paints its own, not `--bg`),
   *  so the page sits seamlessly on it. Read again when the theme changes. */
  const theme = $derived.by(() => {
    const base = $pageTheme;
    if (!inline || box === null) return base;
    return pageThemeOn(backgroundBehind(box.parentElement ?? box));
  });
  // An app view carries a new theme in its URL (a reload) only once the
  // page said it wears it (`page_themed`): an old report keeps its state
  // across a theme switch. An inline card waits for its box, so its page's
  // first theme already has the colour behind it.
  let pageThemed = $state(false);
  $effect(() => {
    void pageUrl;
    pageThemed = false;
  });
  const waiting = $derived(pageUrl === null || (inline && box === null));
  const url = $derived(
    waiting || pageUrl === null
      ? null
      : withPageApi(pageUrl, {
          framed: false,
          theme: pageThemed ? theme : untrack(() => theme),
          inline: inline ? INLINE_MAX : undefined,
        }),
  );
  const frameUrl = $derived(
    waiting || pageUrl === null
      ? null
      : withPageApi(pageUrl, { framed: true, theme: untrack(() => theme), inline: inline ? INLINE_MAX : undefined }),
  );
  const onPageMessage = (m: { send: boolean; text: string; title: string }): void => {
    pageMessage({ chat, writtenAtMs, ...m });
  };

  let expanded = $state(false);
  const height = $derived(compact ? 200 : inline ? (reported ?? INLINE_START) : expanded ? 720 : 360);

  /** A still is taken this long after the page shows (its first paint
   *  settles: fonts, a chart drawing in). */
  const STILL_AFTER_MS = 800;
  /** Retaken on its own at most this often (a heavy page may still have
   *  been drawing at the first one; a scroll alone changes nothing). Leaving
   *  a card the user worked in always retakes it. */
  const STILL_REFRESH_MS = 10_000;

  const id = `card${(seq += 1)}`;
  let native = $state(paneViewsPossible());
  const control = new PaneViewControl();
  let loading = $state(true);
  let crashed = $state(false);
  /** The view is on screen now (its snapshot is the page, not a blank). */
  let shownNow = $state(false);
  let host = $state<HTMLElement | null>(null);
  let stageWidth = $state(0);

  /** This card's own last still: it wins over a shared one (another card
   *  of the same report may show it scrolled elsewhere). */
  let ownStill = $state<{ url: string; still: Still } | null>(null);
  let stillAt = 0;
  const still = $derived.by((): Still | null => {
    if (!native) return null;
    const own = ownStill;
    if (own !== null && own.url === url && Math.round(own.still.width) === Math.round(stageWidth)) return own.still;
    return liveViews.still(url, stageWidth);
  });
  /** Holds a view: one of the cards most recently on screen. Off screen
   *  it stays, hidden (`shown`), so scrolling back doesn't load it again. */
  const live = $derived(native && active && url !== null && liveViews.live(id));
  const working = $derived(liveViews.active === id);

  // `seen` reads the list it writes: untracked, or every card on screen
  // re-runs every other one's effect without end.
  $effect(() => {
    if (native && active && visible) untrack(() => liveViews.seen(id));
  });
  $effect(() => () => liveViews.drop(id));
  $effect(() => {
    if (!live) shownNow = false;
  });

  /** Keep what the view shows now as the card's still. */
  async function takeStill(): Promise<void> {
    const at = url;
    if (at === null || !live || !shownNow || loading || crashed) return;
    stillAt = Date.now();
    const shot = await control.snapshot();
    if (shot !== null && at === url) {
      const taken: Still = { src: `data:image/png;base64,${shot.png}`, width: shot.width, height: shot.height };
      ownStill = { url: at, still: taken };
      liveViews.keepStill(at, taken);
    }
  }

  /** A card over the budget takes a view back when the pointer comes to
   *  it: one evicted while its tab was parked never sees its observer
   *  fire again on the tab's return. */
  function claim(): void {
    if (native && active && !live) liveViews.seen(id);
  }

  $effect(() => {
    const el = host;
    if (el === null) return;
    el.addEventListener("pointerenter", claim);
    return () => el.removeEventListener("pointerenter", claim);
  });

  // A still each time the view settles on screen: once the page is up, and
  // again after every scroll or overlay that hid it (a heavy page may still
  // have been drawing at the first one).
  $effect(() => {
    if (!live || !shownNow || loading || crashed) return;
    if (untrack(() => still) !== null && Date.now() - stillAt < STILL_REFRESH_MS) return;
    const timer = setTimeout(() => void takeStill(), STILL_AFTER_MS);
    return () => clearTimeout(timer);
  });

  // Working in the card ends when the pointer is back on this page
  // anywhere outside it (over the view, this page sees no pointer at all);
  // the still is renewed then, since the user may have changed the page.
  $effect(() => {
    if (!working) return;
    const onMove = (e: PointerEvent): void => {
      const r = host?.getBoundingClientRect();
      if (r === undefined) return;
      if (e.clientX < r.left || e.clientX >= r.right || e.clientY < r.top || e.clientY >= r.bottom) {
        void takeStill();
        liveViews.activate(null);
      }
    };
    document.addEventListener("pointermove", onMove, true);
    return () => document.removeEventListener("pointermove", onMove, true);
  });
</script>

{#if native}
  <div class="html-body native" class:tile={compact} class:inline style:height="{height}px" bind:this={box}>
    <div class="stage" bind:this={host} bind:clientWidth={stageWidth}>
      {#if still !== null}
        <img
          class="still"
          src={still.src}
          alt=""
          draggable="false"
          style:width="{still.width}px"
          style:height="{still.height}px"
        />
      {/if}
      {#if live}
        <!-- The placeholder the view lies over; what it holds shows only
             while the view is hidden (loading, crashed). -->
        <div
          class="slot"
          use:paneView={{
            url,
            shown: visible,
            embedded: true,
            passive: !working,
            stillWhileScrolling: still !== null,
            onFallback: () => (native = false),
            onLoading: (l) => {
              loading = l;
              if (l) crashed = false;
            },
            onCrashed: () => (crashed = true),
            onPage: () => (crashed = false),
            onFocus: () => liveViews.activate(id),
            onShown: (on) => (shownNow = on),
            onPageMessage,
            onPageSize,
            onHover,
            onPageThemed: () => (pageThemed = true),
            control,
          }}
        >
          {#if crashed}
            <div class="note">
              <span>This page stopped responding</span>
              <button class="pill" onclick={() => control.nav("reload")}>Reload</button>
            </div>
          {:else if loading && still === null}
            <div class="note"><SlowLoad after={300}>loading page…</SlowLoad></div>
          {/if}
        </div>
      {:else if still === null && active}
        <!-- Over the window's budget of live pages and never drawn yet. -->
        <div class="note">
          <button class="pill" onclick={claim}>Show the page</button>
        </div>
      {/if}
    </div>
    {#if inline}
      <!-- Under the page, in the space the card keeps for it: nothing draws
           over a view, and showing it never moves anything. -->
      <button
        class="pill open-under"
        class:shown={openShown}
        onclick={() => onOpen?.()}
        onpointerenter={() => (openHover = true)}
        onpointerleave={() => (openHover = false)}
        title="open in a pane">open ↗</button
      >
    {:else if !compact}
      <!-- Outside the stage: the view draws above anything over it. -->
      <div class="foot">
        <button class="pill" onclick={() => (expanded = !expanded)} title={expanded ? "collapse" : "show more of the page"}>
          {expanded ? "collapse" : "expand"}
        </button>
      </div>
    {/if}
  </div>
{:else}
  <div class="html-body" class:tile={compact} class:inline style:height="{height}px" bind:this={box}>
    {#if active && frameUrl !== null}
      <iframe
        src={frameUrl}
        {title}
        sandbox="allow-scripts"
        referrerpolicy="no-referrer"
        use:paneFrame={{ onPageMessage, onPageSize, onHover, theme }}
      ></iframe>
    {/if}
    {#if inline}
      <button
        class="pill open-under"
        class:shown={openShown}
        onclick={() => onOpen?.()}
        onpointerenter={() => (openHover = true)}
        onpointerleave={() => (openHover = false)}
        title="open in a pane">open ↗</button
      >
    {:else if !compact}
      <button class="expand" onclick={() => (expanded = !expanded)} title={expanded ? "collapse" : "show more of the page"}>
        {expanded ? "collapse" : "expand"}
      </button>
    {/if}
  </div>
{/if}

<style>
  .html-body {
    position: relative;
    /* Pages assume a white canvas regardless of theme. */
    background: #ffffff;
    transition: height 0.18s ease;
  }
  /* Part of the answer: the page wears the app's theme (`--ch-*`), so it
     sits on the app's own background; its open button shows on hover. */
  .html-body.inline {
    background: transparent;
    transition: none;
  }
  .open-under {
    position: absolute;
    top: calc(100% + 3px);
    right: 0;
    padding: 0 7px;
    line-height: 17px;
    opacity: 0;
    pointer-events: none;
    transition: opacity 0.12s ease;
  }
  .open-under.shown,
  .open-under:focus-visible {
    opacity: 1;
    pointer-events: auto;
  }
  /* In the app the page is a view of its own: the box under it shows only
     while the view catches up with a new size, so it takes the card's tone
     (a white one flashed in dark mode), and it never animates its height
     (the view follows in steps, leaving a gap). */
  .html-body.native:not(.inline) {
    background: color-mix(in srgb, var(--fg) 2%, var(--bg));
    transition: none;
  }
  .html-body.tile {
    flex: 1;
    height: auto !important;
    min-height: 200px;
  }
  iframe {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    border: none;
  }
  .expand {
    position: absolute;
    right: 8px;
    bottom: 8px;
    padding: 2px 8px;
    border: 1px solid var(--edge);
    border-radius: 999px;
    background: color-mix(in srgb, var(--bg) 92%, transparent);
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
    opacity: 0.85;
  }
  .expand:hover {
    color: var(--accent);
    border-color: var(--accent);
    opacity: 1;
  }
  .native {
    display: flex;
    flex-direction: column;
  }
  .stage {
    position: relative;
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }
  .still {
    position: absolute;
    top: 0;
    left: 0;
    pointer-events: none;
    user-select: none;
  }
  .slot {
    position: absolute;
    inset: 0;
  }
  .note {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
    align-items: center;
    justify-content: center;
    padding: 8px;
    color: #6b6b6b;
    font-size: var(--text-xs);
  }
  .foot {
    display: flex;
    justify-content: flex-end;
    padding: 3px 8px;
    border-top: 1px solid var(--edge);
    background: var(--bg);
  }
  .pill {
    padding: 2px 8px;
    border: 1px solid var(--edge);
    border-radius: 999px;
    background: var(--bg);
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .pill:hover {
    color: var(--accent);
    border-color: var(--accent);
  }
  @media (prefers-reduced-motion: reduce) {
    .html-body {
      transition: none;
    }
  }
</style>
