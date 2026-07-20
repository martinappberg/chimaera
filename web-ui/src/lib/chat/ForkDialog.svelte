<script lang="ts">
  /**
   * Destination picker for a non-destructive transcript branch. Same-agent
   * exact boundaries advertise the native protocol; every cross-agent choice
   * uses Chimaera's normalized transcript handoff.
   */
  import SessionGlyph from "../shared/SessionGlyph.svelte";

  interface AgentChoice {
    id: string;
    name: string;
  }

  interface Props {
    agents: AgentChoice[];
    sourceAgent: string;
    nativeAt: string | null;
    applying: boolean;
    onCancel: () => void;
    onConfirm: (agent: string) => void;
  }

  let { agents, sourceAgent, nativeAt, applying, onCancel, onConfirm }: Props = $props();
</script>

<div class="dialog-veil">
  <div class="dialog" role="dialog" aria-label="fork conversation">
    <div class="dialog-title">fork the conversation from here</div>
    <div class="dialog-note">
      copies the transcript into a new session; this session keeps running
    </div>

    <div class="agent-list">
      {#each agents as agent (agent.id)}
        {@const native = agent.id === sourceAgent && nativeAt !== null}
        <button class="agent" disabled={applying} onclick={() => onConfirm(agent.id)}>
          <SessionGlyph kind="agent" agentKind={agent.id} size={16} />
          <span class="identity">
            <span class="name">{agent.name}</span>
            <span class="method">
              {native
                ? "native fork"
                : agent.id === sourceAgent
                  ? "portable handoff · no exact native boundary here"
                  : "portable transcript handoff"}
            </span>
          </span>
          <span class="arrow">{applying ? "…" : "→"}</span>
        </button>
      {/each}
    </div>

    <div class="dialog-actions">
      <button class="opt quiet" disabled={applying} onclick={onCancel}>cancel</button>
    </div>
  </div>
</div>

<style>
  .dialog-veil {
    position: absolute;
    inset: 0;
    background: color-mix(in srgb, var(--bg) 55%, transparent);
    display: grid;
    place-items: center;
    z-index: 30;
    animation: fade 0.12s ease;
  }
  .dialog {
    width: min(410px, 90%);
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 8px;
    box-shadow: 0 10px 32px rgba(0, 0, 0, 0.28);
    padding: 12px 14px;
    font-size: var(--text-sm);
    animation: rise 0.14s ease;
  }
  .dialog-title {
    color: var(--fg);
  }
  .dialog-note {
    color: var(--muted);
    margin-top: 4px;
  }
  .agent-list {
    display: grid;
    gap: 5px;
    margin-top: 12px;
  }
  .agent {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: center;
    gap: 9px;
    min-width: 0;
    border: 1px solid var(--edge);
    border-radius: 7px;
    padding: 8px 9px;
    background: color-mix(in srgb, var(--fg) 3%, transparent);
    color: var(--fg);
    text-align: left;
    cursor: pointer;
  }
  .agent:hover:not(:disabled) {
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }
  .agent:disabled {
    opacity: 0.65;
    cursor: default;
  }
  .identity {
    display: grid;
    gap: 2px;
    min-width: 0;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .method {
    color: var(--muted);
    font-size: var(--text-xs);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .arrow {
    color: var(--accent);
  }
  .dialog-actions {
    display: flex;
    justify-content: flex-end;
    margin-top: 10px;
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .dialog-veil,
    .dialog {
      animation: none;
    }
  }
</style>
