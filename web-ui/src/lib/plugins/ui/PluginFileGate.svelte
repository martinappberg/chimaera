<script lang="ts">
  /**
   * Where a file meets the plugins active in its workspace
   * (docs/plugin-platform-plan.md §5): a file a plugin claims (`[[files]]`)
   * opens in that plugin's file view, with **Text** one click away; two
   * plugins claiming it offer the choice, remembered per workspace and file
   * kind. The bar above also carries the claiming plugins' status chips
   * (`slot = "status"`) and every matching file action (`[[actions]]`).
   * With none of that, the file shows exactly as it always did.
   */
  import type { Snippet } from "svelte";
  import { workspacePlugins } from "../store";
  import {
    actionsFor,
    claimsFor,
    openPluginView,
    openWith,
    postFileAction,
    rememberOpenWith,
    viewsIn,
    workspaceRelative,
  } from "../platform";
  import PluginScreen from "./PluginScreen.svelte";

  interface Props {
    path: string;
    wsRoot: string | null;
    children: Snippet;
  }

  let { path, wsRoot, children }: Props = $props();

  const wsId = $derived($workspacePlugins?.workspace_id ?? null);
  const plugins = $derived($workspacePlugins?.plugins ?? []);
  const rel = $derived(workspaceRelative(wsRoot, path));
  const claims = $derived(rel !== null ? claimsFor(plugins, rel) : []);
  const actions = $derived(rel !== null ? actionsFor(plugins, rel) : []);
  const chips = $derived(
    viewsIn(plugins, "status").filter((s) => claims.some((c) => c.plugin.id === s.plugin.id)),
  );

  /** A plugin id, `"text"`, or null (the first claimant). */
  let picked = $state<string | null>(null);
  $effect(() => {
    const first = claims[0];
    picked = wsId !== null && first !== undefined ? openWith(wsId, first.kind.label) : null;
  });
  const claim = $derived(
    picked === "text" ? null : (claims.find((c) => c.plugin.id === picked) ?? claims[0] ?? null),
  );

  function pick(choice: string): void {
    picked = choice;
    const first = claims[0];
    if (wsId !== null && first !== undefined) {
      rememberOpenWith(wsId, first.kind.label, choice === first.plugin.id ? null : choice);
    }
  }

  let running = $state<string | null>(null);
  let message = $state<{ text: string; bad: boolean } | null>(null);
  let timer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => () => clearTimeout(timer));

  async function runAction(pid: string, action: string): Promise<void> {
    if (wsId === null || rel === null || running !== null) return;
    running = `${pid}/${action}`;
    try {
      const answer = await postFileAction(wsId, pid, action, rel);
      if (answer.message !== null) message = { text: answer.message, bad: false };
      if (answer.open !== null) openPluginView(pid, answer.open.view);
    } catch (e) {
      message = { text: e instanceof Error ? e.message : String(e), bad: true };
    } finally {
      running = null;
      clearTimeout(timer);
      timer = setTimeout(() => (message = null), 6000);
    }
  }
</script>

{#if wsId === null || rel === null || (claims.length === 0 && actions.length === 0)}
  {@render children()}
{:else}
  <div class="gate">
    <div class="bar">
      {#each chips as c (`${c.plugin.id}/${c.view.id}`)}
        <div class="chip" title={c.view.title}>
          <PluginScreen ws={wsId} {wsRoot} plugin={c.plugin.id} view={c.view.id} file={rel} compact />
        </div>
      {/each}
      {#if message !== null}
        <span class="message" class:bad={message.bad} role="status">{message.text}</span>
      {/if}
      <span class="spacer"></span>
      {#each actions as a (`${a.plugin.id}/${a.action.action}`)}
        <button
          class="opt small quiet"
          title="{a.action.label} ({a.plugin.name})"
          disabled={running !== null}
          onclick={() => void runAction(a.plugin.id, a.action.action)}>{a.action.label}</button
        >
      {/each}
      {#if claims.length > 0}
        <div class="switch" role="tablist" aria-label="open with">
          {#each claims as c (c.plugin.id)}
            <button
              class="seg"
              class:on={claim?.plugin.id === c.plugin.id}
              role="tab"
              aria-selected={claim?.plugin.id === c.plugin.id}
              title="Open with {c.plugin.name}"
              onclick={() => pick(c.plugin.id)}>{claims.length > 1 ? c.plugin.name : c.kind.label}</button
            >
          {/each}
          <button class="seg" class:on={claim === null} role="tab" aria-selected={claim === null} onclick={() => pick("text")}
            >Text</button
          >
        </div>
      {/if}
    </div>
    <div class="body">
      {#if claim !== null}
        <div class="screen-scroll">
          {#key `${claim.plugin.id}/${claim.view.id}/${rel}`}
            <PluginScreen ws={wsId} {wsRoot} plugin={claim.plugin.id} view={claim.view.id} file={rel} />
          {/key}
        </div>
      {:else}
        {@render children()}
      {/if}
    </div>
  </div>
{/if}

<style>
  .gate {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .bar {
    flex: none;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    padding: 4px 10px;
    border-bottom: 1px solid var(--edge);
    min-height: 30px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
  }
  .spacer {
    flex: 1;
  }
  .message {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .message.bad {
    color: var(--err);
  }
  .switch {
    display: inline-flex;
    border: 1px solid var(--edge);
    border-radius: 6px;
    overflow: hidden;
  }
  .seg {
    appearance: none;
    border: none;
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    padding: 2px 10px;
    cursor: pointer;
  }
  .seg + .seg {
    border-left: 1px solid var(--edge);
  }
  .seg.on {
    background: var(--row-active);
    color: var(--fg);
  }
  .body {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .body > :global(*) {
    flex: 1;
    min-height: 0;
  }
  .screen-scroll {
    overflow: auto;
    background: var(--bg);
  }
</style>
