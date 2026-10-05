<script lang="ts">
  import type { Snippet } from "svelte";
  import { finishedTitle } from "./activityFold";
  import ActivityRows from "./ActivityRows.svelte";
  import ActivitySummary from "./ActivitySummary.svelte";
  import type { ChatBlock } from "./store.svelte";

  /**
   * A settled run of finished-work lines, folded (see activityFold.ts):
   * several background tasks and subagents that ended together read as one
   * line saying what ended and how. Expanded, each line is as it was — its
   * report and output link included. The rows mount only while open.
   */
  interface Props {
    rows: Extract<ChatBlock, { kind: "finished" }>[];
    /** False while a retained chat tab is hidden. */
    visible?: boolean;
    /** Absolute transcript span + the first row's uid, for ChatView's
     *  scroll-anchor policy (same contract as ActivityFold). */
    sourceIndex: number;
    sourceEnd: number;
    sourceUid: number;
    children: Snippet;
  }

  let { rows, visible = true, sourceIndex, sourceEnd, sourceUid, children }: Props = $props();

  let open = $state(false);
  const title = $derived(finishedTitle(rows));
  const failed = $derived(rows.some((row) => row.status === "failed"));
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
    tooltip={open ? "hide these lines" : `show all ${rows.length}`}
    health={failed ? "failed" : null}
    {visible}
    onToggle={() => (open = !open)}
  />
  {#if open}
    <ActivityRows>{@render children()}</ActivityRows>
  {/if}
</div>

<style>
  .fold {
    margin: 1px 0;
  }
</style>
