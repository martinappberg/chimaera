<script lang="ts">
  import Chevron from "../shared/Chevron.svelte";
  import type { ChatBlock } from "./store.svelte";
  import ToolCallCard from "./ToolCallCard.svelte";

  /**
   * A run of consecutive tool calls, condensed. Collapsed it is one quiet
   * summary row ("6 commands · 2 files"); expanded it is a light list of
   * rows, each openable for its output. It stays open while anything is still
   * running, then collapses — a finished run is history, whether it succeeded
   * or not (the badge carries the verdict).
   */
  interface Props {
    tools: Extract<ChatBlock, { kind: "tool" }>[];
    onOpenFile?: (path: string) => void;
    /** Background/stop a running row (claude only — the host omits these
     *  for agents without the capability). Called with the tool row id. */
    onBackground?: (id: string) => void;
    onStopTask?: (id: string) => void;
    /** This is the live tail group of a running turn — tools stream in as
     *  separate frames, so hold it open even in the momentary gap where every
     *  already-seen tool has finished but the next hasn't landed yet. Without
     *  this the group collapses and re-expands between commands (the flicker). */
    active?: boolean;
  }

  let { tools, onOpenFile, onBackground, onStopTask, active = false }: Props = $props();

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

  /** null = follow the auto rule; a bool = the user's explicit choice. */
  let userOpen = $state<boolean | null>(null);
  /** Auto-open tracks LIVE work only. A failure deliberately does NOT force
   *  the group open: the red badge already says it failed, the row is one
   *  click away, and the agent's own next message almost always explains it —
   *  so springing a wall of rows open re-litigates a problem the reader may
   *  have already understood, and does it retroactively as history scrolls. */
  const open = $derived(userOpen ?? (running || active));

  /** "6 commands · 2 files · 1 step" — only the non-zero parts, so a pure
   *  command run reads cleanly. Edits are counted by distinct file. */
  const summary = $derived.by(() => {
    let commands = 0;
    let steps = 0;
    const files = new Set<string>();
    for (const t of tools) {
      if (t.tool === "edit") {
        if (t.locations.length > 0) for (const l of t.locations) files.add(l);
        else steps++;
      } else if (t.tool === "execute") {
        commands++;
      } else {
        steps++;
      }
    }
    const parts: string[] = [];
    const plural = (n: number, w: string) => `${n} ${w}${n === 1 ? "" : "s"}`;
    if (commands > 0) parts.push(plural(commands, "command"));
    if (files.size > 0) parts.push(plural(files.size, "file"));
    if (steps > 0) parts.push(plural(steps, "step"));
    return parts.length > 0 ? parts.join(" · ") : plural(tools.length, "tool");
  });
</script>

<div class="group" class:failed class:running>
  <button
    class="summary"
    aria-expanded={open}
    onclick={() => (userOpen = !open)}
    title={open ? "collapse tool activity" : "expand tool activity"}
  >
    <Chevron {open} />
    <span class="label">{summary}</span>
    {#if failed}
      <span class="badge bad">failed</span>
    {:else if running}
      <span class="badge run">running…</span>
    {:else if recovered}
      <span class="badge soft">recovered</span>
    {/if}
  </button>
  {#if open}
    <div class="rows">
      {#each tools as tool (tool.id)}
        <ToolCallCard
          block={tool}
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
    border: 1px solid color-mix(in srgb, var(--edge) 65%, transparent);
    border-radius: 8px;
    margin: 4px 0;
    background: color-mix(in srgb, var(--fg) 2%, transparent);
    overflow: hidden;
    animation: rise 0.15s ease; /* @keyframes rise lives in app.css */
  }
  @media (prefers-reduced-motion: reduce) {
    .group {
      animation: none;
    }
  }
  .summary {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 5px 10px;
    background: none;
    border: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    cursor: pointer;
    transition: background-color 0.12s ease;
  }
  .summary:hover {
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }
  .label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono, monospace);
  }
  .badge {
    flex: none;
    font-size: var(--text-xs);
    font-family: var(--mono, monospace);
    padding: 0 6px;
    border-radius: 999px;
  }
  .badge.bad {
    color: var(--err);
    background: color-mix(in srgb, var(--err) 12%, transparent);
  }
  .badge.run {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }
  /* Recovered: worth a glance, not an alarm. */
  .badge.soft {
    color: var(--muted);
    background: color-mix(in srgb, var(--fg) 7%, transparent);
  }
  .rows {
    border-top: 1px solid color-mix(in srgb, var(--edge) 55%, transparent);
  }
</style>
