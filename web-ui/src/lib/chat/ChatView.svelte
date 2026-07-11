<script lang="ts">
  import { onDestroy, tick } from "svelte";
  import { rewindSession, renameSession, type Session } from "../workspace/sessions";
  import { fsValidate } from "../previews/files";
  import { listAgents } from "../workspace/launcher";
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import { insertIntoComposer } from "./composerBus";
  import { ChatSocket } from "./chatWs";
  import { ChatStore } from "./store.svelte";
  import { dismiss } from "../shared/dismiss";
  import ChatHeader from "./ChatHeader.svelte";
  import Markdown from "./Markdown.svelte";
  import UserText from "./UserText.svelte";
  import ToolGroup from "./ToolGroup.svelte";
  import ArtifactGallery from "./ArtifactGallery.svelte";
  import PermissionCard from "./PermissionCard.svelte";
  import QuestionCard from "./QuestionCard.svelte";
  import UsagePanel from "./UsagePanel.svelte";
  import McpPanel from "./McpPanel.svelte";
  import RewindDialog from "./RewindDialog.svelte";
  import Composer from "./Composer.svelte";
  import type { ImageAttachment } from "./images";
  import type { ChatBlock } from "./store.svelte";

  interface Props {
    session: Session;
    focused: boolean;
    /** Workspace terminals for @term: mention grants. */
    terminals?: { id: string; name: string }[];
    /** Open a file path in an adjacent pane (the workbench path-click flow). */
    onOpenFile?: (path: string) => void;
    /** Kind-aware open: files → viewer pane, dirs → the Finder. */
    onOpenPath?: (path: string, kind: "file" | "dir") => void;
  }

  let { session, focused, terminals = [], onOpenFile, onOpenPath }: Props = $props();

  const store = new ChatStore();
  // The component is {#key}ed on session id by its parent: one instance, one
  // session, one socket — the initial capture is the contract.
  // svelte-ignore state_referenced_locally
  const socket = new ChatSocket(session.id, {
    onReady: (info, replayFrom, head) => store.onReady(info, replayFrom, head),
    onEvent: (entry) => store.apply(entry),
    onDegraded: () => (store.degraded = true),
    onExited: (status) => (store.exited = { status }),
    onError: (message) => (store.fatalError = message),
    onDisconnected: () => store.onDisconnected(),
    lastSeq: () => store.lastSeq,
  });
  onDestroy(() => socket.close());

  // Curated model choices for this agent's picker (daemon-cached catalog).
  let models = $state<{ id: string; label: string }[]>([]);
  // svelte-ignore state_referenced_locally
  const agentKind = session.agent_kind ?? "claude";
  /** Product name for the identity chip. Prefer the daemon catalog's own name;
   *  fall back to a built-in map until it resolves (a workspace can mix agents,
   *  so the surface always says WHICH one this is). */
  let agentCatalogName = $state<string | null>(null);
  const agentName = $derived(
    agentCatalogName ??
      (agentKind === "claude" ? "Claude Code" : agentKind === "codex" ? "Codex" : agentKind),
  );
  void listAgents().then((agents) => {
    const info = agents.find((a) => a.id === agentKind);
    models = info?.models ?? [];
    agentCatalogName = info?.name ?? null;
  });

  let transcriptEl = $state<HTMLElement | null>(null);
  let atBottom = $state(true);
  let menu = $state<"model" | "mode" | "effort" | "mcp" | null>(null);

  /** Model picker: the agent's own catalog (claude initialize.models /
   *  codex model/list) beats the daemon's curated list. */
  const modelChoices = $derived(store.models.length > 0 ? store.models : models);
  /** The catalog row for the live model. Ids come in three spellings:
   *  picker values ("opus[1m]"), catalog resolvedModel
   *  ("claude-opus-4-8[1m]"), and the BARE api id assistant messages report
   *  ("claude-opus-4-8") — match all three, preferring named entries over
   *  "Default (recommended)" (both resolve to the same model). Before the
   *  first turn the "default" entry IS the truth. */
  const currentModel = $derived.by(() => {
    const target = store.model;
    if (target === null) {
      return store.models.find((m) => m.id === "default") ?? store.models[0];
    }
    const exact = store.models.find((m) => m.id === target || m.resolved === target);
    if (exact !== undefined) return exact;
    const norm = (s: string) => s.replace(/\[[^\]]*\]$/, "");
    const targetN = norm(target);
    const named = store.models.find(
      (m) => m.id !== "default" && m.resolved !== null && norm(m.resolved) === targetN,
    );
    return (
      named ??
      store.models.find((m) => m.resolved !== null && norm(m.resolved) === targetN) ??
      store.models.find((m) => norm(m.id) === targetN)
    );
  });
  /** Reasoning-effort choices: per-model when the agent reports them;
   *  codex falls back to its known ladder, claude to none (no effort knob
   *  on that model — e.g. haiku). */
  const FALLBACK_EFFORTS = ["minimal", "low", "medium", "high", "xhigh"];
  const effortChoices = $derived.by(() => {
    if (currentModel !== undefined) return currentModel.efforts;
    return agentKind === "codex" ? FALLBACK_EFFORTS : [];
  });
  /** codex holds the pick client-side (rides the next turn); claude's truth
   *  arrives via effort_state read-backs. */
  let effort = $state<string | null>(null);
  const effortShown = $derived(store.effort ?? effort);
  const hasEffort = $derived(effortChoices.length > 0);
  /** Ultracode: session-scoped xhigh + standing workflow orchestration —
   *  offered when the live model supports xhigh (the extension's gate). */
  const hasUltracode = $derived(
    agentKind === "claude" && (currentModel?.efforts.includes("xhigh") ?? false),
  );

  function onScroll() {
    const el = transcriptEl;
    if (el === null) return;
    atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  }

  function scrollToBottom() {
    transcriptEl?.scrollTo({ top: transcriptEl.scrollHeight });
  }

  // Stick to the bottom while new content streams, unless the user scrolled
  // up to read history. Guarded on atBottom so a background stream never forces
  // layout. lastSeq bumps on every applied event (in-place chunk appends and
  // tool patches change no collection lengths), so streaming keeps following;
  // Markdown's onReveal keeps us pinned as words grow between wire chunks.
  $effect(() => {
    void store.blocks.length;
    void store.pending.length;
    void store.lastSeq;
    if (!atBottom) return;
    void tick().then(scrollToBottom);
  });

  function sendNow(text: string, images: ImageAttachment[]): boolean {
    const blocks: Record<string, unknown>[] = [];
    if (text.length > 0) blocks.push({ type: "text", text });
    for (const img of images) {
      blocks.push({ type: "image", media_type: img.media_type, data: img.data });
    }
    return socket.send({ type: "send", blocks });
  }

  function onSubmit(text: string, images: ImageAttachment[]): boolean {
    // Type-through, official-client semantics: mid-turn sends go straight to
    // the agent — claude's CLI queues them natively, codex steers them into
    // the running turn. No client-side queue to manage or lose. Returns false
    // when the socket isn't open so the composer keeps the draft.
    return sendNow(text, images);
  }

  function decide(requestId: string, optionId: string, destination?: string) {
    socket.send({
      type: "permission",
      request_id: requestId,
      option_id: optionId,
      ...(destination !== undefined ? { destination } : {}),
    });
  }

  function interrupt() {
    socket.send({ type: "interrupt" });
  }

  /** Dialog-only slash commands get native UI here instead of the CLI's
   *  "isn't available in this environment" dead end. Arguments resolve
   *  directly ("/effort high", "/model opus"); bare commands open pickers. */
  function onSlash(name: string, args = ""): boolean {
    const arg = args.trim().toLowerCase();
    switch (name) {
      case "rename": {
        // The agent CLIs can't rename their own thread from here (claude
        // punts, codex has no such command) — but chimaera owns the session
        // name. Pin it, so the tab, rail, and recents all follow. Case is
        // preserved (not the lowercased `arg`).
        const next = args.trim();
        if (next.length === 0) {
          store.notice("usage: /rename <new name>", "info");
          return true;
        }
        void renameSession(session.id, next)
          .then(() => store.notice(`renamed to “${next}”`, "info"))
          .catch((e: unknown) => store.notice(`rename failed: ${String(e)}`, "error"));
        return true;
      }
      case "model": {
        const hit = modelChoices.find(
          (m) => m.id.toLowerCase() === arg || m.label.toLowerCase() === arg,
        );
        if (arg.length > 0 && hit !== undefined) {
          pickModel(hit.id);
        } else {
          menu = "model";
        }
        return true;
      }
      case "mode": {
        const hit = store.modes.find(
          (m) => m.id.toLowerCase() === arg || m.label.toLowerCase() === arg,
        );
        if (arg.length > 0 && hit !== undefined) {
          pickMode(hit.id);
        } else if (store.modes.length > 0) {
          menu = "mode";
        } else {
          return false;
        }
        return true;
      }
      case "usage":
      case "cost":
        // Answered by a usage_report event (plan-limit windows — the honest
        // signal on subscription plans; dollars are not shown). Codex reads
        // the same data from account/read.
        socket.send({ type: "get_usage" });
        return true;
      case "mcp":
        if (agentKind === "claude") {
          store.mcpServers = null;
          socket.send({ type: "get_mcp" });
          menu = "mcp";
          return true;
        }
        return false;
      case "effort":
        if (!hasEffort) return false;
        if (arg.length > 0 && effortChoices.includes(arg)) {
          pickEffort(arg);
        } else {
          menu = "effort";
        }
        return true;
      case "ultracode":
        if (!hasUltracode) return false;
        if (arg === "on" || arg === "off") {
          socket.send({ type: "set_ultracode", enabled: arg === "on" });
        } else {
          toggleUltracode();
        }
        return true;
      default:
        return false;
    }
  }

  function toggleUltracode() {
    socket.send({ type: "set_ultracode", enabled: !store.ultracode });
  }

  /** Prose path candidates validate against the daemon relative to the
   *  session cwd (the terminal-link mechanism) — only real paths become
   *  clickable, and dirs route to the Finder. The workspace id enables the
   *  daemon's unique-basename fallback, keeping chat links and terminal
   *  links in parity for a bare "FIGURE_PLAN.md" living in a subdirectory. */
  async function resolveProsePaths(
    candidates: string[],
  ): Promise<Map<string, { path: string; kind: "file" | "dir" }>> {
    const out = new Map<string, { path: string; kind: "file" | "dir" }>();
    try {
      const valid = await fsValidate(candidates, session.cwd, session.workspace_id ?? null);
      for (const [cand, hit] of Object.entries(valid)) {
        out.set(cand, { path: hit.path, kind: hit.kind });
      }
    } catch {
      // Unreachable daemon: nothing becomes clickable this round.
    }
    return out;
  }

  function openProsePath(path: string, kind: "file" | "dir") {
    if (onOpenPath !== undefined) onOpenPath(path, kind);
    else if (kind === "file") onOpenFile?.(path);
  }

  /** The composer's palette: chimaera-native pickers first (they don't
   *  exist in the CLI's -p catalog), then the CLI's own commands. */
  const composerCommands = $derived.by(() => {
    const native: { name: string; description: string }[] = [];
    native.push({ name: "rename", description: "rename this session — chimaera" });
    native.push({ name: "model", description: `switch model — chimaera picker` });
    if (store.modes.length > 0) {
      native.push({ name: "mode", description: "permission mode — chimaera picker" });
    }
    if (hasEffort) {
      native.push({
        name: "effort",
        description: `reasoning effort (${effortChoices.join("/")}) — chimaera picker`,
      });
    }
    if (hasUltracode) {
      native.push({ name: "ultracode", description: "toggle ultracode (on/off) — session only" });
    }
    native.push({ name: "usage", description: "plan usage limits — chimaera panel" });
    if (agentKind === "claude") {
      native.push({ name: "mcp", description: "MCP servers — chimaera panel" });
    }
    const nativeNames = new Set(native.map((n) => n.name));
    return [...native, ...store.slashCommands.filter((c) => !nativeNames.has(c.name))];
  });

  // --- checkpoint rewind (claude) -------------------------------------------
  // Click "rewind" on a user message → dry-run report → confirm dialog →
  // restore files (rewind_files) → optionally fork the conversation there.
  // The intent flag keeps replayed RewindResult events from reopening UI.
  let rewindIntent = $state<null | {
    id: string;
    preceding: string | null;
    fork: boolean;
    stage: "dry" | "applying";
  }>(null);
  const rewindReport = $derived(
    rewindIntent !== null && store.rewind?.userMessageId === rewindIntent.id
      ? store.rewind
      : null,
  );

  function askRewind(checkpoint: { id: string; preceding: string | null }) {
    store.rewind = null;
    rewindIntent = { id: checkpoint.id, preceding: checkpoint.preceding, fork: false, stage: "dry" };
    socket.send({ type: "rewind", user_message_id: checkpoint.id, dry_run: true });
  }

  function confirmRewind(fork: boolean) {
    if (rewindIntent === null) return;
    rewindIntent = { ...rewindIntent, fork, stage: "applying" };
    store.rewind = null;
    socket.send({ type: "rewind", user_message_id: rewindIntent.id, dry_run: false });
  }

  // The apply answer arrived: finish (and fork the conversation if asked).
  $effect(() => {
    const intent = rewindIntent;
    const report = rewindReport;
    if (intent === null || report === null || intent.stage !== "applying") return;
    rewindIntent = null;
    if (!report.applied) {
      store.notice(report.error ?? "rewind failed", "error");
      return;
    }
    if (intent.fork && intent.preceding !== null) {
      void rewindSession(session.id, intent.preceding).catch((e: unknown) => {
        store.notice(`fork failed: ${String(e)}`, "error");
      });
    } else {
      store.notice("files restored to checkpoint", "info");
    }
  });

  function pickModel(id: string) {
    menu = null;
    socket.send({ type: "set_model", model_id: id });
  }

  function pickMode(id: string) {
    menu = null;
    socket.send({ type: "set_mode", mode_id: id });
  }

  function pickEffort(id: string) {
    menu = null;
    effort = id;
    socket.send({ type: "set_effort", effort_id: id });
  }

  const EFFORT_HINT: Record<string, string> = {
    claude: "reasoning effort — applies immediately, this session only",
    codex: "reasoning effort — applies from the next message",
  };

  /** Extended-thinking toggle (claude). Client-held — the CLI has no read-back,
   *  so we track it locally and start from claude's unset default (off); the
   *  chip shows an explicit on/off and tints when on, so its state is legible. */
  let thinking = $state(false);
  const hasThinking = $derived(agentKind === "claude");
  function toggleThinking() {
    thinking = !thinking;
    socket.send({ type: "set_thinking", enabled: thinking });
  }

  const modeLabel = $derived(
    store.modes.find((m) => m.id === store.currentMode)?.label ?? store.currentMode,
  );
  /** Model chip: the catalog's own display name when known ("Opus",
   *  "Fable"), else a readable fallback from the raw id. */
  const modelLabel = $derived.by(() => {
    if (currentModel !== undefined) return currentModel.label;
    const m = store.model;
    if (m === null) return null;
    const match = /claude-(\w+)-(\d+)-(\d+)/.exec(m);
    return match !== null ? `${match[1]} ${match[2]}.${match[3]}` : m;
  });

  /** Live status line under the transcript: what the agent is doing NOW.
   *  Phases: starting → thinking / writing / {tool title} → working
   *  (between tools) → gone. */
  const activityLabel = $derived.by(() => {
    const a = store.activity;
    if (a === null) return "working";
    switch (a.kind) {
      case "thinking":
        return a.detail.length > 0 ? `thinking · ${a.detail}` : "thinking";
      case "writing":
        return "writing";
      case "tool":
        return a.detail.length > 64 ? `${a.detail.slice(0, 64)}…` : a.detail;
      default:
        return a.detail === "starting" ? "starting" : "working";
    }
  });

  const planDone = $derived(store.plan.filter((p) => p.status === "done").length);

  /** Render list: consecutive tool blocks coalesce into one ToolGroup so a
   *  long run reads as a single condensed line, not a wall of cards. Every
   *  other block passes through as a "single" carrying its ORIGINAL index (the
   *  streaming reveal keys off store.blocks positions). */
  type RenderItem =
    | { t: "group"; key: string; tools: Extract<ChatBlock, { kind: "tool" }>[] }
    | { t: "single"; key: string; index: number; block: ChatBlock };
  const renderItems = $derived.by(() => {
    const items: RenderItem[] = [];
    let group: Extract<RenderItem, { t: "group" }> | null = null;
    store.blocks.forEach((block, i) => {
      if (block.kind === "tool") {
        if (group === null) {
          group = { t: "group", key: `g-${block.id}`, tools: [] };
          items.push(group);
        }
        group.tools.push(block);
      } else {
        group = null;
        items.push({ t: "single", key: `b-${i}`, index: i, block });
      }
    });
    return items;
  });
</script>

<!-- The outside-dismiss action closes any open header menu / the /mcp panel on
     an outside pointerdown or Escape; `.menu-host` marks the surfaces that must
     stay open (the chips live in ChatHeader, the panel is a sibling overlay). -->
<div
  class="chat"
  class:focused
  use:dismiss={{
    enabled: menu !== null,
    onDismiss: () => (menu = null),
    keepOpenWithin: ".menu-host",
  }}
>
  <ChatHeader
    {store}
    {agentKind}
    {agentName}
    bind:menu
    {modelChoices}
    {modelLabel}
    {modeLabel}
    {hasEffort}
    {effortChoices}
    {effortShown}
    effortHint={EFFORT_HINT[agentKind] ?? "reasoning effort"}
    {hasUltracode}
    {hasThinking}
    {thinking}
    onPickModel={pickModel}
    onPickMode={pickMode}
    onPickEffort={pickEffort}
    onToggleUltracode={toggleUltracode}
    onToggleThinking={toggleThinking}
    onInterrupt={interrupt}
  />

  <!-- Focusable so keyboard scrolling works in WKWebView (Safari never
       auto-focuses scrollers); role="log" announces new agent output. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div
    class="transcript"
    role="log"
    aria-label="conversation"
    tabindex="0"
    bind:this={transcriptEl}
    onscroll={onScroll}
  >
    <!-- One real reading column (the Claude Desktop measure): agent prose
         fills it from the left, user bubbles right-align inside it. -->
    <div class="column">
    {#if store.blocks.length === 0 && store.exited === null}
      <div class="empty">
        <SessionGlyph kind="agent" {agentKind} size={18} />
        <span>{store.connected ? `${agentName} is ready` : `connecting to ${agentName}…`}</span>
      </div>
    {/if}
    {#each renderItems as item (item.key)}
      {#if item.t === "group"}
        <ToolGroup tools={item.tools} {onOpenFile} />
      {:else if item.block.kind === "user"}
        {@const block = item.block}
        <div class="msg user">
          <div class="bubble-row">
            {#if agentKind === "claude" && block.checkpoint !== null}
              <button
                class="rewind-btn"
                title="rewind to before this message (restores files; optionally forks the conversation)"
                onclick={() => askRewind(block.checkpoint!)}
              >
                ↺
              </button>
            {/if}
            <div class="bubble">
              <UserText
                text={block.text}
                onOpenPath={openProsePath}
                resolvePaths={resolveProsePaths}
              />
            </div>
          </div>
          {#if block.attachments > 0}
            <span class="attach">{block.attachments} image{block.attachments > 1 ? "s" : ""}</span>
          {/if}
        </div>
      {:else if item.block.kind === "message"}
        <div class="msg agent">
          <Markdown
            text={item.block.text}
            streaming={store.running && item.index === store.blocks.length - 1}
            onOpenPath={openProsePath}
            resolvePaths={resolveProsePaths}
            onReveal={() => {
              if (atBottom) scrollToBottom();
            }}
          />
        </div>
      {:else if item.block.kind === "thought"}
        <details class="thought">
          <summary>thinking · {item.block.text.length} chars</summary>
          <div class="thought-body">{item.block.text}</div>
        </details>
      {:else if item.block.kind === "notice"}
        <div class="notice" class:error={item.block.tone === "error"}>{item.block.text}</div>
      {:else if item.block.kind === "turn_end"}
        {@const block = item.block}
        <!-- The turn's artifacts preview here, after the closing prose. -->
        {#if block.artifacts.length > 0}
          <ArtifactGallery paths={block.artifacts} onOpen={onOpenFile} />
        {/if}
        <!-- Instant turns (retractions, empty results) get no strip: a
             "0.0s" ruler is noise, not information. -->
        {#if block.durationMs >= 100}
          <div class="turn-end">
            <span>{(block.durationMs / 1000).toFixed(1)}s</span>
          </div>
        {/if}
      {:else if item.block.kind === "usage"}
        <UsagePanel windows={item.block.windows} />
      {/if}
    {/each}

    {#each store.pending as request (request.requestId)}
      <PermissionCard
        {request}
        onDecide={(opt, dest) => decide(request.requestId, opt, dest)}
      />
    {/each}

    {#each store.questions as request (request.requestId)}
      <QuestionCard
        {request}
        onAnswer={(answers) =>
          socket.send({ type: "answer", request_id: request.requestId, answers })}
      />
    {/each}

    {#if store.running && store.pending.length === 0 && store.questions.length === 0}
      <div class="status-row" aria-live="polite">
        <span class="status-spark">
          <SessionGlyph kind="agent" {agentKind} size={12} state="alive" />
        </span>
        <span class="status-label">{activityLabel}</span>
      </div>
    {/if}

    {#if store.fatalError !== null}
      <div class="notice error">{store.fatalError}</div>
    {/if}
    {#if store.degraded}
      <div class="notice">continued in terminal — this pane will switch</div>
    {:else if store.exited !== null}
      <div class="notice">
        agent exited{store.exited.status !== null ? ` (status ${store.exited.status})` : ""}
      </div>
    {/if}

    {#if !atBottom && store.pending.length > 0}
      <button class="jump" onclick={scrollToBottom}>
        permission needed ↓
      </button>
    {/if}
    </div>
  </div>

  {#if store.plan.length > 0}
    <details class="plan">
      <summary>plan · {planDone}/{store.plan.length}</summary>
      {#each store.plan as entry, i (i)}
        <div class="plan-row" class:done={entry.status === "done"}>
          <span class="plan-mark">
            {entry.status === "done" ? "✓" : entry.status === "in_progress" ? "◐" : "○"}
          </span>
          <span>{entry.content}</span>
        </div>
      {/each}
    </details>
  {/if}

  {#if rewindIntent !== null}
    <RewindDialog
      intent={rewindIntent}
      report={rewindReport}
      onCancel={() => (rewindIntent = null)}
      onConfirm={confirmRewind}
      {onOpenFile}
    />
  {/if}

  {#if menu === "mcp"}
    <McpPanel
      servers={store.mcpServers}
      onReconnect={(server) => socket.send({ type: "reconnect_mcp", server })}
      onToggleEnabled={(server, enabled) =>
        socket.send({ type: "set_mcp_enabled", server, enabled })}
    />
  {/if}

  {#if store.promptSuggestion !== null && !store.running}
    <div class="suggestion-row">
      <button
        class="suggestion"
        title="suggested next prompt — click to use"
        onclick={() => {
          const text = store.promptSuggestion;
          store.promptSuggestion = null;
          if (text !== null) insertIntoComposer(session.id, text);
        }}
      >
        <span class="suggestion-mark">↳</span>
        <span class="suggestion-text">{store.promptSuggestion}</span>
      </button>
      <button
        class="suggestion-x"
        aria-label="dismiss suggestion"
        onclick={() => (store.promptSuggestion = null)}>×</button
      >
    </div>
  {/if}

  <Composer
    sessionId={session.id}
    running={store.running}
    disabled={store.exited !== null || store.degraded}
    slashCommands={composerCommands}
    workspaceId={session.workspace_id ?? null}
    {terminals}
    {focused}
    {onSubmit}
    onInterrupt={interrupt}
    {onSlash}
  />
</div>

<style>
  .chat {
    position: relative; /* anchors the rewind dialog + /mcp panel overlays */
    height: 100%;
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--term-bg);
    color: var(--fg);
    /* The reading measure (the Claude Desktop proportion) shared by the
       transcript column, the composer, and their satellites. */
    --chat-measure: 52rem;
  }
  .transcript {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    scrollbar-width: thin;
    scrollbar-color: color-mix(in srgb, var(--fg) 22%, transparent) transparent;
    padding: 14px 18px;
    display: flex;
    flex-direction: column;
  }
  /* One real column element, not per-child margin tricks (a block's own
     margin shorthand silently defeated those). It GROWS with content
     (flex-basis auto, no shrink), so children with overflow!=visible
     (tool cards, zero automatic min-size) are never squeezed by an
     overflowing transcript — and it fills the viewport when short, so
     .empty can center in it. */
  .column {
    flex: 1 0 auto;
    display: flex;
    flex-direction: column;
    gap: 3px;
    width: 100%;
    max-width: var(--chat-measure);
    margin: 0 auto;
  }
  /* Blocks size to content and never absorb shrink: a tool card
     (overflow:hidden → zero automatic min-size) would otherwise collapse to
     its borders in a tall transcript. Agent prose and cards stretch to the
     column width (default align); user bubbles opt out via align-self. */
  .column > :global(*) {
    flex: none;
  }
  /* The composer and its satellites share the column; their 18px side
     padding rides OUTSIDE the measure so text edges line up with it. */
  .chat > :global(.composer),
  .chat > .suggestion-row,
  .chat > .plan {
    width: 100%;
    max-width: calc(var(--chat-measure) + 36px);
    margin-left: auto;
    margin-right: auto;
    box-sizing: border-box;
    padding-left: 18px;
    padding-right: 18px;
  }
  /* No full-width rule under a centered column — the input's own border
     is the boundary (the Claude Desktop treatment). */
  .chat > :global(.composer) {
    border-top: none;
  }
  .empty {
    margin: auto;
    color: var(--muted);
    font-size: var(--text-sm);
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
  }
  .msg {
    word-break: break-word;
    line-height: 1.5;
    font-size: var(--text-md);
    animation: rise 0.15s ease; /* @keyframes rise lives in app.css */
  }
  @media (prefers-reduced-motion: reduce) {
    .msg {
      animation: none;
    }
  }
  /* User messages: quiet bubbles RIGHT-ALIGNED inside the column, agent
     prose plain from the left — the Claude Desktop shape. Longhand
     margins only: a shorthand here once zeroed the column's centering. */
  .msg.user {
    align-self: flex-end;
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    max-width: min(85%, 40rem);
    margin-top: 14px;
    margin-bottom: 6px;
  }
  .msg.user .bubble {
    background: color-mix(in srgb, var(--fg) 6%, transparent);
    border-radius: 14px;
    padding: 8px 14px;
    max-width: 100%;
  }
  .attach {
    color: var(--muted);
    font-size: var(--text-sm);
    margin-top: 2px;
  }
  .msg.agent {
    padding: 2px 0;
  }
  .thought {
    margin: 2px 0;
  }
  .thought summary {
    color: var(--muted);
    font-size: var(--text-sm);
    cursor: pointer;
    user-select: none;
    list-style-position: inside;
  }
  .thought-body {
    color: var(--muted);
    font-size: var(--text-sm);
    white-space: pre-wrap;
    word-break: break-word;
    border-left: 2px solid var(--edge);
    padding: 4px 0 4px 10px;
    margin: 4px 0 4px 4px;
  }
  .notice {
    color: var(--muted);
    font-size: var(--text-sm);
    text-align: center;
    padding: 6px 0;
  }
  .notice.error {
    color: var(--err);
  }
  .turn-end {
    display: flex;
    justify-content: flex-end;
    gap: 6px;
    color: color-mix(in srgb, var(--muted) 70%, transparent);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    padding: 2px 0 10px;
    border-bottom: 1px solid color-mix(in srgb, var(--edge) 30%, transparent);
    margin-bottom: 10px;
  }
  .status-row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 0 2px;
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .status-spark {
    display: inline-flex;
    animation: spark-pulse 1.6s ease-in-out infinite;
  }
  .status-label {
    font-family: var(--mono, monospace);
    animation: label-pulse 1.6s ease-in-out infinite;
  }
  /* Ellipsis that breathes with the spark, without layout shift. */
  .status-label::after {
    content: "…";
  }
  @keyframes spark-pulse {
    0%,
    100% {
      opacity: 1;
      transform: scale(1);
    }
    50% {
      opacity: 0.45;
      transform: scale(0.88);
    }
  }
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
    .status-spark,
    .status-label {
      animation: none;
    }
  }
  .plan {
    flex: none;
    border-top: 1px solid var(--edge);
    padding: 4px 14px;
    font-size: var(--text-sm);
    max-height: 160px;
    overflow-y: auto;
  }
  .plan summary {
    color: var(--muted);
    cursor: pointer;
    user-select: none;
    list-style-position: inside;
    padding: 2px 0;
  }
  .plan-row {
    display: flex;
    gap: 8px;
    padding: 1px 0;
  }
  .plan-row.done {
    color: var(--muted);
  }
  .plan-mark {
    color: var(--accent);
    flex: none;
  }
  .bubble-row {
    display: flex;
    align-items: center;
    gap: 6px;
    max-width: 100%;
  }
  .rewind-btn {
    background: none;
    border: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-md);
    cursor: pointer;
    padding: 0 2px;
    opacity: 0;
    transition:
      opacity 0.12s ease,
      color 0.12s ease;
  }
  .msg.user:hover .rewind-btn,
  .rewind-btn:focus-visible {
    opacity: 1;
  }
  .rewind-btn:hover {
    color: var(--accent);
  }
  .suggestion-row {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 4px 10px 0;
    animation: rise 0.15s ease; /* @keyframes rise lives in app.css */
  }
  .suggestion {
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
    background: none;
    border: 1px dashed color-mix(in srgb, var(--accent) 40%, var(--edge));
    border-radius: 999px;
    padding: 2px 12px;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
    transition:
      color 0.12s ease,
      border-color 0.12s ease;
  }
  .suggestion:hover {
    color: var(--fg);
    border-color: var(--accent);
  }
  .suggestion-mark {
    color: var(--accent);
    flex: none;
  }
  .suggestion-text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .suggestion-x {
    background: none;
    border: none;
    color: var(--muted);
    cursor: pointer;
    padding: 0 4px;
    font-size: var(--text-md);
    flex: none;
  }
  .suggestion-x:hover {
    color: var(--fg);
  }
  .jump {
    position: sticky;
    bottom: 4px;
    align-self: center;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--warn);
    background: var(--term-bg);
    border: 1px solid color-mix(in srgb, var(--warn) 55%, var(--edge));
    border-radius: 999px;
    padding: 2px 12px;
    cursor: pointer;
  }
  .jump:hover {
    border-color: var(--warn);
  }
</style>
