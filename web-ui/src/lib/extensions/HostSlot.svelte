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
  // The prop reads through the parent's host row, which every host-list
  // refresh replaces; a derived string changes only when the alias does, so
  // a refresh never retires and remounts the line (it popped out and back).
  const name = $derived(alias);
  const active = $derived(mountable && isNativeShell() && $paidPlan !== null);
  let target = $state<HTMLDivElement>();
  $effect(() => {
    const host = target;
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
     the status size, set by the row's line stack this slot sits in.
     `display: contents` adds no box, so the mounted lines are items of that
     stack; custom properties still inherit through it, so content that asks
     for `var(--mono)` gets the UI font. */
  .host-slot { display: contents; --mono: var(--ui-font); }
</style>
