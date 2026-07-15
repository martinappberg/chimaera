<script lang="ts">
  import Chevron from "../shared/Chevron.svelte";
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
   */
  interface Props {
    /** Subagent tool rows still in flight (kind "agent", in_progress). */
    agents: Extract<ChatBlock, { kind: "tool" }>[];
    /** Stop a subagent (claude stop_task). Omitted when unsupported. */
    onStop?: (id: string) => void;
  }
  let { agents, onStop }: Props = $props();

  /** Collapsed by default — the header line ("✳ 2 subagents working") is the
   *  glance; the per-agent detail is one click away, so the tray stays a quiet
   *  one-line indicator until you want more. */
  let open = $state(false);

  /** The driver titles these "Agent: {description}" — the prefix is the tray's
   *  own label, so drop it from each row. */
  function name(title: string): string {
    return title.startsWith("Agent: ") ? title.slice(7) : title;
  }
  function progress(b: Extract<ChatBlock, { kind: "tool" }>): string {
    return b.content?.kind === "output" ? (b.content.text ?? "").trim() : "";
  }
</script>

<div class="tray">
  <button
    class="tray-head"
    aria-expanded={open}
    onclick={() => (open = !open)}
    title={open ? "collapse" : "expand"}
  >
    <Chevron {open} />
    <span class="spark" aria-hidden="true">✳</span>
    <!-- aria-live on the summary only: the count changing is worth announcing,
         each agent's per-second progress line is not. -->
    <span class="head-label" role="status" aria-live="polite"
      >{agents.length === 1 ? "subagent working" : `${agents.length} subagents working`}</span
    >
  </button>
  {#if open}
    <div class="rows">
      {#each agents as agent (agent.id)}
        <div class="agent">
          <span class="dot" aria-hidden="true"></span>
          <div class="body">
            <span class="name">{name(agent.title)}</span>
            {#if progress(agent)}
              <span class="progress">{progress(agent)}</span>
            {/if}
          </div>
          {#if onStop !== undefined}
            <button class="stop" title="stop this subagent" onclick={() => onStop?.(agent.id)}>
              <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
                <rect x="4.5" y="4.5" width="7" height="7" rx="1" fill="currentColor" />
              </svg>
            </button>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  /* A pinned monitor strip, sibling to the plan panel — same quiet chrome
     (border-top, muted, theme tokens) so the two read as one work tray. */
  .tray {
    flex: none;
    border-top: 1px solid var(--edge);
    padding: 5px 14px 6px;
    font-size: var(--text-sm);
    max-height: 168px;
    overflow-y: auto;
    background: color-mix(in srgb, var(--accent) 4%, transparent);
    animation: rise 0.15s ease; /* shared keyframe in app.css */
  }
  /* The whole header is the collapse toggle (button reset), so the one-line
     summary is the click target — like the ToolGroup summary. */
  .tray-head {
    display: flex;
    align-items: center;
    gap: 7px;
    width: 100%;
    background: none;
    border: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    padding: 1px 0 3px;
    cursor: pointer;
  }
  .head-label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .spark {
    flex: none;
    color: var(--accent);
    /* Presence, not alarm — the same slow breathe as the composer stop ring. */
    animation: tray-breathe 1.8s ease-in-out infinite;
  }
  .agent {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 2px 0;
  }
  .dot {
    flex: none;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
    animation: pulse 1.4s ease-in-out infinite; /* shared keyframe in app.css */
  }
  .body {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: 8px;
    overflow: hidden;
  }
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
  .stop {
    flex: none;
    display: inline-flex;
    background: none;
    border: none;
    color: var(--muted);
    padding: 2px;
    cursor: pointer;
    transition: color 0.12s ease;
  }
  .stop:hover {
    color: var(--err);
  }
  @keyframes tray-breathe {
    0%,
    100% {
      opacity: 0.9;
    }
    50% {
      opacity: 0.5;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .tray,
    .dot,
    .spark {
      animation: none;
    }
  }
</style>
