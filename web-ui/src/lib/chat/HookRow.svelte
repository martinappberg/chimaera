<script lang="ts">
  import Chevron from "../shared/Chevron.svelte";
  import type { HookNotice } from "./hookNotice";

  /**
   * What a hook said, as one quiet line (the hook's name + its first line,
   * faded) with the whole text a click away — the same voice as the thought
   * and tool-group rows. A hook can print a report after every turn; it must
   * not outweigh the reply above it. The body keeps the hook's own line
   * breaks and indents, and is mounted only while open.
   */
  interface Props {
    said: HookNotice;
    /** Absolute transcript index + stable uid, for ChatView's scroll anchor. */
    sourceIndex: number;
    sourceUid: number;
  }

  let { said, sourceIndex, sourceUid }: Props = $props();

  let open = $state(false);
  const preview = $derived(said.lines.find((line) => line.trim() !== "")?.trim() ?? "");
</script>

<details class="hook-row activity" bind:open data-block-index={sourceIndex} data-block-uid={sourceUid}>
  <summary title="show what the hook said">
    <span class="hook-title">{said.hook} hook</span>
    <span class="hook-preview">{preview}</span>
    <Chevron {open} />
  </summary>
  {#if open}
    <div class="hook-body">{said.lines.join("\n")}</div>
  {/if}
</details>

<style>
  .hook-row {
    margin: 1px 0;
  }
  .hook-row > summary {
    display: flex;
    align-items: center;
    gap: 6px;
    width: fit-content;
    max-width: 100%;
    margin-left: -6px;
    padding: 2px 6px;
    border-radius: 6px;
    color: var(--activity-fg, var(--muted));
    font-size: var(--text-xs);
    line-height: 1.4;
    cursor: pointer;
    user-select: none;
    list-style: none;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }
  .hook-row > summary::-webkit-details-marker {
    display: none;
  }
  .hook-row > summary:hover,
  .hook-row > summary:focus-visible {
    color: var(--fg);
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }
  .hook-row > summary :global(.chev) {
    flex: none;
    opacity: 0.55;
  }
  /* A hook's name can be long (an MCP tool's): it gives way before the row
     overflows, the preview first. */
  .hook-title {
    flex: 0 1 auto;
    min-width: 6ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .hook-preview {
    flex: 0 100 auto;
    min-width: 0;
    max-width: 48ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: color-mix(in srgb, var(--muted) 60%, transparent);
  }
  .hook-row[open] .hook-preview {
    display: none;
  }
  .hook-body {
    color: var(--muted);
    font-size: var(--text-sm);
    line-height: 1.55;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    border-left: 2px solid color-mix(in srgb, var(--edge) 70%, transparent);
    padding: 2px 0 2px 12px;
    margin: 2px 0 8px 2px;
  }
</style>
