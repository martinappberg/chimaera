<script lang="ts">
  import WorkTray from "../shared/WorkTray.svelte";
  import WorkTrayRow from "../shared/WorkTrayRow.svelte";
  import type { ChatBlock } from "./store.svelte";

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
    /** Stop a subagent (claude stop_task). Omitted when unsupported. */
    onStop?: (id: string) => void;
    /** False while the owning retained chat tab is hidden. */
    visible?: boolean;
  }
  let { agents, onStop, visible = true }: Props = $props();

  /** The driver titles these "Agent: {description}" — the prefix is the tray's
   *  own label, so drop it from each row. */
  function name(title: string): string {
    for (const prefix of ["Agent: ", "Task: "]) {
      if (title.startsWith(prefix)) return title.slice(prefix.length);
    }
    return title;
  }
  function progress(b: Extract<ChatBlock, { kind: "tool" }>): string {
    return b.content?.kind === "output" ? (b.content.text ?? "").trim() : "";
  }
</script>

<WorkTray
  glyph="✳"
  {visible}
  label={agents.length === 1 ? "subagent working" : `${agents.length} subagents working`}
>
  {#each agents as agent (agent.id)}
    <WorkTrayRow
      onStop={onStop !== undefined ? () => onStop?.(agent.id) : undefined}
      stopTitle="stop this subagent"
    >
      <span class="name">{name(agent.title)}</span>
      {#if progress(agent)}
        <span class="progress">{progress(agent)}</span>
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
