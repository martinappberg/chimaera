<script lang="ts">
  /**
   * One plugin view in the Chimaera format (`ui/1`), wherever it is drawn:
   * its own tab, a dashboard panel, a claimed file's view, a status chip or
   * a section of its Extensions card. It renders the view (the daemon has
   * already checked the tree), sends node actions to the plugin, carries
   * out the built-in ones itself, and renders again when the plugin says
   * the view changed (a `view` frame) — never on a timer.
   *
   * A screen the plugin failed to draw, or sent in a shape chimaera can't
   * draw, says so in one line (its problems are in the daemon's log and a
   * disclosure), never a blank box.
   */
  import { onDestroy, onMount } from "svelte";
  import UiNode from "./UiNode.svelte";
  import Spinner from "../../previews/Spinner.svelte";
  import { copyText } from "../../shared/clipboard";
  import { openPath } from "../../shared/openPath";
  import { referenceNow } from "../../shared/reference";
  import { openInSystemBrowser } from "../../shared/urlOpen";
  import {
    fetchQuery,
    fetchView,
    onPlatformFrame,
    openPluginView,
    postViewAction,
    resolvePlace,
    saveOutput,
    type RenderResult,
  } from "../platform";
  import { BUILTIN_ACTIONS, provideScreen, str, type ScreenCtx } from "./screen";

  interface Props {
    ws: string;
    wsRoot: string | null;
    plugin: string;
    view: string;
    /** A file view's file (workspace-relative). */
    file?: string;
    /** A chip or card section: no padding, no spinner block. */
    compact?: boolean;
  }

  let { ws, wsRoot, plugin, view, file = undefined, compact = false }: Props = $props();

  let result = $state<RenderResult | null>(null);
  let loadError = $state<string | null>(null);
  let busy = $state(false);
  let note = $state<{ text: string; tone: "neutral" | "bad" } | null>(null);
  let replace = $state<{ from: string; to: string } | null>(null);
  let width = $state<"narrow" | "wide">("wide");
  let host: HTMLDivElement | undefined = $state();

  let seq = 0;
  let again = false;
  let loading = false;

  async function load(): Promise<void> {
    if (loading) {
      again = true;
      return;
    }
    loading = true;
    const mine = ++seq;
    try {
      const r = await fetchView(ws, plugin, view, { file, width });
      if (mine === seq) {
        result = r;
        loadError = null;
      }
    } catch (e) {
      if (mine === seq) loadError = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
      if (again) {
        again = false;
        void load();
      }
    }
  }

  let noteTimer: ReturnType<typeof setTimeout> | undefined;
  function say(text: string, tone: "neutral" | "bad" = "neutral"): void {
    note = { text, tone };
    clearTimeout(noteTimer);
    if (tone === "neutral") noteTimer = setTimeout(() => (note = null), 6000);
  }

  const resolve = (ref: string): Promise<string> => resolvePlace(ws, wsRoot, plugin, ref);

  async function builtin(action: string, payload: Record<string, unknown>): Promise<void> {
    switch (action) {
      case "open-file": {
        const ref = str(payload.file);
        if (ref === "") return;
        const line = typeof payload.line === "number" ? payload.line : undefined;
        openPath(await resolve(ref), "file", line !== undefined ? { reveal: { line } } : {});
        return;
      }
      case "open-view":
        if (!openPluginView(plugin, str(payload.view))) say("This view can't open here.", "bad");
        return;
      case "open-url": {
        const url = str(payload.url);
        if (/^https?:\/\//i.test(url)) openInSystemBrowser(url);
        return;
      }
      case "copy":
        say((await copyText(str(payload.text))) ? "Copied." : "Couldn't copy.", "neutral");
        return;
      case "save-to-workspace":
        await save(str(payload.from), str(payload.to), false);
        return;
      case "ask-agent": {
        const ref = str(payload.file);
        const text = str(payload.text);
        const line = typeof payload.line === "number" ? payload.line : null;
        referenceNow(host, {
          kind: "file",
          path: ref !== "" ? await resolve(ref) : "",
          startLine: line,
          endLine: line,
          text,
          quote: text,
        });
        return;
      }
    }
  }

  async function save(from: string, to: string, overwrite: boolean): Promise<void> {
    try {
      const saved = await saveOutput(ws, plugin, from, to, overwrite);
      replace = null;
      say(`Saved to ${to}.`);
      openPath(saved.path, "file");
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      if (!overwrite && message.includes("already exists")) replace = { from, to };
      else say(message, "bad");
    }
  }

  async function act(action: string, payload: unknown, extra?: { form?: Record<string, unknown>; value?: unknown }): Promise<void> {
    if (busy) return;
    busy = true;
    try {
      if (BUILTIN_ACTIONS.has(action)) {
        await builtin(action, (payload ?? {}) as Record<string, unknown>);
        return;
      }
      const next = await postViewAction(ws, plugin, view, action, payload, extra ?? {});
      if (next !== null) result = next;
    } catch (e) {
      say(e instanceof Error ? e.message : String(e), "bad");
    } finally {
      busy = false;
    }
  }

  const ctx: ScreenCtx = {
    get ws() {
      return ws;
    },
    get wsRoot() {
      return wsRoot;
    },
    get plugin() {
      return plugin;
    },
    get view() {
      return view;
    },
    get busy() {
      return busy;
    },
    act,
    resolve,
    query: (name, args) => fetchQuery(ws, plugin, name, args),
  };
  provideScreen(ctx);

  // Render again when the view, file or width class changes.
  $effect(() => {
    void ws;
    void plugin;
    void view;
    void file;
    void width;
    void load();
  });

  let stop: (() => void) | null = null;
  let observer: ResizeObserver | null = null;
  onMount(() => {
    stop = onPlatformFrame((f) => {
      if (f.type === "view" && f.plugin === plugin && f.workspace === ws && f.view === view) void load();
    });
    if (host !== undefined && typeof ResizeObserver !== "undefined") {
      observer = new ResizeObserver((entries) => {
        const w = entries[0]?.contentRect.width ?? 0;
        const next = w > 0 && w < 560 ? "narrow" : "wide";
        if (next !== width) width = next;
      });
      observer.observe(host);
    }
  });
  onDestroy(() => {
    stop?.();
    observer?.disconnect();
    clearTimeout(noteTimer);
  });
</script>

<div class="screen" class:compact bind:this={host} aria-busy={busy}>
  {#if loadError !== null}
    <p class="failed">{loadError}</p>
  {:else if result === null}
    {#if !compact}<div class="loading"><Spinner /></div>{/if}
  {:else if !result.ok}
    <div class="failed">
      <span>{result.error}</span>
      {#if result.problems.length > 0}
        <details>
          <summary>What to fix</summary>
          <ul>
            {#each result.problems as p, i (i)}<li>{p}</li>{/each}
          </ul>
        </details>
      {/if}
      <button class="opt small" onclick={() => void load()}>Try again</button>
    </div>
  {:else}
    <UiNode node={result.tree.root} />
  {/if}
  {#if replace !== null}
    <div class="note" role="alert">
      <span>{replace.to} already exists.</span>
      <button class="opt small primary" onclick={() => replace && void save(replace.from, replace.to, true)}>Replace it</button>
      <button class="opt small quiet" onclick={() => (replace = null)}>Keep it</button>
    </div>
  {:else if note !== null}
    <div class="note" class:bad={note.tone === "bad"} role="status">{note.text}</div>
  {/if}
</div>

<style>
  .screen {
    container-type: inline-size;
    container-name: plugin-screen;
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-width: 0;
    color: var(--fg);
  }
  .screen:not(.compact) {
    padding: 14px 16px;
  }
  .loading {
    display: flex;
    justify-content: center;
    padding: 24px;
  }
  .failed {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 12px;
    color: var(--muted);
    font-size: var(--text-sm);
    margin: 0;
  }
  .failed details {
    flex-basis: 100%;
    font-size: var(--text-xs);
  }
  .failed ul {
    margin: 4px 0 0;
    padding-left: 18px;
    font-family: var(--mono);
  }
  .note {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .note.bad {
    color: var(--err);
  }
</style>
