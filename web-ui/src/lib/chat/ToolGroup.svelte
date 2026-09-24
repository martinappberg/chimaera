<script lang="ts">
  import Chevron from "../shared/Chevron.svelte";
  import type { ChatBlock } from "./store.svelte";
  import ToolCallCard from "./ToolCallCard.svelte";
  import { toolGroupTitle } from "./toolLabels";

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
  /** A failure is RECOVERED when a later call of the same tool against the
   *  same target completed (the read-before-write dance, a retried command) —
   *  a net-success run shouldn't wear the hard red badge. Presentation only:
   *  the failed row inside still shows its own error. Denials never recover
   *  (the user said no); a failure with no matching later success stays hard. */
  const failed = $derived.by(() =>
    tools.some((t, i) => {
      if (t.denied) return true;
      if (t.status !== "failed") return false;
      const sameTarget = (s: (typeof tools)[number]) =>
        s.tool === t.tool &&
        (t.locations.length > 0
          ? s.locations.some((l) => t.locations.includes(l))
          : s.title === t.title);
      return !tools.some(
        (s, j) => j > i && s.status === "completed" && !s.denied && sameTarget(s),
      );
    }),
  );
  const recovered = $derived(
    !failed && tools.some((t) => t.status === "failed" || t.denied),
  );

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
  class:failed
  class:running
  class:visible
  data-block-index={sourceIndex}
  data-block-end={sourceEnd}
  data-block-uid={sourceUid}
>
  <button
    class="summary"
    aria-expanded={open}
    onclick={() => (open = !open)}
    title={open ? "hide tool activity" : `show tool activity — ${tools.length} call${tools.length === 1 ? "" : "s"}`}
  >
    {#if running}
      <span class="live-dot" aria-hidden="true"></span>
    {/if}
    <span class="label">{title}</span>
    {#if failed}
      <span class="badge bad">failed</span>
    {:else if recovered}
      <span class="badge soft">recovered</span>
    {/if}
    <Chevron {open} />
  </button>
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
  /* A quiet line, not a card: the Claude/Codex apps render tool runs as a
     muted sentence with a trailing chevron, so prose stays the page's voice. */
  .summary {
    display: inline-flex;
    align-items: center;
    gap: 6px;
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
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }
  .summary:hover,
  .summary:focus-visible {
    color: var(--fg);
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }
  .summary :global(.chev) {
    opacity: 0.55;
  }
  .summary:hover :global(.chev) {
    opacity: 1;
  }
  .label {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* Running: a small breathing dot instead of a badge (the title already
     says what is running, in the present tense). */
  .live-dot {
    flex: none;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
    animation: live-breathe 1.6s ease-in-out infinite;
  }
  @keyframes live-breathe {
    0%,
    100% {
      opacity: 0.35;
    }
    50% {
      opacity: 1;
    }
  }
  :global(html.app-hidden) .live-dot,
  .group:not(.visible) .live-dot {
    animation-play-state: paused;
  }
  @media (prefers-reduced-motion: reduce) {
    .live-dot {
      animation: none;
    }
  }
  .group.failed .summary {
    color: color-mix(in srgb, var(--err) 80%, var(--muted));
  }
  .badge {
    flex: none;
    font-size: var(--text-xs);
    padding: 0 6px;
    border-radius: 999px;
  }
  .badge.bad {
    color: var(--err);
    background: color-mix(in srgb, var(--err) 12%, transparent);
  }
  /* Recovered: worth a glance, not an alarm. */
  .badge.soft {
    color: var(--muted);
    background: color-mix(in srgb, var(--fg) 7%, transparent);
  }
  .rows {
    margin: 2px 0 6px 2px;
    padding-left: 10px;
    border-left: 2px solid color-mix(in srgb, var(--edge) 70%, transparent);
  }
</style>
