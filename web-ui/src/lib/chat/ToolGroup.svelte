<script lang="ts">
  import ActivityRows from "./ActivityRows.svelte";
  import ActivitySummary from "./ActivitySummary.svelte";
  import type { OpenPathFn, PathResolver } from "./paths";
  import type { ChatBlock, SubagentInfo } from "./store.svelte";
  import { subagentTitle } from "./subagentView";
  import ToolCallCard from "./ToolCallCard.svelte";
  import { isLive, toolGroupTitle, toolRunHealth, type TurnTail } from "./toolLabels";
  import ModSite from "./ModSite.svelte";
  import type { ModsController } from "./mods.svelte";
  import { pageVisible } from "../shared/visibility";

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
    /** The turn's later calls, where a retry clears a failure here. */
    tail?: TurnTail;
    /** Open a tool's locations (resolved against the session first). */
    onOpenPath?: OpenPathFn;
    resolvePaths?: PathResolver;
    /** Background/stop a running row (claude only — the host omits these
     *  for agents without the capability). Called with the tool row id. */
    onBackground?: (id: string) => void;
    onStopTask?: (id: string) => void;
    /** What the agent has said about each subagent, by tool row id. */
    subagents?: ReadonlyMap<string, SubagentInfo>;
    modelName?: (id: string) => string;
    /** Open a subagent's own conversation (its agent handle + label). */
    onOpenSubagent?: (agentId: string, title: string, newSplit: boolean) => void;
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
    mods?: ModsController;
  }

  let {
    tools,
    tail,
    onOpenPath,
    resolvePaths,
    onBackground,
    onStopTask,
    subagents,
    modelName,
    onOpenSubagent,
    visible = true,
    sourceIndex,
    sourceEnd,
    sourceUid,
    mods,
  }: Props = $props();

  /** The opener for one agent row, once the wire has named its subagent. */
  function openSubagentOf(tool: Extract<ChatBlock, { kind: "tool" }>) {
    const agentId = tool.tool === "agent" ? subagents?.get(tool.id)?.agentId : null;
    if (agentId == null || onOpenSubagent === undefined) return undefined;
    return (newSplit: boolean) => onOpenSubagent?.(agentId, subagentTitle(tool.title), newSplit);
  }

  const running = $derived(tools.some(isLive));
  /** Failed / recovered badge — see toolLabels.ts. */
  const health = $derived(toolRunHealth(tools, tail));

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
    <ActivityRows>
      {#each tools as tool (tool.id)}
        {#snippet coreTool()}
        <ToolCallCard
          block={tool}
          {visible}
          {onOpenPath}
          {resolvePaths}
          onBackground={onBackground !== undefined ? () => onBackground?.(tool.id) : undefined}
          onStop={onStopTask !== undefined ? () => onStopTask?.(tool.id) : undefined}
          subagent={tool.tool === "agent" ? subagents?.get(tool.id) : undefined}
          {modelName}
          onOpenSubagent={openSubagentOf(tool)}
        />
        {/snippet}
        {#if mods && tool.nativeName && tool.nativeInput !== undefined}
          <ModSite {mods} component="ToolUse" instanceId={tool.id} active={visible && $pageVisible} props={{ tool_use_id: tool.id, tool: tool.nativeName, input: tool.nativeInput, isRunning: isLive(tool), isErrored: tool.status === "failed", isInterrupted: false, ...(tool.nativeOutput !== undefined ? { output: tool.nativeOutput } : {}) }}>
            {#snippet children(_draw)}{@render coreTool()}{/snippet}
          </ModSite>
        {:else}{@render coreTool()}{/if}
      {/each}
    </ActivityRows>
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
</style>
