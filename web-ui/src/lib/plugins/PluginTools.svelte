<script lang="ts">
  /**
   * An installed plugin's Tools section (docs/plugin-platform-plan.md §8):
   * each side program it can download, with what is on this host (version,
   * size) and the one action that fits — Install, Update (the plugin now
   * names a newer version), Remove. The daemon downloads from the declared
   * address, checks the sha256, unpacks and runs the setup steps; this only
   * asks and shows the answer. A tool with no build for this computer says
   * so instead of offering Install.
   */
  import { onDestroy } from "svelte";
  import { fetchTools, installTool, progressWords, removeTool, sizeWords, type ToolState } from "./platform";

  interface Props {
    plugin: string;
    name: string;
    /** Held, faulted or switched off everywhere: Install waits for it. */
    canInstall: boolean;
  }

  let { plugin, name, canInstall }: Props = $props();

  let tools = $state<ToolState[] | null>(null);
  let error = $state<string | null>(null);
  let working = $state<{ tool: string; what: "install" | "remove" } | null>(null);
  let rowError = $state<{ tool: string; text: string } | null>(null);

  async function load(): Promise<void> {
    try {
      tools = await fetchTools(plugin);
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  $effect(() => {
    void plugin;
    void load();
  });

  // While one installs, its progress is read every second — while anyone
  // can see it (a hidden page skips the tick; the next visible one catches
  // up, and the install's own answer reloads the row at the end).
  let poll: ReturnType<typeof setInterval> | null = null;
  onDestroy(() => {
    if (poll !== null) clearInterval(poll);
  });

  async function run(t: ToolState, what: "install" | "remove"): Promise<void> {
    working = { tool: t.tool, what };
    rowError = null;
    if (what === "install") {
      poll = setInterval(() => {
        if (document.visibilityState === "visible") void load();
      }, 1000);
    }
    try {
      if (what === "install") await installTool(plugin, t.tool);
      else await removeTool(plugin, t.tool);
    } catch (e) {
      rowError = { tool: t.tool, text: e instanceof Error ? e.message : String(e) };
    } finally {
      if (poll !== null) clearInterval(poll);
      poll = null;
      working = null;
      await load();
    }
  }

  function stateWords(t: ToolState): string {
    if (t.installed === null) return "Not installed";
    const size = t.installed.bytes > 0 ? ` · ${sizeWords(t.installed.bytes)}` : "";
    return `${t.installed.version} installed${size}`;
  }
</script>

{#if error !== null}
  <p class="err">{error}</p>
{:else if tools !== null && tools.length > 0}
  <div class="tools" aria-label="{name}'s tools">
    {#each tools as t (t.tool)}
      {@const busy = working?.tool === t.tool || t.installing}
      <div class="tool">
        <div class="text">
          <span class="title">{t.name}</span>
          <span class="state">{stateWords(t)}</span>
          {#if t.download === null && t.installed === null}
            <span class="state">No build for this computer</span>
          {:else if t.download !== null && (t.installed === null || !t.current)}
            <span class="state">
              {t.installed === null ? "" : `${t.version} is available · `}{t.download.size !== null
                ? `${sizeWords(t.download.size)} from `
                : "from "}{t.download.host}
            </span>
          {/if}
          {#if t.installing}
            {@const p = progressWords(t.progress)}
            <span class="state">{p.text}</span>
            <span class="bar" class:indeterminate={p.fraction === null}
              ><span style:width={p.fraction === null ? undefined : `${Math.round(p.fraction * 100)}%`}></span></span
            >
          {/if}
          {#if rowError?.tool === t.tool}<span class="err">{rowError.text}</span>{/if}
        </div>
        <div class="actions">
          {#if t.download !== null && (t.installed === null || !t.current)}
            <button
              class="opt small"
              class:primary={t.installed === null}
              disabled={busy || !canInstall}
              title={canInstall
                ? `Downloads ${t.name} ${t.version}, checks it, and keeps it in ${name}'s own folder; nothing else on this computer changes`
                : `${name} has to be able to run here first`}
              onclick={() => void run(t, "install")}
            >
              {#if busy && working?.what !== "remove"}
                {t.installed === null ? "Installing…" : "Updating…"}
              {:else}
                {t.installed === null ? "Install" : "Update"}
              {/if}
            </button>
          {/if}
          {#if t.installed !== null}
            <button
              class="opt small quiet"
              disabled={busy}
              title="Removes every version of {t.name} that {name} downloaded"
              onclick={() => void run(t, "remove")}
            >
              {working?.tool === t.tool && working.what === "remove" ? "Removing…" : "Remove"}
            </button>
          {/if}
        </div>
      </div>
    {/each}
  </div>
{/if}

<style>
  .tools {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .tool {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 6px 10px;
    border: 1px solid var(--edge);
    border-radius: 8px;
  }
  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .title {
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .state {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .actions {
    flex: none;
    display: flex;
    gap: 6px;
  }
  .bar {
    display: block;
    height: 3px;
    margin-top: 3px;
    border-radius: 2px;
    background: color-mix(in srgb, var(--accent) 15%, transparent);
    overflow: hidden;
    max-width: 260px;
  }
  .bar > span {
    display: block;
    height: 100%;
    background: var(--accent);
    transition: width 0.4s ease;
  }
  .bar.indeterminate > span {
    width: 30%;
    animation: tool-slide 1.4s ease-in-out infinite;
  }
  @keyframes tool-slide {
    from {
      transform: translateX(-100%);
    }
    to {
      transform: translateX(340%);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .bar.indeterminate > span {
      animation: none;
    }
  }
  .err {
    color: var(--err);
    font-size: var(--text-xs);
    margin: 0;
  }
</style>
