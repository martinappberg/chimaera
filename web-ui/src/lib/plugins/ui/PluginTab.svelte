<script lang="ts">
  /**
   * A plugin's tab view (`[[views]]` with `slot = "tab"`): its screen, or —
   * when the plugin is off here, gone, or no longer declares the view — one
   * plain line saying so, never a blank pane.
   */
  import { workspacePlugins } from "../store";
  import PluginScreen from "./PluginScreen.svelte";

  interface Props {
    wsId: string | null;
    wsRoot: string | null;
    plugin: string;
    view: string;
  }

  let { wsId, wsRoot, plugin, view }: Props = $props();

  const entry = $derived($workspacePlugins?.plugins.find((p) => p.id === plugin) ?? null);
  const decl = $derived(entry?.platform.views.find((v) => v.id === view && v.slot === "tab") ?? null);
</script>

<div class="plugin-tab">
  {#if wsId === null}
    <p class="hint">Open a workspace to see this.</p>
  {:else if $workspacePlugins === null}
    <!-- the workspace's plugins are still loading -->
  {:else if entry === null || decl === null}
    <p class="hint">This plugin view is no longer available.</p>
  {:else if !entry.active}
    <p class="hint">{entry.name} is off in this workspace. Switch it on in Extensions to see {decl.title}.</p>
  {:else}
    <PluginScreen ws={wsId} {wsRoot} {plugin} {view} />
  {/if}
</div>

<style>
  .plugin-tab {
    height: 100%;
    overflow: auto;
    background: var(--bg);
  }
  .hint {
    color: var(--muted);
    font-size: var(--text-sm);
    padding: 24px;
    margin: 0;
    text-align: center;
  }
</style>
