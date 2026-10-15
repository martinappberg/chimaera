<script lang="ts">
  import { untrack } from "svelte";
  import type { Snippet } from "svelte";
  import { foldTitle } from "./activityFold";
  import ActivityRows from "./ActivityRows.svelte";
  import ActivitySummary from "./ActivitySummary.svelte";
  import type { ChatBlock } from "./store.svelte";
  import { isLive, spanLabel, toolRunHealth, toolSpanMs, type TurnTail } from "./toolLabels";
  import { liveClock } from "./liveClock.svelte";

  /**
   * A settled run of thought and tool lines, folded (see activityFold.ts).
   * Collapsed it is one quiet line; expanded, the original rows render as
   * they did live, each still openable. The rows mount only while open, so a
   * long transcript's folds cost one line each.
   */
  interface Props {
    /** Every tool call in the run, across its groups. */
    tools: Extract<ChatBlock, { kind: "tool" }>[];
    /** The turn's calls after the fold, where a retry clears a failure. */
    tail?: TurnTail;
    thoughts: number;
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
    /** Local clock minus the journal's (`ChatStore.clockSkewMs`), so a live
     *  elapsed never counts a remote daemon's clock offset. */
    clockSkewMs?: number;
  }

  let {
    tools,
    tail,
    thoughts,
    steps,
    visible = true,
    sourceIndex,
    sourceEnd,
    sourceUid,
    children,
    clockSkewMs = 0,
  }: Props = $props();

  let open = $state(false);
  const title = $derived(foldTitle(thoughts, tools));
  /** Work can outlive the reply or finished line that settled its run: a
   *  backgrounded or cross-turn call. A run a mid-turn row follows is kept
   *  open by ChatView until its calls end. */
  const running = $derived(tools.some(isLive));
  /** Only a hard failure surfaces on the fold; a recovered one is the
   *  inner group's detail. */
  const failed = $derived(toolRunHealth(tools, tail) === "failed");
  /** The run's tool time, read the way a group line reads it. */
  const clock = liveClock(() => running && visible);
  // Only a shown live line follows the shared tick; a hidden tab's line
  // reads the time once and stays still.
  const elapsed = $derived(
    spanLabel(toolSpanMs(tools, (running ? (visible ? clock.ms : untrack(() => clock.ms)) : 0) - clockSkewMs)),
  );
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
    tooltip={open ? "hide these steps" : `show ${steps} steps`}
    {running}
    health={failed ? "failed" : null}
    {elapsed}
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
