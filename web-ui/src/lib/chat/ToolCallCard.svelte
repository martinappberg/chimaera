<script lang="ts">
  import { copyText } from "../shared/clipboard";
  import type { ChatBlock, ToolContent } from "./store.svelte";

  interface Props {
    block: Extract<ChatBlock, { kind: "tool" }>;
    /** Open a touched file in an adjacent pane (existing path-click flow). */
    onOpenFile?: (path: string) => void;
    /** Move this running tool to the background (claude background_tasks —
     *  the TUI's Ctrl-B). Provided only when the agent supports it. */
    onBackground?: () => void;
    /** Stop this running subagent (claude stop_task). */
    onStop?: () => void;
  }

  let { block, onOpenFile, onBackground, onStop }: Props = $props();

  const running = $derived(block.status === "in_progress" || block.status === "pending");

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
  /** A permission the user ALLOWED — a quiet inline "allowed" mark beside the
   *  command, never the denied/failed red. A genuinely failed or denied command
   *  wins (an allowed exec can still exit non-zero), so those stay distinct. */
  const allowed = $derived(block.allowed && block.status !== "failed" && !block.denied);
  const statusTitle = $derived(
    block.denied ? "denied" : allowed ? "allowed" : block.status.replace("_", " "),
  );

  // Live output follows its own tail (terminal-style) while streaming.
  let bodyEl = $state<HTMLElement | null>(null);
  $effect(() => {
    if (!block.streaming || block.content === null) return;
    void block.content.text;
    bodyEl?.scrollTo({ top: bodyEl.scrollHeight });
  });

  // Copy the plain-text output body (the same string the pre renders — a
  // truncated block copies what the UI has; the trunc note is visible).
  let copied = $state(false);
  let copiedTimer: ReturnType<typeof setTimeout> | null = null;
  function copyOutput() {
    const text = block.content?.kind === "output" ? (block.content.text ?? "") : "";
    if (text === "") return;
    void copyText(text).then((ok) => {
      if (!ok) return;
      copied = true;
      if (copiedTimer !== null) clearTimeout(copiedTimer);
      copiedTimer = setTimeout(() => {
        copiedTimer = null;
        copied = false;
      }, 1400);
    });
  }
  $effect(() => () => {
    if (copiedTimer !== null) clearTimeout(copiedTimer);
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
    {#if running && block.tool !== "agent" && onBackground !== undefined}
      <!-- Ctrl-B parity: the tool keeps running while the agent moves on;
           an honest "could not be backgrounded" notice covers refusals. -->
      <button class="act" title="continue in the background — the agent moves on" onclick={onBackground}>
        <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
          <path
            d="M8 3v6M5 6.5L8 9.5l3-3M4 12.5h8"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
      </button>
    {/if}
    {#if running && block.tool === "agent" && onStop !== undefined}
      <button class="act" title="stop this subagent" onclick={onStop}>
        <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
          <rect x="4.5" y="4.5" width="7" height="7" rx="1" fill="currentColor" />
        </svg>
      </button>
    {/if}
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
    {#if allowed}
      <!-- Compact "allowed" mark: the command stays on its one collapsed line
           (the title), an explicit allowed state instead of dumping it below. -->
      <span class="allowed" title="you allowed this command">
        <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
          <path
            d="M3.5 8.5l3 3 6-6.5"
            fill="none"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
        allowed
      </span>
    {/if}
    <span
      class="dot"
      class:run={block.status === "in_progress" || block.status === "pending"}
      class:ok={block.status === "completed" && !block.denied}
      class:bad={block.status === "failed" || block.denied}
    ></span>
  </div>
  {#if open && hasBody && block.content !== null}
    <!-- .body scrolls; the wrapper is the non-scrolling anchor the copy
         button pins to, so it never rides away with the content. -->
    <div class="body-wrap">
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
      {#if block.content.kind === "output"}
        <button
          class="copy"
          class:copied
          aria-label={copied ? "copied" : "copy output"}
          title={copied ? "copied" : "copy output"}
          onclick={copyOutput}
        >
          {#if copied}
            <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
              <path
                d="M3.5 8.5l3 3 6-6.5"
                fill="none"
                stroke="currentColor"
                stroke-width="1.8"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
          {:else}
            <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
              <rect
                x="6"
                y="6"
                width="7.5"
                height="7.5"
                rx="1.5"
                fill="none"
                stroke="currentColor"
                stroke-width="1.5"
              />
              <path
                d="M4 10h-.5A1.5 1.5 0 0 1 2 8.5v-5A1.5 1.5 0 0 1 3.5 2h5A1.5 1.5 0 0 1 10 3.5V4"
                fill="none"
                stroke="currentColor"
                stroke-width="1.5"
                stroke-linecap="round"
              />
            </svg>
          {/if}
        </button>
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
  .loc,
  .act {
    flex: none;
    display: inline-flex;
    background: none;
    border: none;
    color: var(--muted);
    padding: 2px;
    cursor: pointer;
    transition: color 0.12s ease;
  }
  .loc:hover,
  .act:hover {
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
  /* Quiet "allowed" affordance: an accent check, not the red of denied/failed
     — a permission-gated command the user let through reads calmly. */
  .allowed {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 3px;
    color: color-mix(in srgb, var(--accent) 85%, var(--fg));
    font-family: var(--mono, monospace);
    font-size: var(--text-xs);
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
  /* `pulse` (the live-dot / cursor opacity pulse) is the shared keyframe in
     app.css — reused by the subagents tray dot too. */
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
  .body-wrap {
    position: relative;
  }
  .body {
    border-top: 1px solid var(--edge);
    padding: 6px 10px;
    max-height: 260px;
    overflow: auto;
    scrollbar-width: thin;
    scrollbar-color: color-mix(in srgb, var(--fg) 22%, transparent) transparent;
  }
  /* Hover-reveal copy, pinned over the scrolling body (the .loc/.rewind-btn
     hover language; scrim keeps it legible over scrolled text). Collapsed
     rows never render it, so the quiet card stays quiet. */
  .copy {
    position: absolute;
    top: 6px;
    right: 6px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 4px;
    background: color-mix(in srgb, var(--term-bg) 82%, transparent);
    border: 1px solid var(--edge);
    border-radius: 5px;
    color: var(--muted);
    cursor: pointer;
    opacity: 0;
    transition:
      opacity 0.12s ease,
      color 0.12s ease;
  }
  .body-wrap:hover .copy,
  .copy:focus-visible,
  .copy.copied {
    opacity: 1;
  }
  .copy:hover,
  .copy.copied {
    color: var(--accent);
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
