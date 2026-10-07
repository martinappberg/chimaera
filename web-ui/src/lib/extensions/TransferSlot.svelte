<script module lang="ts">
  import { selectedApplication } from "./selected";
  /** A build without the optional extension never mounts anything here. */
  const mountable = selectedApplication?.mountTransfer !== undefined;
</script>
<script lang="ts">
  /**
   * The cover over a workspace whose project is moving (to the cloud or
   * back). The host renders nothing of its own: with the optional extension,
   * an active plan and a project, the extension draws into this element and
   * asks the host to make everything underneath inert (`onblock`). Everyone
   * else gets no element and no request. What the extension shows, and when,
   * is its own reading of the window's daemon or of the browser view's
   * placement.
   */
  import { paidPlan } from "../net/plan";
  import { pageVisible } from "../shared/visibility";
  import { projectMoving } from "../net/placement";

  let { workspaceId, source, onblock }: {
    /** The project this window shows, or null (Home): null never mounts. */
    workspaceId: string | null;
    /** Where the extension reads the transfer from. */
    source: "daemon" | "placement";
    /** The host makes the workspace inert while true. */
    onblock: (blocked: boolean) => void;
  } = $props();

  let target = $state<HTMLDivElement>();
  const active = $derived(mountable && workspaceId !== null && $paidPlan !== null);

  $effect(() => {
    const host = target;
    const id = workspaceId;
    if (!active || host === undefined || id === null || selectedApplication?.mountTransfer === undefined) return;
    const controller = new AbortController();
    const signal = controller.signal;
    let owner: { dispose(): void } | null = null;
    selectedApplication.mountTransfer(host, {
      version: 1, workspaceId: id, signal,
      visibility: pageVisible,
      block: (blocked) => { if (!signal.aborted) onblock(blocked === true); },
      source,
      moving: projectMoving,
    }).then((mounted) => {
      if (signal.aborted) mounted.dispose(); else owner = mounted;
    }).catch(() => {
      // An extension that cannot mount leaves the workspace usable.
      if (!signal.aborted) onblock(false);
    });
    return () => {
      controller.abort();
      onblock(false);
      try { owner?.dispose(); } catch { /* The workspace is already usable. */ }
      owner = null;
    };
  });
</script>

{#if active}<div class="transfer" bind:this={target}></div>{/if}

<style>
  /* The extension's own element sits over the workspace; empty, it is nothing. */
  .transfer { display: contents; }
</style>
