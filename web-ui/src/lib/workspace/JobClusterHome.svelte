<script lang="ts">
  /**
   * A cluster workspace window's way to the rest of its cluster: the
   * cluster page, over the workspace (which stays mounted underneath). Its
   * own chimaera knows only this one workspace, so the page is where every
   * workspace on the cluster is — the jobs, what's open in each, and where
   * to open more. Its row for this window's workspace says so, and Open
   * there (or the back button) returns to it.
   */
  import { onMount } from "svelte";
  import ClusterPage from "./ClusterPage.svelte";
  import { listHosts, type HostState } from "../net/native";

  interface Props {
    alias: string;
    /** This window's cluster workspace (`cws=`). */
    here: string;
    /** Its name, for the back button. */
    hereName: string;
    onClose: () => void;
  }

  let { alias, here, hereName, onClose }: Props = $props();

  let hosts = $state<HostState[]>([]);

  function refreshHosts(): void {
    listHosts()
      .then((list) => (hosts = list))
      .catch(() => {
        // the page still works without the host row (no override state)
      });
  }

  onMount(refreshHosts);
</script>

<div class="cluster-home" role="region" aria-label="{alias} — every workspace on the cluster">
  <div class="drag" data-tauri-drag-region aria-hidden="true"></div>
  <ClusterPage
    {alias}
    host={hosts.find((h) => h.alias === alias) ?? null}
    onBack={onClose}
    backLabel={hereName}
    {here}
    onHere={onClose}
    onHostState={(state) => {
      hosts = hosts.map((h) => (h.alias === state.alias ? state : h));
    }}
    onHostsChanged={refreshHosts}
  />
</div>

<style>
  .cluster-home {
    position: fixed;
    inset: 0;
    /* Over the workspace; under the context menu (90) its menus open in. */
    z-index: 60;
    overflow-y: auto;
    background: var(--bg);
  }

  /* The native window's 32px drag strip, which the page's top padding
     leaves clear. */
  .drag {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    height: 32px;
  }
</style>
