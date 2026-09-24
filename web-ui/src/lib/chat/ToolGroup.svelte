<script lang="ts">
  import ActivitySummary from "./ActivitySummary.svelte";
  import type { ChatBlock } from "./store.svelte";
  import ToolCallCard from "./ToolCallCard.svelte";
  import { toolGroupTitle, toolRunHealth } from "./toolLabels";

  /**
   * A run of consecutive tool calls, condensed. Collapsed it is one quiet
   * line — the agent's own batch label ("Listed files in directory") or a
   * readable count ("Ran 2 commands, read a file"); expanded it is a light
   * list of rows, each openable for its output. Groups start collapsed even
   * while running: a breathing dot and the present-tense title carry live
   * status without turning the transcript into a wall of command rows.
   */
  interface Props {
    tools: Extract<ChatBlock, { kind: "tool" }>[];
    onOpenFile?: (path: string) => void;
    /** Background/stop a running row (claude only — the host omits these
     *  for agents without the capability). Called with the tool row id. */
    onBackground?: (id: string) => void;
    onStopTask?: (id: string) => void;
    /** False while a retained chat tab is hidden; suppresses layout work in
     *  streaming child rows without destroying their expanded state. */
    visible?: boolean;
    /** Absolute transcript index used by ChatView's scroll-anchor policy. */
    sourceIndex?: number;
    /** Inclusive source end; a prepended page can merge adjacent tool runs. */
    sourceEnd?: number;
    /** First tool row's stable block uid — the anchor policy's trim-proof
     *  identity (index labels go stale when the reducer cap trims). */
    sourceUid?: number;
  }

  let {
    tools,
    onOpenFile,
    onBackground,
    onStopTask,
    visible = true,
    sourceIndex,
    sourceEnd,
    sourceUid,
  }: Props = $props();

  const running = $derived(
    tools.some((t) => t.status === "in_progress" || t.status === "pending"),
  );
  /** Failed / recovered badge — see toolLabels.ts. */
  const health = $derived(toolRunHealth(tools));

  /** Tool history is opt-in detail. Running and failure state remain visible
   *  in the summary badge, while an explicit user toggle persists as rows
   *  stream into this keyed group. */
  let open = $state(false);

  /** The agent's own batch labels, else readable counts ("Ran 2 commands,
   *  read a file") — see toolLabels.ts. The full list stays in the tooltip. */
  const title = $derived(toolGroupTitle(tools));
</script>

<div
  class="group activity"
  class:visible
  data-block-index={sourceIndex}
  data-block-end={sourceEnd}
  data-block-uid={sourceUid}
>
  <ActivitySummary
    label={title}
    {open}
    tooltip={open ? "hide tool activity" : `show tool activity — ${tools.length} call${tools.length === 1 ? "" : "s"}`}
    {running}
    {health}
    {visible}
    onToggle={() => (open = !open)}
  />
  {#if open}
    <div class="rows">
      {#each tools as tool (tool.id)}
        <ToolCallCard
          block={tool}
          {visible}
          {onOpenFile}
          onBackground={onBackground !== undefined ? () => onBackground?.(tool.id) : undefined}
          onStop={onStopTask !== undefined ? () => onStopTask?.(tool.id) : undefined}
        />
      {/each}
    </div>
  {/if}
</div>

<style>
  .group {
    margin: 1px 0;
    animation: rise 0.15s ease; /* @keyframes rise lives in app.css */
  }
  .group:not(.visible) {
    animation: none;
  }
  @media (prefers-reduced-motion: reduce) {
    .group {
      animation: none;
    }
  }
  .rows {
    margin: 2px 0 6px 2px;
    padding-left: 10px;
    border-left: 2px solid color-mix(in srgb, var(--edge) 70%, transparent);
  }
</style>
