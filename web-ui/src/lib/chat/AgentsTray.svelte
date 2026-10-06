<script lang="ts">
  import WorkTray from "../shared/WorkTray.svelte";
  import WorkTrayRow from "../shared/WorkTrayRow.svelte";
  import SubagentChip from "./SubagentChip.svelte";
  import ModelChip from "./ModelChip.svelte";
  import type { ChatBlock, SubagentInfo } from "./store.svelte";
  import { subagentTitle } from "./subagentView";

  /**
   * A live monitor for the subagents running RIGHT NOW, pinned above the
   * composer next to the plan. Subagents otherwise live only as "Agent:" rows
   * buried inside collapsed tool groups — easy to lose when several run in
   * parallel or the run scrolls away. This is the workbench move Claude Desktop
   * can't make: long-lived parallel work gets a stable, glanceable surface
   * instead of scrolling off in the chat. Collapsed by default to a single
   * "N subagents working" line; expand for each agent's latest progress line
   * (tools · tokens, from task_progress) and a stop button. When an agent
   * finishes it drops out of the tray but keeps its in-place row in history.
   * Chrome lives in the shared WorkTray/WorkTrayRow shell (sibling:
   * BackgroundTray).
   */
  interface Props {
    /** Subagent tool rows still in flight (kind "agent", pending/running). */
    agents: Extract<ChatBlock, { kind: "tool" }>[];
    /** What the agent has said about each subagent, by tool row id — the
     *  model chip, and the handle its conversation opens by. */
    subagents?: ReadonlyMap<string, SubagentInfo>;
    /** A model id as the picker labels it. */
    modelName?: (id: string) => string;
    /** Open a subagent's own conversation (its agent handle + label). */
    onOpen?: (agentId: string, title: string, newSplit: boolean) => void;
    /** Stop a subagent (claude stop_task). Omitted when unsupported. */
    onStop?: (id: string) => void;
    /** False while the owning retained chat tab is hidden. */
    visible?: boolean;
  }
  let { agents, subagents, modelName, onOpen, onStop, visible = true }: Props = $props();

  const name = subagentTitle;
  function progress(b: Extract<ChatBlock, { kind: "tool" }>): string {
    return b.content?.kind === "output" ? (b.content.text ?? "").trim() : "";
  }
  /** One agent: name it and its latest step, so the collapsed tray alone
   *  says what is being worked on; several: the count (expand for each). */
  const label = $derived.by(() => {
    if (agents.length !== 1) return `${agents.length} subagents working`;
    const step = progress(agents[0]).split("\n", 1)[0];
    return `subagent “${name(agents[0].title)}”${step !== "" ? ` · ${step}` : " working"}`;
  });
</script>

<WorkTray glyph="✳" {visible} {label}>
  {#each agents as agent (agent.id)}
    <WorkTrayRow
      onStop={onStop !== undefined ? () => onStop?.(agent.id) : undefined}
      stopTitle="stop this subagent"
      {visible}
    >
      {@const info = subagents?.get(agent.id)}
      <SubagentChip
        onOpen={info?.agentId != null && onOpen !== undefined
          ? (newSplit) => onOpen?.(info.agentId as string, name(agent.title), newSplit)
          : undefined}
      />
      <span class="name">{name(agent.title)}</span>
      {#if progress(agent)}
        <span class="progress">{progress(agent)}</span>
      {/if}
      {#if info?.model}
        <ModelChip model={info.model} {modelName} />
      {/if}
    </WorkTrayRow>
  {/each}
</WorkTray>

<style>
  .name {
    flex: none;
    max-width: 60%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg);
    font-family: var(--mono, monospace);
  }
  .progress {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    font-size: var(--text-xs);
  }
</style>
