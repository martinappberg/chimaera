<script lang="ts" module>
  let seq = 0;
</script>

<script lang="ts">
  /**
   * The `web` node (docs/design/rich-views-plan.md, Part 2): a page of the
   * plugin's own bundle, for a drawing area a JavaScript library draws (a
   * genome track, a molecule). Only the drawing area is web, and it fills
   * its slot: no head of its own (its tab or screen already names it; the
   * label is its accessible name). Everything around it stays core's.
   *
   * The page runs where agents' pages run: a native view of its own in the
   * macOS app (`shared/paneView.ts`), else a sandboxed frame inside the
   * navigation guard (`shared/guardFrame.ts`). Its URL is a grant ticket
   * (`mintWebPage`), asked again while it is shown to keep it alive. The
   * page hears its node's `data` (on load, and again when it or `key`
   * changes), whether it is shown, and the theme — all without a reload
   * (`PaneViewControl.command`). What it asks back: `act` → the screen's
   * action (the plugin's `on_action`, or a built-in), `status` → a small
   * line under the page only while it says something (`warn` or `bad`, or
   * `busy` past `BUSY_AFTER_MS`; `ok` and `idle` show nothing),
   * `reference` → a draft with a picture for the agent the person works
   * with.
   *
   * Limits (`webViews.svelte.ts`): at most four run per window, the least
   * recently shown closed first; one hidden for two minutes closes. A
   * closed one shows its last still (native) or a "paused" note, and loads
   * again when shown or clicked.
   */
  import { untrack } from "svelte";
  import SlowLoad from "../../shared/SlowLoad.svelte";
  import { PaneViewControl, paneView, paneViewsPossible } from "../../shared/paneView";
  import { paneFrame } from "../../shared/paneFrame";
  import { guardedPage } from "../../shared/guardFrame";
  import { withPageApi, type WebEvent, type WebState } from "../../shared/paneViewBridge";
  import { pageTheme } from "../../shared/pageTheme";
  import { pageMessage } from "../../shared/pageMessage";
  import { cropBlob, referenceText } from "../../shared/pageCrop";
  import { paneVisibility } from "../../previews/viewLifecycle.svelte";
  import { mintWebPage } from "../platform";
  import { useScreen } from "./screen";
  import { PARKED_MS, webViews } from "./webViews.svelte";
  import { PendingCalls, registerWebCallTarget } from "./webCalls";

  let { node }: { node: Record<string, unknown> } = $props();

  const screen = useScreen();
  const entry = $derived(typeof node.entry === "string" ? node.entry : "");
  const label = $derived(typeof node.label === "string" ? node.label : "");
  /** What the page hears as `chimaera.data`: re-sent when the data or the
   *  node's `key` changes. */
  const delivery = $derived(JSON.stringify({ data: node.data ?? null, key: node.key ?? null }));

  /** A grant is asked again this often while the view is shown (it lives
   *  ten minutes from its last mint). */
  const RENEW_MS = 5 * 60 * 1000;

  const id = `web${(seq += 1)}`;
  let box = $state<HTMLElement | null>(null);
  const vis = paneVisibility(
    () => undefined,
    () => box,
  );
  let native = $state(paneViewsPossible());
  const control = new PaneViewControl();
  let pageUrl = $state<string | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(true);
  let crashed = $state(false);
  let loaded = $state(false);
  let status = $state<{ state: WebState; text: string } | null>(null);
  /** A `busy` shows only once it has lasted this long: a quick load says
   *  nothing. */
  const BUSY_AFTER_MS = 600;
  let busyShown = $state(false);
  $effect(() => {
    busyShown = false;
    if (status?.state !== "busy") return;
    const timer = setTimeout(() => (busyShown = true), BUSY_AFTER_MS);
    return () => clearTimeout(timer);
  });
  /** What the line under the page says, when it says anything. */
  const said = $derived(
    status === null || status.state === "ok" || status.state === "idle" || (status.state === "busy" && !busyShown)
      ? null
      : status,
  );
  let still = $state<{ src: string; width: number; height: number } | null>(null);
  /** Hidden longer than `PARKED_MS`: its page is closed. */
  let parked = $state(false);

  // The grant: minted when first shown, renewed while shown.
  $effect(() => {
    const want = entry;
    const shown = vis.shown;
    if (want === "" || !shown) return;
    let stop = false;
    const mint = (): void => {
      mintWebPage(screen.ws, screen.plugin, want, untrack(() => $pageTheme)).then(
        (url) => {
          if (stop) return;
          error = null;
          if (url !== untrack(() => pageUrl)) pageUrl = url;
        },
        (err: unknown) => {
          if (!stop) error = err instanceof Error ? err.message : String(err);
        },
      );
    };
    mint();
    const timer = setInterval(mint, RENEW_MS);
    return () => {
      stop = true;
      clearInterval(timer);
    };
  });

  // On screen: one of the window's most recent; hidden for long: closed.
  $effect(() => {
    if (vis.shown) {
      parked = false;
      webViews.seen(id);
      return;
    }
    const timer = setTimeout(() => {
      parked = true;
      webViews.drop(id);
    }, PARKED_MS);
    return () => clearTimeout(timer);
  });
  $effect(() => () => webViews.drop(id));

  const live = $derived(pageUrl !== null && !parked && webViews.live(id));
  $effect(() => {
    if (!live) loaded = false;
  });

  // Calls from the plugin to this page (`web-call`): while the page runs
  // and is shown, this window answers for its node; else the daemon asks a
  // headless page. A node without an `id` can't be named, so takes none.
  const calls = new PendingCalls();
  const nodeId = $derived(typeof node.id === "string" && node.id !== "" ? node.id : null);
  $effect(() => {
    const key = nodeId;
    if (key === null || !live || !loaded || !vis.shown) return;
    const unregister = registerWebCallTarget({
      plugin: screen.plugin,
      view: screen.view,
      id: key,
      file: screen.file,
      call: (callId, method, args) =>
        calls.start(callId, () => {
          if (!untrack(() => loaded)) return false;
          control.command({ op: "call", id: callId, method, args });
          return true;
        }),
    });
    return () => {
      unregister();
      if (!untrack(() => loaded)) calls.failAll("the page closed");
    };
  });
  $effect(() => () => calls.failAll("the view closed"));

  const nativeUrl = $derived(
    pageUrl === null ? null : withPageApi(pageUrl, { framed: false, theme: untrack(() => $pageTheme) }),
  );
  const frameUrl = $derived(
    pageUrl === null ? null : withPageApi(pageUrl, { framed: true, theme: untrack(() => $pageTheme) }),
  );

  /** Everything the page should hear now: after it loads, and on change. */
  const tell = (): void => {
    control.command({ op: "data", ...JSON.parse(delivery) });
    control.command({ op: "visibility", visible: vis.shown });
  };
  $effect(() => {
    void delivery;
    if (untrack(() => loaded)) tell();
  });
  $effect(() => {
    const visible = vis.shown;
    if (untrack(() => loaded)) control.command({ op: "visibility", visible });
  });
  // A native view hears a new theme as a command (a frame through
  // `paneFrame`'s own `theme`).
  $effect(() => {
    const theme = $pageTheme;
    if (native && untrack(() => loaded) && theme !== "") control.command({ op: "theme", theme });
  });

  const onPage = (): void => {
    crashed = false;
    loaded = true;
    tell();
    if (native) {
      const theme = $pageTheme;
      if (theme !== "") control.command({ op: "theme", theme });
    }
  };

  const onWeb = (ev: WebEvent): void => {
    switch (ev.kind) {
      case "web_act":
        void screen.act(ev.action, ev.payload);
        break;
      case "web_status":
        status = ev.state === "idle" && ev.text === "" ? null : { state: ev.state, text: ev.text };
        break;
      case "web_answer":
        calls.answer(ev.call, ev.ok, ev.value, ev.error);
        break;
      case "web_reference": {
        const text = referenceText(ev.text, ev.file, ev.line);
        void cropBlob(ev.crop, native ? control : null).then((image) =>
          pageMessage({ chat: null, writtenAtMs: null, send: false, text, title: label, image }),
        );
        break;
      }
    }
  };

  /** Keep a still while the page shows, for when it closes. */
  async function takeStill(): Promise<void> {
    if (!native || !loaded) return;
    const shot = await control.snapshot();
    if (shot !== null && shot.width > 0) {
      still = { src: `data:image/png;base64,${shot.png}`, width: shot.width, height: shot.height };
    }
  }
  $effect(() => {
    if (!live || !vis.shown) return;
    const timer = setTimeout(() => void takeStill(), 1500);
    return () => clearTimeout(timer);
  });

  function resume(): void {
    parked = false;
    webViews.seen(id);
  }
</script>

<div class="web" bind:this={box} role="group" aria-label={label}>
  <div class="stage">
    {#if error !== null}
      <div class="note bad">{error}</div>
    {:else if live && native}
      <div
        class="slot"
        use:paneView={{
          url: nativeUrl,
          shown: vis.shown,
          embedded: true,
          passive: false,
          onFallback: () => (native = false),
          onLoading: (l) => {
            loading = l;
            if (l) crashed = false;
          },
          onCrashed: () => (crashed = true),
          onPage,
          onFocus: () => webViews.seen(id),
          onWeb,
          control,
        }}
      >
        {#if crashed}
          <div class="note">
            <span>This view stopped responding</span>
            <button class="pill" onclick={() => control.nav("reload")}>Reload</button>
          </div>
        {:else if loading}
          <div class="note"><SlowLoad after={300}>loading…</SlowLoad></div>
        {/if}
      </div>
    {:else if live && frameUrl !== null}
      <iframe
        srcdoc={guardedPage(frameUrl, label)}
        title={label}
        use:paneFrame={{ onFocus: () => webViews.seen(id), onPage, onWeb, theme: $pageTheme, control }}
      ></iframe>
    {:else if still !== null}
      <button class="resume" onclick={resume} title="show the live view">
        <img src={still.src} alt={label} draggable="false" style:width="{still.width}px" />
      </button>
    {:else if pageUrl !== null}
      <div class="note">
        <button class="pill" onclick={resume}>Show the view</button>
      </div>
    {:else}
      <div class="note"><SlowLoad after={300}>loading…</SlowLoad></div>
    {/if}
  </div>
  {#if said !== null}
    <!-- Under the page, never over it (nothing draws over a native view),
         and only while there is something to say. -->
    <div class="said s-{said.state}" role="status">
      {#if said.state === "busy"}<span class="spin" aria-hidden="true"></span>{/if}
      <span class="said-text">{said.text !== "" ? said.text : said.state === "busy" ? "working…" : said.state}</span>
    </div>
  {/if}
</div>

<style>
  .web {
    display: flex;
    flex-direction: column;
    width: 100%;
    height: 100%;
    min-height: 0;
  }
  .said {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: none;
    min-width: 0;
    padding: 3px 10px;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .said-text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .s-warn {
    color: var(--warn);
  }
  .s-bad {
    color: var(--err);
  }
  .spin {
    width: 8px;
    height: 8px;
    border: 1.5px solid currentColor;
    border-right-color: transparent;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  :global(html.app-hidden) .spin {
    animation-play-state: paused;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .stage {
    position: relative;
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }
  .slot,
  iframe {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    border: none;
  }
  .note {
    position: absolute;
    inset: 0;
    display: flex;
    gap: 8px;
    align-items: center;
    justify-content: center;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .note.bad {
    color: var(--err);
    padding: 0 16px;
    text-align: center;
  }
  .resume {
    position: absolute;
    inset: 0;
    padding: 0;
    border: none;
    background: none;
    cursor: pointer;
    overflow: hidden;
  }
  .resume img {
    display: block;
    opacity: 0.85;
  }
  .pill {
    padding: 2px 8px;
    border: 1px solid var(--edge);
    border-radius: 999px;
    background: var(--bg);
    color: var(--muted);
    font: inherit;
    cursor: pointer;
  }
  .pill:hover {
    color: var(--accent);
    border-color: var(--accent);
  }
  @media (prefers-reduced-motion: reduce) {
    .spin {
      animation: none;
    }
  }
</style>
