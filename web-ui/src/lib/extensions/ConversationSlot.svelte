<script module lang="ts">
  import { selectedApplication } from "./selected";
  /** A build without the optional extension never mounts anything here. */
  const mountable = selectedApplication !== null;
</script>
<script lang="ts">
  /**
   * A conversation's own place for something only the optional extension can
   * answer, beside its permission and question cards. Today that is a setup
   * command the agent proposed for this project. Free, signed-out and no-plan
   * windows, browser views, and conversations that proposed nothing render
   * nothing here: no element, no request, no transcript scan.
   */
  import { untrack } from "svelte";
  import { writable } from "svelte/store";
  import { isNativeShell } from "../net/native";
  import { paidPlan } from "../net/plan";
  import { conversationProposals, sameProposals } from "./proposals";

  let { workspaceId, sessionId, blocks, visible }: {
    workspaceId: string | null;
    sessionId: string;
    blocks: readonly { kind: string; nativeName?: string; nativeInput?: unknown }[];
    visible: boolean;
  } = $props();

  // Native only: the decision is the native shell's profile command.
  const active = $derived(mountable && workspaceId !== null && isNativeShell() && $paidPlan !== null);
  const proposals = $derived(active ? conversationProposals(blocks) : []);
  const present = $derived(proposals.length > 0);

  // Republished only when the list itself changes, not on every streamed block.
  const proposalStore = writable<readonly string[]>([]);
  let published: readonly string[] = [];
  $effect(() => {
    const next = proposals;
    if (sameProposals(published, next)) return;
    published = Object.freeze([...next]);
    proposalStore.set(published);
  });
  const visibility = writable(true);
  $effect(() => { visibility.set(visible); });

  let target = $state<HTMLDivElement>();
  $effect(() => {
    const host = target;
    const workspace = workspaceId;
    const session = untrack(() => sessionId);
    if (!present || host === undefined || workspace === null || selectedApplication?.mountConversation === undefined) return;
    const controller = new AbortController();
    let owner: { dispose(): void } | null = null;
    void selectedApplication.mountConversation(host, {
      version: 1, workspaceId: workspace, sessionId: session, signal: controller.signal,
      visibility, proposals: proposalStore,
    }).then((mounted) => {
      if (controller.signal.aborted) mounted.dispose(); else owner = mounted;
    }).catch(() => {
      // An extension that cannot mount leaves the conversation as it was.
    });
    return () => {
      controller.abort();
      try { owner?.dispose(); } catch { /* The slot is already empty. */ }
      owner = null;
    };
  });
</script>

{#if present}<div class="conversation-slot" bind:this={target}></div>{/if}

<style>
  /* The extension's card sits in the conversation's own flow. */
  .conversation-slot { display: contents; }
</style>
