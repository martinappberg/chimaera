<script module lang="ts">
  import { selectedApplication } from "./selected";
  /** A build without the optional extension never mounts anything here. */
  const mountable = selectedApplication !== null;
</script>
<script lang="ts">
  /**
   * One quiet line the optional extension may add under a remote machine's
   * Home row (whether its sign-in stays connected while you're away). Free,
   * signed-out and no-plan windows render nothing here: no element, no
   * request.
   */
  import { isNativeShell } from "../net/native";
  import { paidPlan } from "../net/plan";
  import { pageVisible } from "../shared/visibility";

  let { alias }: { alias: string } = $props();
  const active = $derived(mountable && isNativeShell() && $paidPlan !== null);
  let target = $state<HTMLDivElement>();
  $effect(() => {
    const host = target;
    const name = alias;
    if (!active || host === undefined || selectedApplication?.mountHost === undefined) return;
    const controller = new AbortController();
    let owner: { dispose(): void } | null = null;
    void selectedApplication.mountHost(host, { version: 1, alias: name, signal: controller.signal, visibility: pageVisible })
      .then((mounted) => { if (controller.signal.aborted) mounted.dispose(); else owner = mounted; })
      .catch(() => { /* The row stays exactly as it was. */ });
    return () => {
      controller.abort();
      try { owner?.dispose(); } catch { /* The slot is already empty. */ }
      owner = null;
    };
  });
</script>

{#if active}<div class="host-slot" bind:this={target}></div>{/if}

<style>
  /* The mounted line takes the host row's status register: the UI font at
     the status size. `display: contents` adds no box, but custom properties
     still inherit through it, so content that asks for `var(--mono)` gets the
     UI font here without the extension changing. */
  .host-slot { display: contents; --mono: var(--ui-font); }
</style>
