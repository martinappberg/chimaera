<script lang="ts">
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import ModelChip from "./ModelChip.svelte";

  /**
   * The header of a subagent view, in place of the chat header: it says
   * this is NOT a chat of its own — whose subagent it is, what it was asked
   * to do, which model serves it, and that it is read-only (a subagent
   * answers to its parent, so there is nothing here to pick or send).
   */
  interface Props {
    agentKind: string;
    /** What the parent called the subagent when it started it. */
    title: string;
    /** The chat that started it, as the rail names it. */
    parentName: string;
    /** The model serving it, in the agent's own words; null until known. */
    model: string | null;
    /** A model id as the picker labels it. */
    modelName?: (id: string) => string;
    /** The agent's name for the kind of subagent ("general-purpose"). */
    agentType: string | null;
    /** Working, finished, or not known yet (the parent is still loading). */
    running: boolean | null;
    onOpenParent?: () => void;
  }
  let { agentKind, title, parentName, model, modelName, agentType, running, onOpenParent }: Props =
    $props();
</script>

<div class="subagent-header">
  <header class="strip">
    <span class="who">
      <SessionGlyph kind="agent" {agentKind} size={11} state={running === true ? "alive" : undefined} />
      <span class="label">Subagent of</span>
      {#if onOpenParent !== undefined}
        <button class="parent" title="Open the chat that started this subagent" onclick={onOpenParent}
          >{parentName}</button
        >
      {:else}
        <span class="parent-name">{parentName}</span>
      {/if}
    </span>
    <span class="title" {title}>{title}</span>
    {#if agentType !== null}
      <span class="chip" title="the kind of subagent">{agentType}</span>
    {/if}
    {#if model !== null}
      <ModelChip {model} {modelName} pill />
    {/if}
    <span class="end">
      {#if running !== null}
        <span class="state" class:working={running}>{running ? "working" : "finished"}</span>
      {/if}
      <span class="read-only" title="A subagent answers to the chat that started it. Message that chat to steer it."
        >read-only</span
      >
    </span>
  </header>
</div>

<style>
  .subagent-header {
    container: pane-chrome / inline-size;
    flex: none;
    min-width: 0;
  }
  .strip {
    display: flex;
    align-items: center;
    min-width: 0;
    height: var(--pane-toolbar-height);
    gap: 8px;
    padding: 0 8px;
    white-space: nowrap;
    border-bottom: 1px solid var(--edge);
    /* A step toward the accent: the strip itself says "not an ordinary chat". */
    background: color-mix(in srgb, var(--accent) 7%, color-mix(in srgb, var(--bg) 65%, var(--term-bg)));
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .who {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    flex: none;
    font-family: var(--mono);
    padding-right: 8px;
    border-right: 1px solid var(--edge);
  }
  .label {
    color: var(--fg);
  }
  .parent,
  .parent-name {
    max-width: 22ch;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--fg);
    font: inherit;
  }
  .parent {
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
    text-decoration: underline;
    text-decoration-color: color-mix(in srgb, var(--fg) 35%, transparent);
    text-underline-offset: 2px;
  }
  .parent:hover {
    color: var(--accent);
    text-decoration-color: currentColor;
  }
  .title {
    flex: 0 1 auto;
    min-width: 3ch;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--fg);
  }
  .chip {
    flex: none;
    padding: 0 6px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--fg) 6%, transparent);
    font-family: var(--mono);
  }
  .end {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    flex: none;
  }
  .state.working {
    color: var(--accent);
  }
  .read-only {
    padding: 0 6px;
    border: 1px solid color-mix(in srgb, var(--edge) 80%, transparent);
    border-radius: 999px;
  }
  /* A narrow pane keeps who and what; the kind is the first to go. */
  @container pane-chrome (max-width: 520px) {
    .chip {
      display: none;
    }
  }
  @container pane-chrome (max-width: 400px) {
    .read-only {
      display: none;
    }
  }
</style>
