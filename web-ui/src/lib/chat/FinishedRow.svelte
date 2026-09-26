<script lang="ts">
  import Chevron from "../shared/Chevron.svelte";
  import Markdown from "./Markdown.svelte";
  import type { OpenPathFn, PathResolver } from "./paths";
  import type { EmbedResolver } from "./embeds";
  import type { ChatBlock } from "./store.svelte";

  /**
   * Work that ran beside the conversation just ended — a subagent, a
   * background command, or a monitor watch — folded into the transcript where
   * the end landed. One quiet line in the tool-group voice: what finished,
   * how, and its footprint. A subagent's report (untrusted markdown, capped by
   * the driver) opens beneath; a background task links its output file.
   */
  interface Props {
    block: Extract<ChatBlock, { kind: "finished" }>;
    /** Open a file the task wrote (its full output). */
    onOpenFile?: (path: string) => void;
    onOpenPath?: OpenPathFn;
    resolvePaths?: PathResolver;
    /** Local images in the report render as embed cards. */
    embeds?: EmbedResolver;
    visible?: boolean;
    sourceIndex?: number;
    sourceUid?: number;
  }
  let {
    block,
    onOpenFile,
    onOpenPath,
    resolvePaths,
    embeds,
    visible = true,
    sourceIndex,
    sourceUid,
  }: Props = $props();

  let open = $state(false);

  const tone = $derived(
    block.status === "failed" ? "bad" : block.status === "stopped" ? "quiet" : "ok",
  );
  /** Subagents speak in the agent's verb; background tasks already arrive as
   *  the CLI's own full sentence (or "“desc” completed"). */
  const title = $derived(
    block.source === "agent"
      ? `Agent “${block.title}” ${block.status === "completed" ? "finished" : block.status}`
      : block.title,
  );
  const expandable = $derived(block.result !== null && block.result.trim() !== "");
</script>

<div
  class="finished activity {tone}"
  data-block-index={sourceIndex}
  data-block-uid={sourceUid}
>
  <div class="head">
  <button
    class="line"
    class:expandable
    aria-expanded={expandable ? open : undefined}
    disabled={!expandable}
    onclick={() => (open = !open)}
    title={expandable ? (open ? "hide the report" : "show the report") : undefined}
  >
    <span class="icon" aria-hidden="true">
      {#if tone === "bad"}
        <svg viewBox="0 0 16 16" width="12" height="12"
          ><path d="M5 5l6 6M11 5l-6 6" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" /></svg
        >
      {:else if tone === "quiet"}
        <svg viewBox="0 0 16 16" width="12" height="12"
          ><rect x="5" y="5" width="6" height="6" rx="1" fill="currentColor" /></svg
        >
      {:else if block.source === "monitor"}
        <svg viewBox="0 0 16 16" width="12" height="12"
          ><circle cx="8" cy="8" r="2" fill="currentColor" /><circle
            cx="8"
            cy="8"
            r="5"
            fill="none"
            stroke="currentColor"
            stroke-width="1.3"
          /></svg
        >
      {:else}
        <svg viewBox="0 0 16 16" width="12" height="12"
          ><path
            d="M3.5 8.5l3 3 6-7"
            fill="none"
            stroke="currentColor"
            stroke-width="1.6"
            stroke-linecap="round"
            stroke-linejoin="round"
          /></svg
        >
      {/if}
    </span>
    <span class="title">{title}</span>
    {#if block.stats !== null}
      <span class="stats">{block.stats}</span>
    {/if}
    {#if expandable}
      <Chevron {open} />
    {/if}
  </button>
  {#if block.outputFile !== null && onOpenFile !== undefined}
    <button
      class="output"
      title={block.outputFile}
      onclick={() => onOpenFile?.(block.outputFile as string)}>output</button
    >
  {/if}
  </div>
  {#if open && block.result !== null}
    <div class="report">
      <Markdown text={block.result} streaming={false} {visible} {onOpenPath} {resolvePaths} {embeds} />
    </div>
  {/if}
</div>

<style>
  .finished {
    margin: 1px 0;
    animation: rise 0.15s ease; /* @keyframes rise lives in app.css */
  }
  @media (prefers-reduced-motion: reduce) {
    .finished {
      animation: none;
    }
  }
  /* The line shrinks (its title ellipsizes) before the output link wraps. */
  .head {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .line {
    flex: 0 1 auto;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    max-width: 100%;
    margin-left: -6px;
    padding: 2px 6px;
    background: none;
    border: none;
    border-radius: 6px;
    color: var(--activity-fg, var(--muted));
    font: inherit;
    font-size: var(--text-xs);
    line-height: 1.4;
    text-align: left;
    cursor: default;
  }
  .line.expandable {
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }
  .line.expandable:hover,
  .line.expandable:focus-visible {
    color: var(--fg);
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }
  .line:disabled {
    opacity: 1;
  }
  .icon {
    display: inline-flex;
    flex: none;
  }
  /* Success is the expected outcome — it stays in the row's quiet voice;
     only a failure earns colour. */
  .ok .icon {
    color: color-mix(in srgb, var(--accent) 55%, var(--muted));
  }
  .bad .icon,
  .bad .title {
    color: var(--err);
  }
  .title {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .stats {
    flex: none;
    color: color-mix(in srgb, var(--muted) 75%, transparent);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .stats::before {
    content: "· ";
  }
  /* A quiet affordance: plain muted text, underlined only on hover. */
  .output {
    flex: none;
    padding: 0 4px;
    background: none;
    border: none;
    border-radius: 4px;
    color: color-mix(in srgb, var(--muted) 70%, transparent);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .output:hover,
  .output:focus-visible {
    color: var(--fg);
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .report {
    margin: 2px 0 8px 2px;
    padding: 2px 0 2px 12px;
    border-left: 2px solid color-mix(in srgb, var(--edge) 80%, transparent);
    font-size: var(--text-sm);
  }
</style>
