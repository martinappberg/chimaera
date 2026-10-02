<script lang="ts">
  /**
   * The dashboard's plugin panels (docs/design/plugin-platform-plan.md §3): each
   * active plugin's `[[views]]` with `slot = "panel"`, after core's own
   * sections, headed by the view's title and the plugin's name. Nothing
   * when no active plugin has one.
   */
  import { workspacePlugins } from "../plugins/store";
  import { viewsIn } from "../plugins/platform";
  import PluginScreen from "../plugins/ui/PluginScreen.svelte";

  interface Props {
    wsId: string | null;
    wsRoot: string | null;
  }

  let { wsId, wsRoot }: Props = $props();

  const panels = $derived(viewsIn($workspacePlugins?.plugins ?? [], "panel"));
</script>

{#if wsId !== null}
  {#each panels as p (`${p.plugin.id}/${p.view.id}`)}
    <section class="panel" aria-label={p.view.title}>
      <div class="shead">
        <span class="lbl">{p.view.title}</span>
        <span class="sub">from {p.plugin.name}</span>
      </div>
      <div class="card">
        <PluginScreen ws={wsId} {wsRoot} plugin={p.plugin.id} view={p.view.id} compact />
      </div>
    </section>
  {/each}
{/if}

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-width: 0;
  }
  .shead {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  .lbl {
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
  }
  .sub {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .card {
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 10px;
    padding: 14px 18px;
    min-width: 0;
  }
</style>
