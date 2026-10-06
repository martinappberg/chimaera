<script module lang="ts">
  import { selectedApplication } from "./selected";
  /** A build without the optional extension never mounts anything here. */
  const placeable = selectedApplication !== null;
</script>
<script lang="ts">
  /**
   * The window's host indicator ("local" beside the connection dot). The host
   * always renders its own label; with the optional extension, an active plan
   * and a local project, the extension may take the label's place to say
   * where that project's work runs (`PlaceMount.claim`). Everyone else gets
   * exactly the label, with no wrapper element and no request.
   */
  import type { Snippet } from "svelte";
  import { writable } from "svelte/store";
  import { paidPlan } from "../net/plan";
  import { pageVisible } from "../shared/visibility";
  import { cloudOnboarding } from "../pro/onboarding.svelte";
  import type { Session } from "../workspace/sessions";
  import type { PlaceSession } from "./application";

  let { workspaceId, sessions, label }: {
    /** The local project this window shows, or null (Home, a remote window,
     *  a browser view): null never mounts the extension. */
    workspaceId: string | null;
    sessions: Session[];
    label: Snippet;
  } = $props();

  let target = $state<HTMLSpanElement>();
  let claimed = $state(false);
  const active = $derived(placeable && workspaceId !== null && $paidPlan !== null);

  const toPlace = (rows: Session[]): readonly PlaceSession[] => Object.freeze(rows.map(row => Object.freeze({
    id: row.id,
    agentKind: row.kind === "agent" ? row.agent_kind ?? "claude" : null,
    remote: typeof row.placement === "object" && row.placement !== null && typeof row.placement.remote === "string" ? row.placement.remote : null,
  })));
  // One store per slot; the extension subscribes and sees each new list.
  const placeSessions = writable<readonly PlaceSession[]>([]);
  $effect(() => { placeSessions.set(toPlace(sessions)); });

  $effect(() => {
    const host = target;
    const id = workspaceId;
    if (!active || host === undefined || id === null || selectedApplication?.mountPlace === undefined) return;
    const controller = new AbortController();
    const signal = controller.signal;
    let owner: { dispose(): void } | null = null;
    void import("./placeHost").then(({ runHere, runInCloud }) => {
      if (signal.aborted) throw new Error("Place retired");
      return selectedApplication!.mountPlace!(host, {
        version: 1, workspaceId: id, signal,
        claim: (value) => { if (!signal.aborted) claimed = value === true; },
        visibility: pageVisible,
        sessions: placeSessions,
        connectAgents: (providerIds) => {
          if (!signal.aborted) cloudOnboarding.request({ providerIds: [...providerIds], workspaceId: id });
        },
        runHere: () => signal.aborted ? Promise.resolve({ started: false, error: "unavailable" }) : runHere(id),
        runInCloud: () => signal.aborted ? Promise.resolve({ started: false, error: "unavailable" }) : runInCloud(id),
      });
    }).then((mounted) => {
      if (signal.aborted) mounted.dispose(); else owner = mounted;
    }).catch(() => {
      // An extension that cannot mount leaves the host's label as it was.
      if (!signal.aborted) claimed = false;
    });
    return () => {
      controller.abort();
      claimed = false;
      try { owner?.dispose(); } catch { /* The host label is already back. */ }
      owner = null;
    };
  });
</script>

{#if !claimed}{@render label()}{/if}
{#if active}<span class="place" bind:this={target}></span>{/if}

<style>
  /* The extension's own element takes the label's place in the row. */
  .place { display: contents; }
</style>
