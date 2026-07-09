<script lang="ts">
  import type { ChatBlock, ToolContent } from "./store.svelte";

  interface Props {
    block: Extract<ChatBlock, { kind: "tool" }>;
    /** Open a touched file in an adjacent pane (existing path-click flow). */
    onOpenFile?: (path: string) => void;
  }

  let { block, onOpenFile }: Props = $props();

  /** Rows start COLLAPSED — the title carries the what, the dot the state;
   *  output is one click away. Failures auto-expand (the error is the
   *  point), and a user toggle always wins afterwards. */
  let open = $state(false);
  let userToggled = false;
  $effect(() => {
    if (block.status === "failed" && !userToggled) open = true;
  });

  const GLYPHS: Record<string, string> = {
    execute: "❯",
    read: "≡",
    edit: "±",
    search: "⌕",
    fetch: "↓",
    delete: "✕",
    move: "→",
    think: "…",
    agent: "✳",
    other: "·",
  };
  const glyph = $derived(GLYPHS[block.tool] ?? "·");
  const hasBody = $derived(
    block.content !== null &&
      !(block.content.kind === "output" && (block.content.text ?? "").trim() === ""),
  );
  const statusTitle = $derived(
    block.denied ? "denied" : block.status.replace("_", " "),
  );

  // Live output follows its own tail (terminal-style) while streaming.
  let bodyEl = $state<HTMLElement | null>(null);
  $effect(() => {
    if (!block.streaming || block.content === null) return;
    void block.content.text;
    bodyEl?.scrollTo({ top: bodyEl.scrollHeight });
  });
</script>

{#snippet diff(d: ToolContent, showPath = true)}
  <div class="diff">
    {#if d.path && showPath}
      <button class="diff-path" onclick={() => onOpenFile?.(d.path ?? "")}>{d.path}</button>
    {/if}
    {#if d.old_text}
      <pre class="old">{d.old_text}</pre>
    {/if}
    <pre class="new">{d.new_text}</pre>
    {#if d.truncated}
      <span class="trunc">truncated — open the file for the full change</span>
    {/if}
  </div>
{/snippet}

<div class="tool" class:failed={block.status === "failed"} class:denied={block.denied}>
  <div class="head">
    <button
      class="expand"
      class:inert={!hasBody}
      onclick={() => {
        if (hasBody) {
          userToggled = true;
          open = !open;
        }
      }}
      aria-expanded={hasBody ? open : undefined}
      title={statusTitle}
    >
      <span class="glyph">{glyph}</span>
      <span class="title">{block.title}</span>
    </button>
    {#if block.locations.length > 0 && onOpenFile !== undefined}
      <!-- The workbench is right there: every located tool opens its file
           (image/markdown/csv/pdf land in their native previews). -->
      <button
        class="loc"
        title="open {block.locations[0]} in a pane"
        onclick={() => onOpenFile?.(block.locations[0])}
      >
        <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
          <path
            d="M6 4h6v6M12 4l-7 7"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
      </button>
    {/if}
    <span
      class="dot"
      class:run={block.status === "in_progress" || block.status === "pending"}
      class:ok={block.status === "completed" && !block.denied}
      class:bad={block.status === "failed" || block.denied}
    ></span>
  </div>
  {#if open && hasBody && block.content !== null}
    <div class="body" bind:this={bodyEl}>
      {#if block.content.kind === "output"}
        <pre>{block.content.text}</pre>
        {#if block.streaming}
          <span class="cursor" aria-hidden="true"></span>
        {/if}
        {#if block.content.truncated}
          <span class="trunc">output truncated</span>
        {/if}
      {:else if block.content.kind === "diff"}
        {@render diff(block.content)}
      {:else if block.content.kind === "batch"}
        {@const diffs = block.content.diffs ?? []}
        {#each diffs as d, i (i)}
          {@render diff(d, i === 0 || diffs[i - 1].path !== d.path)}
        {/each}
      {/if}
    </div>
  {/if}
</div>

<style>
  /* A row inside a ToolGroup — the group draws the container, so the row is
     borderless and quiet. A failed/denied row gets a soft left rail so the
     error stands out in a long list. */
  .tool {
    overflow: hidden;
  }
  .tool + :global(.tool) {
    border-top: 1px solid color-mix(in srgb, var(--edge) 40%, transparent);
  }
  .cursor {
    display: inline-block;
    width: 7px;
    height: 12px;
    background: var(--accent);
    vertical-align: text-bottom;
    animation: pulse 1s steps(2, jump-none) infinite;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 0 10px 0 0;
    color: var(--fg);
    font-size: var(--text-sm);
  }
  .expand {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 0 5px 10px;
    background: none;
    border: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
    transition: background-color 0.12s ease;
  }
  /* fg color-mix, not --row-hover: the card sits on --term-bg, where the
     solid row tokens tuned for --bg surfaces can clash. */
  .expand:not(.inert):hover {
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }
  .expand.inert {
    cursor: default;
  }
  .loc {
    flex: none;
    display: inline-flex;
    background: none;
    border: none;
    color: var(--muted);
    padding: 2px;
    cursor: pointer;
    transition: color 0.12s ease;
  }
  .loc:hover {
    color: var(--accent);
  }
  .glyph {
    color: var(--muted);
    font-family: var(--mono, monospace);
    flex: none;
  }
  .title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono, monospace);
  }
  .dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--muted);
  }
  .dot.run {
    background: var(--accent);
    animation: pulse 1.4s ease-in-out infinite;
  }
  .dot.ok {
    background: var(--accent);
  }
  .dot.bad {
    background: var(--err);
  }
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .dot.run,
    .cursor {
      animation: none;
      opacity: 0.55;
    }
  }
  .failed,
  .denied {
    box-shadow: inset 2px 0 0 color-mix(in srgb, var(--err) 60%, transparent);
  }
  .body {
    border-top: 1px solid var(--edge);
    padding: 6px 10px;
    max-height: 260px;
    overflow: auto;
    scrollbar-width: thin;
    scrollbar-color: color-mix(in srgb, var(--fg) 22%, transparent) transparent;
  }
  pre {
    margin: 0;
    font-size: var(--text-sm);
    font-family: var(--mono, monospace);
    white-space: pre-wrap;
    word-break: break-word;
  }
  .diff + .diff {
    margin-top: 8px;
  }
  .diff-path {
    display: block;
    background: none;
    border: none;
    padding: 0 0 4px;
    color: var(--accent);
    font-size: var(--text-sm);
    font-family: var(--mono, monospace);
    cursor: pointer;
    text-align: left;
  }
  .diff-path:hover {
    text-decoration: underline;
  }
  .old {
    background: color-mix(in srgb, var(--err) 12%, transparent);
    padding: 3px 6px;
    border-radius: 4px;
  }
  .new {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    padding: 3px 6px;
    border-radius: 4px;
    margin-top: 2px;
  }
  .trunc {
    display: block;
    margin-top: 4px;
    color: var(--muted);
    font-size: var(--text-sm);
  }
</style>
