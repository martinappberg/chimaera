<script lang="ts">
  import Chevron from "../shared/Chevron.svelte";
  import Markdown from "./Markdown.svelte";
  import type { OpenPathFn, PathResolver } from "./paths";
  import { thoughtPreview } from "./thoughtText";

  /**
   * Reasoning is secondary to prose: one quiet line (label + the thought's
   * title or first line, faded), the full text a click away — the same voice
   * as the tool-group and finished rows. The body is agent markdown (codex
   * titles each section in bold), rendered through the sanitizer and mounted
   * only while open, so a long transcript's thoughts cost one line each.
   */
  interface Props {
    text: string;
    /** The turn's streaming tail: "Thinking", and the newest section title. */
    live: boolean;
    /** False while a retained chat tab is hidden. */
    visible?: boolean;
    onOpenPath?: OpenPathFn;
    resolvePaths?: PathResolver;
    /** Absolute transcript index + stable uid, for ChatView's scroll anchor. */
    sourceIndex: number;
    sourceUid: number;
  }

  let { text, live, visible = true, onOpenPath, resolvePaths, sourceIndex, sourceUid }: Props = $props();

  let open = $state(false);
  const preview = $derived(thoughtPreview(text, live));
</script>

<details class="thought activity" bind:open data-block-index={sourceIndex} data-block-uid={sourceUid}>
  <summary title="show the agent's reasoning">
    <span class="thought-title" class:live>{live ? "Thinking" : "Thought"}</span>
    <span class="thought-preview">{preview}</span>
    <Chevron {open} />
  </summary>
  {#if open}
    <div class="thought-body">
      <Markdown {text} streaming={live} {visible} {onOpenPath} {resolvePaths} />
    </div>
  {/if}
</details>

<style>
  .thought {
    margin: 1px 0;
  }
  .thought > summary {
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
  .thought > summary::-webkit-details-marker {
    display: none;
  }
  .thought > summary:hover,
  .thought > summary:focus-visible {
    color: var(--fg);
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }
  .thought > summary :global(.chev) {
    opacity: 0.55;
  }
  .thought-title {
    flex: none;
  }
  .thought-title.live {
    animation: label-pulse 1.6s ease-in-out infinite;
  }
  :global(html.app-hidden) .thought-title.live {
    animation-play-state: paused;
  }
  /* ChatView's status label breathes the same way. */
  @keyframes label-pulse {
    0%,
    100% {
      opacity: 0.9;
    }
    50% {
      opacity: 0.55;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .thought-title.live {
      animation: none;
    }
  }
  .thought-preview {
    min-width: 0;
    max-width: 48ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: color-mix(in srgb, var(--muted) 60%, transparent);
    font-style: italic;
  }
  .thought[open] .thought-preview {
    display: none;
  }
  .thought-body {
    color: var(--muted);
    font-size: var(--text-sm);
    line-height: 1.55;
    border-left: 2px solid color-mix(in srgb, var(--edge) 70%, transparent);
    padding: 2px 0 2px 12px;
    margin: 2px 0 8px 2px;
  }
  /* The body keeps the row's quieter type: the markdown shell's prose size
     is the message voice. Section titles stay the body's color, bold. */
  .thought-body :global(.md) {
    font-size: inherit;
    line-height: inherit;
  }
  .thought-body :global(.md > :first-child) {
    margin-top: 0;
  }
  .thought-body :global(.md > :last-child) {
    margin-bottom: 0;
  }
</style>
