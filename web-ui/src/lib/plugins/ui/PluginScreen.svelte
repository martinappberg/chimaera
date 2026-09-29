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
  import { get } from "svelte/store";
  import { referenceNow, referenceTarget } from "../../shared/reference";
  import { openInSystemBrowser } from "../../shared/urlOpen";
  import {
    fetchQuery,
    fetchTools,
    fetchView,
    installTool,
    progressWords,
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
    /** A file view that owns its pane: the tree's last split grows to the
     *  pane's height, and a viewer alone in a split pane fills it. */
    fill?: boolean;
  }

  let { ws, wsRoot, plugin, view, file = undefined, compact = false, fill = false }: Props = $props();

  let result = $state<RenderResult | null>(null);
  let loadError = $state<string | null>(null);
  let busy = $state(false);
  let note = $state<{ text: string; tone: "neutral" | "bad" } | null>(null);
  let replace = $state<{ from: string; to: string } | null>(null);
  let width = $state<"narrow" | "wide">("wide");
  let host: HTMLDivElement | undefined = $state();
  /** A long built-in action in flight (a tool install): its progress. */
  let task = $state<{ title: string; text: string; fraction: number | null } | null>(null);
  let taskPoll: ReturnType<typeof setInterval> | null = null;

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
      case "install-tool": {
        // The user's click, as on the card's Tools section: the daemon
        // downloads, checks, unpacks and sets it up (a minute or more for a
        // TeX Live). Its progress shows at the top meanwhile; the view draws
        // again at once (it may say it is installing) and when it is done.
        const tool = str(payload.tool);
        if (tool === "") return;
        const known = (await fetchTools(plugin).catch(() => [])).find((t) => t.tool === tool);
        const name = known?.name ?? tool;
        const title = `Installing ${name}`;
        task = { title, text: "Starting", fraction: null };
        const running = installTool(plugin, tool);
        void load();
        taskPoll = setInterval(() => {
          // Only while the page can be seen; the install's answer ends it.
          if (document.visibilityState !== "visible") return;
          void fetchTools(plugin)
            .then((all) => {
              const t = all.find((x) => x.tool === tool);
              if (t?.installing && task !== null) task = { title, ...progressWords(t.progress) };
            })
            .catch(() => {});
        }, 1000);
        try {
          await running;
          say(`${name} is installed.`);
        } finally {
          if (taskPoll !== null) clearInterval(taskPoll);
          taskPoll = null;
          task = null;
        }
        await load();
        return;
      }
      case "ask-agent": {
        // With no agent session here the reference would land nowhere.
        if (get(referenceTarget) === null) {
          say("Start an agent in this workspace, then ask again.");
          return;
        }
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
    if (taskPoll !== null) clearInterval(taskPoll);
  });
</script>

<div class="screen" class:compact class:fill bind:this={host} aria-busy={busy}>
  {#if task !== null}
    <div class="task" role="status" aria-live="polite">
      <div class="task-head">
        <span class="task-spin" aria-hidden="true"></span>
        <span class="task-title">{task.title}</span>
        <span class="task-text">{task.text}</span>
      </div>
      <div class="task-bar" class:indeterminate={task.fraction === null}>
        <span style:width={task.fraction === null ? undefined : `${Math.round(task.fraction * 100)}%`}></span>
      </div>
    </div>
  {/if}
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
  .task {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 9px 12px;
    border: 1px solid color-mix(in srgb, var(--accent) 40%, var(--edge));
    border-radius: 8px;
    background: color-mix(in srgb, var(--accent) 6%, var(--bg));
    flex: none;
  }
  .task-head {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: var(--text-sm);
    min-width: 0;
  }
  .task-title {
    font-weight: 600;
    white-space: nowrap;
  }
  .task-text {
    color: var(--muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .task-spin {
    width: 11px;
    height: 11px;
    flex: none;
    border-radius: 50%;
    border: 1.6px solid var(--accent);
    border-right-color: transparent;
    animation: task-spin 0.8s linear infinite;
  }
  @keyframes task-spin {
    to {
      transform: rotate(360deg);
    }
  }
  .task-bar {
    height: 4px;
    border-radius: 2px;
    background: color-mix(in srgb, var(--accent) 15%, transparent);
    overflow: hidden;
  }
  .task-bar span {
    display: block;
    height: 100%;
    background: var(--accent);
    border-radius: 2px;
    transition: width 0.4s ease;
  }
  .task-bar.indeterminate span {
    width: 30%;
    animation: task-slide 1.4s ease-in-out infinite;
  }
  @keyframes task-slide {
    from {
      transform: translateX(-100%);
    }
    to {
      transform: translateX(340%);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .task-spin,
    .task-bar.indeterminate span {
      animation: none;
    }
  }
  .screen.fill {
    box-sizing: border-box;
    min-height: 100%;
  }
  .screen.fill > :global(.stack) {
    flex: 1;
    min-height: 0;
  }
  /* The root's last child is the body (a split, or one side alone): it
     grows, and a viewer atop a stack in it (the editor, the PDF) fills. */
  .screen.fill > :global(.stack > :last-child) {
    flex: 1;
    min-height: 0;
  }
  .screen.fill > :global(.stack > .rich.tall:last-child) {
    height: auto;
    min-height: 240px;
  }
  .screen.fill :global(.split .pane > .stack > .rich:first-child),
  .screen.fill > :global(.stack > .stack:last-child > .rich:first-child) {
    flex: 1;
    height: auto;
    min-height: 240px;
  }
  /* Narrow: the same body in tabs; the open tab fills. */
  .screen.fill > :global(.stack > .tabs:last-child),
  .screen.fill > :global(.stack > .tabs:last-child > .tabpanel) {
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .screen.fill > :global(.stack > .tabs:last-child > .tabpanel) {
    flex: 1;
  }
  .screen.fill > :global(.stack > .tabs:last-child > .tabpanel > *) {
    flex: 1;
    min-height: 0;
  }
  .screen.fill > :global(.stack > .tabs:last-child > .tabpanel > .rich) {
    height: auto;
  }
  .screen.fill > :global(.stack > .tabs:last-child > .tabpanel > .stack > .rich:first-child) {
    flex: 1;
    height: auto;
    min-height: 240px;
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
