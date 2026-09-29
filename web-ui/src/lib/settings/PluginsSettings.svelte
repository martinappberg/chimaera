<script lang="ts">
  /**
   * Settings → Plugins: each installed plugin's declared settings
   * (`[[settings]]`, docs/plugin-platform-plan.md §9), a second, declared
   * source beside `schema.ts` (the way Environment and Documents are
   * bespoke). Drawn by `plugins/PluginSettings.svelte`, the same rows the
   * plugin's card shows. Renders its own <h2>.
   */
  import { workspacePlugins } from "../plugins/store";
  import PluginSettings from "../plugins/PluginSettings.svelte";

  interface Props {
    /** The search box's text, lowercased: narrows to plugins it names. */
    query?: string;
  }

  let { query = "" }: Props = $props();

  const withSettings = $derived(
    ($workspacePlugins?.plugins ?? []).filter(
      (p) =>
        p.installed &&
        p.platform.settings.length > 0 &&
        (query === "" ||
          p.name.toLowerCase().includes(query) ||
          p.platform.settings.some((s) => s.label.toLowerCase().includes(query))),
    ),
  );
</script>

<h2 class="cat">Plugins</h2>
{#if $workspacePlugins === null}
  <p class="lead">Open a workspace to see its plugins' settings.</p>
{:else if withSettings.length === 0}
  <p class="lead">No installed plugin has settings.</p>
{:else}
  {#each withSettings as p (p.id)}
    <PluginSettings plugin={p.id} name={p.name} wsId={$workspacePlugins.workspace_id} heading />
  {/each}
{/if}

<style>
  /* SettingsView's own section heading, so the section reads as one. */
  .cat {
    margin: 18px 0 4px;
    padding: 0 14px;
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .lead {
    color: var(--muted);
    font-size: var(--text-sm);
    margin: 0 0 8px;
    padding: 0 14px;
  }
</style>
