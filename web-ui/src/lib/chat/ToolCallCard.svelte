<script lang="ts">
  import { copyText } from "../shared/clipboard";
  import { contextMenu } from "../shared/contextMenu.svelte";
  import { menuPoint, resolveAndOpen, type OpenPathFn, type PathResolver } from "./paths";
  import type { ChatBlock, SubagentInfo } from "./store.svelte";
  import DiffView from "./DiffView.svelte";
  import ModelChip from "./ModelChip.svelte";
  import { argRows } from "./toolArgs";
  import ToolGlyph from "./ToolGlyph.svelte";
  import { readableToolTitle, toolDescription, toolTitleParts } from "./toolLabels";

  interface Props {
    block: Extract<ChatBlock, { kind: "tool" }>;
    /** Open a touched path: a file in a pane, a directory in the Finder. */
    onOpenPath?: OpenPathFn;
    /** Resolves a relative location (a Grep/Glob `path`, a Codex change)
     *  against the session before it opens. */
    resolvePaths?: PathResolver;
    /** Move this running tool to the background (claude background_tasks —
     *  the TUI's Ctrl-B). Provided only when the agent supports it. */
    onBackground?: () => void;
    /** Stop this running subagent (claude stop_task). */
    onStop?: () => void;
    /** What the agent has said about this row's subagent (agent rows). */
    subagent?: SubagentInfo;
    /** A model id as the picker labels it. */
    modelName?: (id: string) => string;
    /** Open this subagent's own conversation; provided once it has one to
     *  open (the wire named it). */
    onOpenSubagent?: (newSplit: boolean) => void;
    /** False while the owning retained chat tab is hidden. */
    visible?: boolean;
  }

  let {
    block,
    onOpenPath,
    resolvePaths,
    onBackground,
    onStop,
    subagent,
    modelName,
    onOpenSubagent,
    visible = true,
  }: Props = $props();

  const running = $derived(block.status === "in_progress" || block.status === "pending");

  /** Rows start COLLAPSED — the title carries the what, the dot the state;
   *  output is one click away. Failures auto-expand (the error is the
   *  point), and a user toggle always wins afterwards. */
  let open = $state(false);
  let userToggled = false;
  $effect(() => {
    if (block.status === "failed" && !userToggled) open = true;
  });

  /** Agent-communication calls read as what they did ("Message to …");
   *  the driver's own title stays on hover. */
  const shownTitle = $derived(readableToolTitle(block));
  /** The title's two halves: the call's name in prose, its subject in mono. */
  const parts = $derived(toolTitleParts(block, shownTitle));
  /** The call's own plain-language description (claude Bash), when the
   *  wire gave one: it leads, the command follows in mono. */
  const described = $derived.by(() => {
    const d = toolDescription(block);
    return d !== null && d !== shownTitle ? d : null;
  });
  const hasContent = $derived(
    block.content !== null &&
      !(block.content.kind === "output" && (block.content.text ?? "").trim() === ""),
  );
  /** The whole command, for the opened row: the head keeps one clipped line. */
  const command = $derived(
    block.tool === "execute" ? (block.command ?? (block.title.trim() !== "" ? block.title : null)) : null,
  );
  /** The head already shows a short command whole; the opened row repeats
   *  it only when the head clipped it or led with the description. */
  const commandDetail = $derived(
    command !== null && (described !== null || command !== shownTitle || command.length > 72 || command.includes("\n")),
  );
  /** What the agent passed, for calls whose title is only a name. */
  const args = $derived(argRows(block.nativeInput, block.locations));
  /** Every call that has anything to say opens: its command, its arguments,
   *  its result. A line with none of those opens what it touched instead. */
  const hasBody = $derived(hasContent || command !== null || args.length > 0);
  const opensPath = $derived(!hasBody && block.locations.length > 0 && onOpenPath !== undefined);
  /** A permission the user ALLOWED — a quiet inline "allowed" mark beside the
   *  command, never the denied/failed red. A genuinely failed or denied command
   *  wins (an allowed exec can still exit non-zero), so those stay distinct. */
  const allowed = $derived(block.allowed && block.status !== "failed" && !block.denied);
  /** A running subagent's latest progress line ("reading foo.rs · 12 tools ·
   *  48k tokens"), surfaced on the COLLAPSED head so a minutes-long agent
   *  shows life without expanding. Hidden once open (the body has it). */
  const liveLine = $derived(
    block.tool === "agent" && running && !open && block.content?.kind === "output"
      ? (block.content.text ?? "").trim() || null
      : null,
  );
  /** The prompt's outcome when it was not a plain allow: the row is the
   *  transcript's only record of the decision, so it says which. */
  const refusal = $derived(
    block.decision === "denied" ? "denied" : block.decision === "lapsed" ? "not answered" : null,
  );
  /** A prompt that lapsed was never refused (the reducer leaves `denied`
   *  off); rows journaled before that distinction stay quiet too. */
  const denied = $derived(block.denied && block.decision !== "lapsed");
  const statusTitle = $derived(
    refusal ?? (block.denied ? "denied" : allowed ? "allowed" : block.status.replace("_", " ")),
  );

  // Live output follows its own tail (terminal-style) while streaming.
  let bodyEl = $state<HTMLElement | null>(null);
  $effect(() => {
    if (!visible || !block.streaming || block.content === null) return;
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

  /** A location as the menu names it: workspace-relative when it can be. */
  function locLabel(loc: string): string {
    return resolvePaths?.label(loc) ?? loc;
  }

  /** Open one location, resolved first (it may be relative, or a
   *  directory). Cmd/Ctrl: in a split. */
  function openLoc(loc: string, e: MouseEvent, anchor: Element) {
    if (onOpenPath === undefined || loc === "") return;
    void resolveAndOpen(resolvePaths, loc, onOpenPath, {
      split: e.metaKey || e.ctrlKey,
      at: menuPoint(e, anchor),
    });
  }

  /** The head's open button: the one location, or a compact list of all. */
  function onLocations(e: MouseEvent) {
    const btn = e.currentTarget as Element;
    const locs = block.locations;
    if (locs.length === 1) {
      openLoc(locs[0], e, btn);
      return;
    }
    const r = btn.getBoundingClientRect();
    contextMenu.openAtPoint(
      r.left,
      r.bottom,
      locs.map((loc) => ({ label: locLabel(loc), onSelect: () => openLoc(loc, e, btn) })),
    );
  }
  const locTitle = $derived(
    block.locations.length === 1
      ? `open ${locLabel(block.locations[0])}`
      : `open one of ${block.locations.length} paths`,
  );
</script>

<div
  class="tool"
  class:visible
  class:failed={block.status === "failed"}
  class:denied={denied}
>
  <div class="head">
    <button
      class="expand"
      class:inert={!hasBody && !opensPath}
      onclick={(e) => {
        if (hasBody) {
          userToggled = true;
          open = !open;
        } else if (opensPath) {
          openLoc(block.locations[0], e, e.currentTarget);
        }
      }}
      aria-expanded={hasBody ? open : undefined}
      title={statusTitle}
    >
      <span class="glyph"><ToolGlyph kind={block.tool} /></span>
      {#if described !== null && liveLine === null}
        <span class="title described">{described}</span>
        <span class="cmd" title={block.title}>{parts.detail}</span>
      {:else}
        <span
          class="title"
          class:with-live={liveLine !== null}
          title={parts.detail !== block.title ? block.title : undefined}
          >{#if parts.name !== null}<span class="name">{parts.name}</span>{/if}<span class:mono={parts.mono}
            >{parts.detail}</span
          ></span
        >
      {/if}
      {#if liveLine !== null}
        <span class="live">{liveLine}</span>
      {/if}
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
    {#if block.tool === "agent" && subagent?.model}
      <ModelChip model={subagent.model} {modelName} pill />
    {/if}
    {#if block.tool === "agent" && onOpenSubagent !== undefined}
      <button
        class="act"
        title="open this subagent's conversation"
        aria-label="open this subagent's conversation"
        onclick={(e) => onOpenSubagent?.(e.metaKey || e.ctrlKey)}
      >
        <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
          <path
            d="M6.5 3.5H4a.5.5 0 0 0-.5.5v8a.5.5 0 0 0 .5.5h8a.5.5 0 0 0 .5-.5V9.5M9.5 3.5h3v3M12.5 3.5L7.5 8.5"
            fill="none"
            stroke="currentColor"
            stroke-width="1.4"
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
    {#if block.locations.length > 0 && onOpenPath !== undefined}
      <!-- The workbench is right there: every located tool opens what it
           touched (image/markdown/csv/pdf land in their native previews, a
           directory in the Finder); several locations open from a list. -->
      <button
        class="loc"
        title={locTitle}
        aria-haspopup={block.locations.length > 1 ? "menu" : undefined}
        onclick={onLocations}
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
        {#if block.locations.length > 1}
          <span class="loc-count">{block.locations.length}</span>
        {/if}
      </button>
    {/if}
    {#if allowed}
      <!-- Compact "allowed" mark: the command stays on its one collapsed line
           (the title), an explicit allowed state instead of dumping it below. -->
      <span class="allowed" title={block.decisionLabel ?? "you allowed this"}>
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
        {block.decision === "always" ? "always allowed" : "allowed"}
      </span>
    {:else if refusal !== null}
      <span class="refused" class:lapsed={block.decision === "lapsed"} title={block.decisionLabel}>{refusal}</span>
    {/if}
    <!-- One slot, one width: a call that ran and was fine says nothing here
         (it is the expected outcome, and a column of identical marks is
         noise); a live or failed one shows its dot; the rest show, under
         the pointer, that the row opens. -->
    <span class="state">
      {#if running}
        <span class="dot run"></span>
      {:else if block.status === "failed" || denied}
        <span class="dot bad"></span>
      {:else if hasBody}
        <svg class="chev" class:open viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
          <path d="M5 3l6 5-6 5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
        </svg>
      {/if}
    </span>
  </div>
  {#if open && hasBody}
    <!-- .body scrolls; the wrapper is the non-scrolling anchor the copy
         button pins to, so it never rides away with the content. -->
    <div class="body-wrap">
      <div class="body quiet-scrollbar" bind:this={bodyEl}>
        {#if commandDetail}
          <pre class="command"><span class="prompt" aria-hidden="true">❯ </span>{command}</pre>
        {/if}
        {#if args.length > 0}
          <dl class="args">
            {#each args as arg (arg.key)}
              <dt>{arg.key}</dt>
              <dd>{arg.value}</dd>
            {/each}
          </dl>
        {/if}
        {#if block.content === null || !hasContent}
          {#if command !== null && !running}
            <span class="none">no output</span>
          {/if}
        {:else if block.content.kind === "output"}
          <pre>{block.content.text}</pre>
          {#if block.streaming}
            <span class="cursor" aria-hidden="true"></span>
          {/if}
          {#if block.content.truncated}
            <span class="trunc">output truncated</span>
          {/if}
        {:else if block.content.kind === "diff"}
          <DiffView diff={block.content} label={locLabel} onOpen={openLoc} />
        {:else if block.content.kind === "batch"}
          {@const diffs = block.content.diffs ?? []}
          {#each diffs as d, i (i)}
            <DiffView diff={d} showPath={i === 0 || diffs[i - 1].path !== d.path} label={locLabel} onOpen={openLoc} />
          {/each}
        {/if}
      </div>
      {#if hasContent && block.content?.kind === "output"}
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
    /* The opened body answers to the row's own width. */
    container-type: inline-size;
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
  .tool:not(.visible) .cursor,
  .tool:not(.visible) .dot.run {
    animation: none;
    opacity: 0.55;
  }
  /* Hidden document: pause the blink/pulse for the visible tab too (the
     html.app-hidden contract; see app.css). */
  :global(html.app-hidden) .cursor,
  :global(html.app-hidden) .dot.run {
    animation-play-state: paused;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 0 10px 0 0;
    color: var(--fg);
    font-size: var(--text-sm);
    transition: background-color 0.12s ease;
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
  }
  /* fg color-mix, not --row-hover: the card sits on --term-bg, where the
     solid row tokens tuned for --bg surfaces can clash. The whole head
     tints, so the row's trailing marks sit inside its hover, not past it. */
  .head:has(> .expand:not(.inert)):hover {
    background: var(--hover-tint);
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
  .loc {
    align-items: center;
    gap: 2px;
  }
  .loc-count {
    font-family: var(--mono, monospace);
    font-size: var(--text-xs);
    line-height: 1;
  }
  .glyph {
    flex: none;
    display: inline-flex;
    color: var(--muted);
  }
  .failed .glyph,
  .denied .glyph {
    color: color-mix(in srgb, var(--err) 75%, var(--muted));
  }
  .title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* "Read  calc.py": the kind of call steps back, its subject carries. */
  .name {
    margin-right: 7px;
    color: var(--muted);
  }
  .mono {
    font-family: var(--mono, monospace);
  }
  /* A described call: the agent's words lead in the prose face, the
     command trails, quieter, in mono — clipped first when space runs out. */
  .title.described {
    flex: 0 1 auto;
    max-width: 65%;
  }
  .cmd {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono, monospace);
    color: var(--muted);
  }
  /* When a subagent's live progress rides the head, the title yields room —
     same split as the AgentsTray rows. */
  .title.with-live {
    flex: none;
    max-width: 55%;
  }
  .live {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  /* Quiet "allowed" affordance: an accent check, not the red of denied/failed
     — a permission-gated command the user let through reads calmly. */
  .allowed {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 3px;
    color: var(--accent-text);
    font-family: var(--mono, monospace);
    font-size: var(--text-xs);
  }
  .refused {
    flex: none;
    color: var(--err);
    font-family: var(--mono, monospace);
    font-size: var(--text-xs);
  }
  .refused.lapsed {
    color: var(--muted);
  }
  .state {
    flex: none;
    display: inline-grid;
    place-items: center;
    width: 12px;
    height: 12px;
  }
  .chev {
    color: var(--muted);
    opacity: 0;
    transition:
      opacity 0.12s ease,
      transform 0.12s ease;
  }
  .chev.open {
    opacity: 1;
    transform: rotate(90deg);
  }
  .head:hover .chev,
  .expand:focus-visible ~ .state .chev {
    opacity: 1;
  }
  /* No pointer to hover with: the row always shows that it opens. */
  @media (hover: none) {
    .chev {
      opacity: 1;
    }
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
    .chev {
      transition: none;
    }
  }
  .failed,
  .denied {
    box-shadow: inset 2px 0 0 color-mix(in srgb, var(--err) 60%, transparent);
  }
  /* The opened row: one framed panel under the row's words (past its
     mark), holding what was asked and what came back. */
  .body-wrap {
    position: relative;
    margin: 1px 10px 8px 32px;
  }
  @container (max-width: 420px) {
    .body-wrap {
      margin-left: 10px;
    }
  }
  .body {
    border: 1px solid color-mix(in srgb, var(--edge) 80%, transparent);
    border-radius: 7px;
    background: color-mix(in srgb, var(--fg) 2.5%, transparent);
    padding: 8px 10px;
    max-height: 280px;
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
  /* The opened row leads with what was asked — the command as it ran, the
     arguments — then the result, set apart by a hairline. */
  .command,
  .args {
    margin: 0 0 8px;
    padding-bottom: 8px;
    border-bottom: 1px solid color-mix(in srgb, var(--edge) 60%, transparent);
  }
  .command:last-child,
  .args:last-child {
    margin-bottom: 0;
    padding-bottom: 0;
    border-bottom: none;
  }
  .prompt {
    color: var(--accent-text, var(--accent));
    user-select: none;
  }
  .args {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: 2px 12px;
    font-size: var(--text-sm);
    font-family: var(--mono, monospace);
  }
  .args dt {
    color: var(--muted);
  }
  .args dd {
    margin: 0;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .none {
    color: var(--muted);
    font-size: var(--text-sm);
  }
  pre {
    margin: 0;
    font-size: var(--text-sm);
    line-height: 1.5;
    font-family: var(--mono, monospace);
    white-space: pre-wrap;
    word-break: break-word;
  }
  .trunc {
    display: block;
    margin-top: 4px;
    color: var(--muted);
    font-size: var(--text-sm);
  }
</style>
