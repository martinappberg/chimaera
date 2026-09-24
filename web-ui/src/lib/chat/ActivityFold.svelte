<script lang="ts">
  import type { Snippet } from "svelte";
  import { foldTitle } from "./activityFold";
  import ActivitySummary from "./ActivitySummary.svelte";
  import type { ChatBlock } from "./store.svelte";
  import { toolRunHealth } from "./toolLabels";

  /**
   * A settled run of activity lines, folded under the reply it led to (see
   * activityFold.ts). Collapsed it is one quiet line; expanded, the original
   * rows render as they did live, each still openable. The rows mount only
   * while open, so a long transcript's folds cost one line each.
   */
  interface Props {
    /** Every tool call in the run, across its groups. */
    tools: Extract<ChatBlock, { kind: "tool" }>[];
    thoughts: number;
    finished: number;
    /** How many lines the fold holds (the tooltip's count). */
    steps: number;
    /** False while a retained chat tab is hidden. */
    visible?: boolean;
    /** Absolute transcript span + the first row's uid, for ChatView's
     *  scroll-anchor policy (same contract as ToolGroup). */
    sourceIndex: number;
    sourceEnd: number;
    sourceUid: number;
    children: Snippet;
  }

  let {
    tools,
    thoughts,
    finished,
    steps,
    visible = true,
    sourceIndex,
    sourceEnd,
    sourceUid,
    children,
  }: Props = $props();

  let open = $state(false);
  const title = $derived(foldTitle(thoughts, tools, finished));
  /** Backgrounded or cross-turn work can outlive the reply that followed it. */
  const running = $derived(
    tools.some((t) => t.status === "in_progress" || t.status === "pending"),
  );
  /** Only a hard failure surfaces on the fold; a recovered one is the
   *  inner group's detail. */
  const failed = $derived(toolRunHealth(tools) === "failed");
</script>

<div
  class="fold activity"
  data-block-index={sourceIndex}
  data-block-end={sourceEnd}
  data-block-uid={sourceUid}
>
  <ActivitySummary
    label={title}
    {open}
    tooltip={open ? "hide these steps" : `show the ${steps} steps before this reply`}
    {running}
    health={failed ? "failed" : null}
    {visible}
    onToggle={() => (open = !open)}
  />
  {#if open}
    <div class="rows">
      {@render children()}
    </div>
  {/if}
</div>

<style>
  .fold {
    margin: 1px 0;
  }
  .rows {
    margin: 2px 0 6px 2px;
    padding-left: 10px;
    border-left: 2px solid color-mix(in srgb, var(--edge) 70%, transparent);
  }
  /* The rows cluster as they do in the column (ChatView's .activity rule). */
  .rows > :global(.activity + .activity) {
    margin-top: -2px;
  }
</style>
