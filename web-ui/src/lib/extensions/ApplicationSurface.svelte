<script lang="ts">
  import { untrack } from "svelte";
  import {
    ApplicationSurfaceSession,
    type ApplicationExtension, type ApplicationRuntime, type HostSurfaceActions,
    type SurfaceIdentity, type SurfaceKind, type SurfacePresentation, type SurfaceStatus,
  } from "./application";

  let { kind, identity, presentation, runtime, extension, actions }: {
    kind: SurfaceKind; identity: SurfaceIdentity; presentation: SurfacePresentation;
    runtime: ApplicationRuntime; extension: ApplicationExtension | null; actions: HostSurfaceActions;
  } = $props();
  let root = $state<HTMLDivElement>();
  let session: ApplicationSurfaceSession | null = null;
  let status = $state<SurfaceStatus>("absent");
  $effect(() => {
    // A new original identity creates a new owner; visibility updates do not.
    const container = root;
    if (!container) return;
    const view = identity;
    const module = extension;
    const service = runtime;
    const surface = kind;
    const callbacks = actions;
    const original = untrack(() => presentation);
    let owner: ApplicationSurfaceSession;
    try {
      owner = new ApplicationSurfaceSession(container, surface, view, original, service, module, callbacks,
        (next) => { status = next; });
    } catch {
      status = "failed";
      return;
    }
    session = owner;
    status = owner.status;
    owner.start();
    return () => {
      owner.close();
      if (session === owner) session = null;
    };
  });
  $effect(() => { const next = presentation; untrack(() => session?.update(next)); });
</script>

<div class="application-surface">
  <div bind:this={root} class="application-target"></div>
  {#if status === "absent"}
    <p role="status">This optional extension is not available.</p>
  {:else if status === "loading"}
    <p role="status">Loading…</p><button onclick={() => session?.cancel()}>Cancel</button>
  {:else if status === "failed"}
    <p role="status">This view couldn’t be loaded.</p>
    <button onclick={() => session?.retry()}>Retry</button>
    <button onclick={() => session?.cancel()}>Cancel</button>
  {:else if status === "closed"}
    <p role="status">This view has closed.</p>
  {/if}
</div>

<style>
  .application-surface { height: 100%; color: var(--fg); background: var(--bg); }
  .application-target { display: contents; }
  p { margin: 16px; color: var(--muted); }
  button { margin: 0 0 16px 16px; padding: 6px 10px; color: var(--fg); background: var(--bg); border: 1px solid var(--edge); border-radius: 6px; font: inherit; }
  button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 3px; }
</style>
