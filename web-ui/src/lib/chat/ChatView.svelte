<script lang="ts">
  import { onDestroy, tick } from "svelte";
  import { rewindSession, renameSession, type Session } from "../workspace/sessions";
  import { fsValidate } from "../previews/files";
  import { listAgents } from "../workspace/launcher";
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import { insertIntoComposer } from "./composerBus";
  import { acquireChat, releaseChat, saveChatScroll, chatScroll, chatTurnStart } from "./chatPool";
  import { dismiss } from "../shared/dismiss";
  import { formatElapsedSeconds } from "../shared/time";
  import ChatHeader from "./ChatHeader.svelte";
  import Markdown from "./Markdown.svelte";
  import UserText from "./UserText.svelte";
  import ToolGroup from "./ToolGroup.svelte";
  import AgentsTray from "./AgentsTray.svelte";
  import BackgroundTray from "./BackgroundTray.svelte";
  import WorkTray from "../shared/WorkTray.svelte";
  import Chevron from "../shared/Chevron.svelte";
  import ArtifactGallery from "./ArtifactGallery.svelte";
  import PermissionCard from "./PermissionCard.svelte";
  import PlanApprovalCard from "./PlanApprovalCard.svelte";
  import QuestionCard from "./QuestionCard.svelte";
  import UsagePanel from "./UsagePanel.svelte";
  import McpPanel from "./McpPanel.svelte";
  import RewindDialog from "./RewindDialog.svelte";
  import Composer from "./Composer.svelte";
  import type { ImageAttachment } from "./images";
  import type { ChatBlock, PlanEntry } from "./store.svelte";

  interface Props {
    session: Session;
    focused: boolean;
    /** Workspace terminals for @term: mention grants. */
    terminals?: { id: string; name: string }[];
    /** Open a file path in an adjacent pane (the workbench path-click flow). */
    onOpenFile?: (path: string) => void;
    /** Kind-aware open: files → viewer pane, dirs → the Finder. */
    onOpenPath?: (path: string, kind: "file" | "dir") => void;
    /** Flip this session to its real TUI (the pane-bar view toggle). Used for
     *  interactive CLI flows the `-p` stream-json mode can't run — `/login`
     *  above all — so the native auth flow runs where it belongs. */
    onSwitchToTerminal?: () => void;
  }

  let {
    session,
    focused,
    terminals = [],
    onOpenFile,
    onOpenPath,
    onSwitchToTerminal,
  }: Props = $props();

  // The component is {#key}ed on session id by its parent: one instance per
  // session. The store + socket come from the session-keyed chat pool, so a
  // tab switch (which remounts this component) reuses the warm store and the
  // open socket instead of re-fetching the whole journal. Release keeps them
  // warm; the pool disposes them when the session ends or toggles to a PTY.
  // svelte-ignore state_referenced_locally
  const { store, socket } = acquireChat(session.id);
  // svelte-ignore state_referenced_locally
  onDestroy(() => releaseChat(session.id));

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
  // Seed scroll intent from the pool so a remount restores the reading
  // position instead of snapping to the bottom.
  // svelte-ignore state_referenced_locally
  let atBottom = $state(chatScroll(session.id).atBottom);
  let menu = $state<"model" | "mode" | "effort" | "mcp" | null>(null);

  /** Model picker: the agent's own catalog (claude initialize.models /
   *  codex model/list) beats the daemon's curated list. */
  const modelChoices = $derived(store.models.length > 0 ? store.models : models);
  /** The catalog row for the live model. Ids come in three spellings:
   *  picker values ("opus[1m]"), catalog resolvedModel
   *  ("claude-opus-4-8[1m]"), and the BARE api id assistant messages report
   *  ("claude-opus-4-8") — match all three, preferring named entries over
   *  "Default (recommended)" (both resolve to the same model). While the real
   *  model is not yet known (store.model === null, before init/ready resolves)
   *  this is undefined so the header shows a neutral loading chip — NOT a
   *  concrete "default" that would flash the wrong name (slow on remote). */
  const currentModel = $derived.by(() => {
    const target = store.model;
    if (target === null) return undefined;
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
    // Persist into the pool so the next remount restores this position.
    saveChatScroll(session.id, el.scrollTop, atBottom);
  }

  function scrollToBottom() {
    transcriptEl?.scrollTo({ top: transcriptEl.scrollHeight });
  }

  // On (re)mount, restore the saved reading position ONCE: bottom-pinned
  // sessions stick to the bottom, otherwise jump back to where the user was
  // reading. Guarded so it never re-fires mid-stream and fights the autoscroll.
  let didRestore = false;
  $effect(() => {
    const el = transcriptEl;
    if (el === null || didRestore) return;
    didRestore = true;
    const saved = chatScroll(session.id);
    void tick().then(() => {
      if (transcriptEl === null) return;
      if (saved.atBottom) scrollToBottom();
      else transcriptEl.scrollTop = saved.scrollTop;
    });
  });

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
    // The daemon owns delivery semantics so reconnect/replay stay exact:
    // mid-turn sends queue for the next run; Codex entries can be explicitly
    // promoted with Steer. Returns false when the socket isn't open so the
    // composer keeps the draft.
    return sendNow(text, images);
  }

  /** One never-lose-a-click path for every interactive AgentCommand. A closed
   *  socket cannot queue locally (replay would make that ambiguous), so keep
   *  the authoritative UI state unchanged and tell the user to retry. */
  function sendCommand(command: Record<string, unknown>, failure: string): boolean {
    if (socket.send(command)) return true;
    store.notice(`not connected — ${failure}, try again in a moment`, "error");
    return false;
  }

  function decide(requestId: string, optionId: string, destination?: string, feedback?: string) {
    // Never lose a decision to a closed socket: the card stays answerable
    // (no resolved event will arrive), so say why nothing happened.
    sendCommand(
      {
        type: "permission",
        request_id: requestId,
        option_id: optionId,
        ...(destination !== undefined ? { destination } : {}),
        ...(feedback !== undefined ? { feedback } : {}),
      },
      "decision not sent",
    );
  }

  function answer(requestId: string, answers: Record<string, string[]>) {
    sendCommand({ type: "answer", request_id: requestId, answers }, "answer not sent");
  }

  function interrupt() {
    sendCommand({ type: "interrupt" }, "stop not sent");
  }

  // Stop/background ride the same never-lose-a-click contract as decide():
  // a send into a closed socket says why nothing happened.
  function stopTask(id: string) {
    sendCommand({ type: "stop_task", task_id: id }, "stop not sent");
  }
  function backgroundTool(id: string) {
    sendCommand({ type: "background_tool", tool_call_id: id }, "background request not sent");
  }

  /** Pull back a still-queued message before the agent consumes it. The store
   *  removes it on the resulting `user_message_update{cancelled}` (deterministic
   *  from the wire, so replay agrees). Respect the closed-socket rule: if it
   *  can't send, the message stays queued and the user can retry. */
  function cancelQueued(id: string) {
    sendCommand({ type: "cancel_queued", id }, "couldn't cancel");
  }

  /** Promote one Codex follow-up from the next-run FIFO into the active turn.
   *  The pending bubble stays until the driver's turn/steer RPC resolves, so
   *  a disconnect or rejection never lies about delivery. */
  function steerQueued(id: string) {
    sendCommand({ type: "steer_queued", id }, "couldn't steer");
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
          return pickModel(hit.id);
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
          return pickMode(hit.id);
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
        return sendCommand({ type: "get_usage" }, "usage request not sent");
      case "compact":
        // Codex has no slash catalog; thread/compact/start is the native
        // path (the compaction turn's notice confirms completion). Claude's
        // own /compact rides its catalog — fall through to the CLI send.
        if (agentKind !== "codex") return false;
        if (!sendCommand({ type: "compact" }, "compact request not sent")) return false;
        return true;
      case "mcp":
        if (agentKind === "claude") {
          if (!sendCommand({ type: "get_mcp" }, "MCP request not sent")) return false;
          store.mcpServers = null;
          menu = "mcp";
          return true;
        }
        return false;
      case "effort":
        if (!hasEffort) return false;
        if (arg.length > 0 && effortChoices.includes(arg)) {
          return pickEffort(arg);
        } else {
          menu = "effort";
        }
        return true;
      case "ultracode":
        if (!hasUltracode) return false;
        if (arg === "on" || arg === "off") {
          return setUltracode(arg === "on");
        } else {
          return toggleUltracode();
        }
      case "login":
        // /login is an interactive OAuth / setup-token / SSO flow the `-p`
        // stream-json CLI can't run ("/login isn't available in this
        // environment") — so an expired session dead-ends in chat with no way
        // back. Flip to the real TUI, where claude's own /login handles every
        // auth method safely (chimaera never sees the credentials); sign in
        // there, then toggle back to chat with the pane-bar button. Claude
        // only for now — codex's auth flow (`codex login`) is a follow-up.
        if (agentKind !== "claude" || onSwitchToTerminal === undefined) return false;
        onSwitchToTerminal();
        return true;
      default:
        return false;
    }
  }

  function setUltracode(enabled: boolean): boolean {
    return sendCommand({ type: "set_ultracode", enabled }, "ultracode change not sent");
  }

  function toggleUltracode(): boolean {
    return setUltracode(!store.ultracode);
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
      native.push({ name: "login", description: "sign in — opens the terminal for Claude's native auth" });
    }
    if (agentKind === "codex") {
      native.push({ name: "compact", description: "compact conversation context" });
    }
    const nativeNames = new Set(native.map((n) => n.name));
    return [...native, ...store.slashCommands.filter((c) => !nativeNames.has(c.name))];
  });

  // --- checkpoint rewind ------------------------------------------------------
  // Claude: click "rewind" on a user message → dry-run report (rewind_files) →
  // confirm dialog → restore files → optionally fork the conversation there.
  // Codex: no file restore exists — the dialog is a plain confirmation and the
  // rewind is conversation-only (the daemon rolls the thread back in place).
  // The intent flag keeps replayed RewindResult events from reopening UI.
  const conversationOnlyRewind = agentKind !== "claude";
  let rewindIntent = $state<null | {
    id: string;
    preceding: string | null;
    fork: boolean;
    stage: "dry" | "confirm" | "applying";
  }>(null);
  const rewindReport = $derived(
    rewindIntent !== null && store.rewind?.userMessageId === rewindIntent.id
      ? store.rewind
      : null,
  );

  function askRewind(checkpoint: { id: string; preceding: string | null }) {
    store.rewind = null;
    if (conversationOnlyRewind) {
      rewindIntent = { id: checkpoint.id, preceding: checkpoint.preceding, fork: true, stage: "confirm" };
    } else {
      rewindIntent = { id: checkpoint.id, preceding: checkpoint.preceding, fork: false, stage: "dry" };
      if (
        !sendCommand(
          { type: "rewind", user_message_id: checkpoint.id, dry_run: true },
          "rewind check not sent",
        )
      ) {
        // Do not leave the dialog forever in its loading state. The user can
        // retry the rewind button once the socket is ready.
        rewindIntent = null;
      }
    }
  }

  function confirmRewind(fork: boolean) {
    if (rewindIntent === null) return;
    if (conversationOnlyRewind) {
      const preceding = rewindIntent.preceding;
      if (preceding === null) {
        rewindIntent = null;
        return;
      }
      rewindIntent = { ...rewindIntent, stage: "applying" };
      void rewindSession(session.id, preceding)
        .then(() => {
          rewindIntent = null;
        })
        .catch((e: unknown) => {
          rewindIntent = null;
          store.notice(`rewind failed: ${String(e)}`, "error");
      });
      return;
    }
    const intent = rewindIntent;
    if (
      !sendCommand(
        { type: "rewind", user_message_id: intent.id, dry_run: false },
        "rewind request not sent",
      )
    ) {
      return;
    }
    rewindIntent = { ...intent, fork, stage: "applying" };
    store.rewind = null;
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

  function pickModel(id: string): boolean {
    if (!sendCommand({ type: "set_model", model_id: id }, "model change not sent")) return false;
    menu = null;
    return true;
  }

  function pickMode(id: string): boolean {
    if (!sendCommand({ type: "set_mode", mode_id: id }, "mode change not sent")) return false;
    menu = null;
    return true;
  }

  /** Shift+Tab from the composer advances to the next permission mode, wrapping
   *  round — the same cycle the agent TUIs offer. No-op when the agent exposes
   *  no modes; an unknown current mode starts the cycle at the first entry. */
  function cycleMode() {
    if (store.modes.length === 0) return;
    const cur = store.modes.findIndex((m) => m.id === store.currentMode);
    const next = store.modes[(cur + 1) % store.modes.length];
    if (next.id !== store.currentMode) pickMode(next.id);
  }

  function pickEffort(id: string): boolean {
    if (!sendCommand({ type: "set_effort", effort_id: id }, "effort change not sent")) return false;
    effort = id;
    menu = null;
    return true;
  }

  const EFFORT_HINT: Record<string, string> = {
    claude: "reasoning effort — applies immediately, this session only",
    codex: "reasoning effort — applies from the next message",
  };

  /** Extended-thinking toggle (claude). ON by default — chimaera's chat is a
   *  workbench for real coding work, where the reasoning pass earns its keep;
   *  the chip shows an explicit on/off and tints when on, so the state (and the
   *  cost) is never hidden, and one click turns it off. The preference lives in
   *  the pooled store, not here, so a tab remount keeps it. */
  const hasThinking = $derived(agentKind === "claude");
  /** Effective thinking state: the user's explicit choice, or ON by default
   *  (the reasoning pass earns its keep in a coding workbench). `null` in the
   *  store means "unchosen" — so a toggle-off (a real `false`) is never
   *  mistaken for the default and re-forced on. */
  const thinkingOn = $derived(store.thinkingEnabled ?? true);
  function toggleThinking() {
    const next = !thinkingOn;
    store.setThinking(next);
    if (!sendCommand({ type: "set_thinking", enabled: next }, "thinking change not sent")) {
      // Keep the user's preference, but mark it unsynchronized so the existing
      // connected-effect retries it on the next ready frame.
      store.markThinkingPending();
    }
  }
  // Push the effective preference to the live driver, once per driver process.
  // It pushes whatever the user's effective choice IS (never forces a value),
  // so it can't override a toggle-off; `thinkingPushed` is reset on each `init`
  // (a fresh process defaults thinking OFF) so a respawn/resume/view-toggle
  // re-syncs, and is marked only once the send actually leaves so an
  // undelivered push retries instead of stranding the chip out of sync.
  $effect(() => {
    if (!hasThinking || !store.connected || store.thinkingPushed) return;
    if (socket.send({ type: "set_thinking", enabled: thinkingOn })) {
      store.markThinkingPushed();
    }
  });

  const modeLabel = $derived(
    store.modes.find((m) => m.id === store.currentMode)?.label ?? store.currentMode,
  );
  /** Model chip: the catalog's own display name when known ("Opus",
   *  "Fable"), else a readable fallback from the raw id. */
  const modelLabel = $derived.by(() => {
    if (currentModel !== undefined) return currentModel.label;
    const m = store.model;
    if (m === null) {
      // A fresh session never reports a model until its first turn — don't
      // skeleton forever. Once the catalog is loaded, show the DEFAULT it will
      // use (correct for a new chat); only the brief pre-catalog window (no
      // choices yet) stays null → skeleton.
      const def = modelChoices.find((c) => c.id === "default") ?? modelChoices[0];
      return def?.label ?? null;
    }
    const match = /claude-(\w+)-(\d+)-(\d+)/.exec(m);
    return match !== null ? `${match[1]} ${match[2]}.${match[3]}` : m;
  });

  /** Live status line under the transcript: what the agent is doing NOW.
   *  Phases: starting → compacting / thinking / writing / {tool title} → working
   *  (between tools) → gone. */
  const agentBusy = $derived(store.running || store.compacting);
  const activityLabel = $derived.by(() => {
    if (store.compacting) return "compacting context";
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

  // Elapsed-turn timer: a quiet counter that surfaces once a turn passes 5s and
  // ticks each second. The START is held in the chat pool (per session), so
  // switching away mid-turn and back keeps counting from the real turn start
  // instead of resetting — and performance.now() never leaks into replay. The
  // interval tears down when the turn ends or the component unmounts.
  let turnElapsedMs = $state(0);
  $effect(() => {
    const start = chatTurnStart(session.id, agentBusy, performance.now());
    if (start === null) {
      turnElapsedMs = 0;
      return;
    }
    turnElapsedMs = performance.now() - start;
    const iv = setInterval(() => {
      turnElapsedMs = performance.now() - start;
    }, 1000);
    return () => clearInterval(iv);
  });
  /** Upward elapsed for the live status row (shared ladder: "7s", "1m 04s",
   *  "1h 02m 03s"). Null below 5s so quick turns stay uncluttered. */
  const turnElapsedLabel = $derived.by(() => {
    const total = Math.floor(turnElapsedMs / 1000);
    if (total < 5) return null;
    return formatElapsedSeconds(total);
  });
  /** A completed turn's duration for the turn-end badge. Sub-minute keeps one
   *  decimal ("2.4s"); a minute or more switches to the shared ladder so a
   *  long turn never renders as a raw "2664.6s". */
  function formatDurationMs(ms: number): string {
    const totalSec = ms / 1000;
    if (totalSec < 60) return `${totalSec.toFixed(1)}s`;
    return formatElapsedSeconds(Math.floor(totalSec));
  }

  const planDone = $derived(store.plan.filter((p) => p.status === "done").length);
  /** The step the agent is on now — surfaced in the plan summary so the
   *  current goal is legible without expanding the panel. `activeForm` is the
   *  agent's own present-continuous phrasing for exactly this spot ("Running
   *  tests"), so prefer it and fall back to the subject. */
  const planActive = $derived.by(() => {
    const active = store.plan.find((p) => p.status === "in_progress");
    return active ? (active.activeForm ?? active.content) : null;
  });

  /** Blocked is orthogonal to status: a blocked task is still `todo`, so it
   *  would otherwise render identically to one that simply hasn't started.
   *  The server already filters `blockedBy` to blockers that are still open. */
  const isBlocked = (entry: PlanEntry) => entry.status !== "done" && entry.blockedBy.length > 0;
  const planMark = (entry: PlanEntry) =>
    entry.status === "done"
      ? "✓"
      : entry.status === "in_progress"
        ? "◐"
        : isBlocked(entry)
          ? "⊘"
          : "○";
  /** Agents often restate the subject as the description; showing both then is
   *  pure noise in a panel this small. */
  const planDetail = (entry: PlanEntry) => {
    const detail = entry.description?.trim();
    return detail && detail !== entry.content.trim() ? detail : null;
  };
  /** Finished work folds away: on a long plan the ✓ rows are the majority and
   *  push what's actually next out of view. They stay one click away, and when
   *  EVERYTHING is done there is nothing else to show, so the fold steps
   *  aside rather than leaving an empty panel. */
  let showFinished = $state(false);
  let planOpen = $state(false);
  const planLive = $derived(store.plan.filter((p) => p.status !== "done"));
  const planFinished = $derived(store.plan.filter((p) => p.status === "done"));
  const planFolds = $derived(planLive.length > 0 && planFinished.length > 0);
  const planRows = $derived(planFolds && !showFinished ? planLive : store.plan);
  const planLabel = $derived(
    `plan · ${planDone}/${store.plan.length}` +
      (planActive !== null ? ` · ◐ ${planActive}` : planLive.length === 0 ? " · all done" : ""),
  );

  /** Subagents in flight right now — promoted into the live tray above the
   *  composer. They also keep their in-place "Agent:" rows in the transcript
   *  (the history); the tray is the glanceable live monitor. Reconciled shut
   *  at turn end like any tool, so a finished/abandoned run never lingers. */
  const activeAgents = $derived(
    store.blocks.filter(
      (b): b is Extract<ChatBlock, { kind: "tool" }> =>
        b.kind === "tool" && b.tool === "agent" && b.status === "in_progress",
    ),
  );

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
      // Every user block in `blocks` is delivered — queued/undelivered sends
      // live in the pending transcript tail (`store.pendingSends`), never
      // here — so they all render inline in transcript order.
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

  /** Index of the last block (the streaming reveal keys off it). Queued sends
   *  render from their own pending tail, not `blocks`, so this is simply the
   *  delivered-block tail. */
  const lastInlineIndex = $derived(store.blocks.length - 1);
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
    thinking={thinkingOn}
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
        <ToolGroup
          tools={item.tools}
          active={store.running && item === renderItems[renderItems.length - 1]}
          {onOpenFile}
          onBackground={agentKind === "claude" ? backgroundTool : undefined}
          onStopTask={agentKind === "claude" ? stopTask : undefined}
        />
      {:else if item.block.kind === "user"}
        {@const block = item.block}
        <!-- Only delivered (sent) user messages render inline; queued/dropped
             ones live in the pending tail below. -->
        <div class="msg user">
          <div class="bubble-row">
            <!-- Codex rewinds whole turns from a preceding anchor, so its
                 first message (nothing precedes it) offers no button; claude
                 can still restore files there. -->
            {#if block.checkpoint !== null && (agentKind === "claude" || block.checkpoint.preceding !== null)}
              <button
                class="rewind-btn"
                title={agentKind === "claude"
                  ? "rewind to before this message (restores files; optionally forks the conversation)"
                  : "rewind the conversation to before this message"}
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
            streaming={store.running && item.index === lastInlineIndex}
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
      {:else if item.block.kind === "question"}
        <!-- The transcript's memory of an ask: invisible while the pending
             overlay below is the answerable card, a quiet question+answer
             card once resolved (replay rebuilds the same). -->
        {#if item.block.resolved}
          <QuestionCard
            request={{ requestId: item.block.id, questions: item.block.questions, expiresAtMs: null }}
            answered={item.block.answers}
          />
        {/if}
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
            <span>{formatDurationMs(block.durationMs)}</span>
          </div>
        {/if}
      {:else if item.block.kind === "usage"}
        <UsagePanel windows={item.block.windows} />
      {/if}
    {/each}

    {#each store.pending as request (request.requestId)}
      {#if request.plan !== null}
        <PlanApprovalCard
          {request}
          onDecide={(opt, feedback) => decide(request.requestId, opt, undefined, feedback)}
          onOpenPath={openProsePath}
          resolvePaths={resolveProsePaths}
        />
      {:else}
        <PermissionCard
          {request}
          onDecide={(opt, dest, feedback) => decide(request.requestId, opt, dest, feedback)}
        />
      {/if}
    {/each}

    {#each store.questions as request (request.requestId)}
      <QuestionCard {request} onAnswer={(answers) => answer(request.requestId, answers)} />
    {/each}

    {#if agentBusy && store.pending.length === 0 && store.questions.length === 0}
      <div class="status-row" aria-live="polite">
        <span class="status-spark">
          <SessionGlyph kind="agent" {agentKind} size={12} state="alive" />
        </span>
        <span class="status-label">{activityLabel}</span>
        {#if turnElapsedLabel !== null}
          <span class="status-elapsed">{turnElapsedLabel}</span>
        {/if}
        {#if store.compacting}
          <span class="compaction-progress" role="progressbar" aria-label="Compacting conversation">
            <span></span>
          </span>
        {/if}
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

    <!-- Pending sends are part of the scrollable reading surface, but remain
         OUT of `blocks`: a mid-turn send must not splice the agent's current
         response. Keeping the stack at the transcript tail makes queued text
         inspectable without pinning it to (and crowding) the composer. On
         delivery it leaves this stack and enters `blocks` as the newest user
         turn. A Stop preserves it; ✕ cancels it. Dropped sends remain visible
         as "not delivered" until dismissed, with replay-safe state owned by
         the daemon. -->
    {#if store.pendingSends.length > 0}
      <div class="pending" aria-label="queued messages" aria-live="polite">
        {#each store.pendingSends as send (send.id)}
          <div class="msg user pending-msg" class:dropped={send.state === "dropped"}>
            <div class="bubble-row">
              <div class="bubble">
                <UserText
                  text={send.text}
                  onOpenPath={openProsePath}
                  resolvePaths={resolveProsePaths}
                />
              </div>
              {#if agentKind === "codex" && send.state === "queued" && store.running}
                <button
                  class="steer-btn"
                  title="add this message to the current run"
                  aria-label="steer queued message into current run"
                  onclick={() => steerQueued(send.id)}
                >↪ Steer</button>
              {/if}
              <button
                class="cancel-btn"
                title={send.state === "dropped"
                  ? "dismiss (this message was never delivered)"
                  : "cancel this queued message (remove it before the agent sees it)"}
                aria-label={send.state === "dropped"
                  ? "dismiss undelivered message"
                  : "cancel queued message"}
                onclick={() => cancelQueued(send.id)}
              >
                ✕
              </button>
            </div>
            <span class="delivery" class:dropped={send.state === "dropped"}>
              {send.state === "dropped" ? "not delivered" : "queued"}
            </span>
            {#if send.attachments > 0}
              <span class="attach"
                >{send.attachments} image{send.attachments > 1 ? "s" : ""}</span
              >
            {/if}
          </div>
        {/each}
      </div>
    {/if}

    {#if !atBottom && store.pending.length > 0}
      <button class="jump" onclick={scrollToBottom}>
        permission needed ↓
      </button>
    {/if}
    </div>
  </div>

  {#if activeAgents.length > 0}
    <AgentsTray agents={activeAgents} onStop={agentKind === "claude" ? stopTask : undefined} />
  {/if}

  {#if store.backgroundTasks.length > 0}
    <!-- Background work (backgrounded Bash / workflows) — stopTask sends the
         native task key the wire gave us; the driver passes it through. -->
    <BackgroundTray
      tasks={store.backgroundTasks}
      onStop={agentKind === "claude" ? stopTask : undefined}
    />
  {/if}

  {#if store.plan.length > 0}
    <!-- Same shell as the subagent/background strips: one collapsible family
         above the composer instead of three different-looking bars. The glyph
         only breathes while a step is actually in flight. -->
    <WorkTray glyph="≡" label={planLabel} bind:open={planOpen} pulse={planActive !== null}>
      {#if planFolds}
        <button class="plan-fold" onclick={() => (showFinished = !showFinished)}>
          <Chevron open={showFinished} />
          <span>{planFinished.length} done</span>
        </button>
      {/if}
      {#each planRows as entry, i (entry.id ? `id:${entry.id}` : `ix:${i}`)}
        <div
          class="plan-row"
          class:done={entry.status === "done"}
          class:blocked={isBlocked(entry)}
        >
          <span class="plan-mark">{planMark(entry)}</span>
          <span class="plan-body">
            <span class="plan-line">
              <span class="plan-subject">{entry.content}</span>
              {#if entry.owner}<span class="plan-owner">@{entry.owner}</span>{/if}
              {#if isBlocked(entry)}<span class="plan-blocked"
                  >blocked by {entry.blockedBy.map((id) => `#${id}`).join(", ")}</span
                >{/if}
            </span>
            {#if planDetail(entry)}
              <span class="plan-desc">{planDetail(entry)}</span>
            {/if}
          </span>
        </div>
      {/each}
    </WorkTray>
  {/if}

  {#if rewindIntent !== null}
    <RewindDialog
      intent={rewindIntent}
      report={rewindReport}
      conversationOnly={conversationOnlyRewind}
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

  {#if store.promptSuggestion !== null && !agentBusy}
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
    running={agentBusy}
    disabled={store.exited !== null || store.degraded}
    slashCommands={composerCommands}
    workspaceId={session.workspace_id ?? null}
    {terminals}
    {focused}
    {onSubmit}
    onInterrupt={interrupt}
    onCycleMode={cycleMode}
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
  /* Every pinned strip (subagents, background, plan) — they were full-bleed
     while the plan alone was inset, so the group never lined up. */
  .chat > :global(.tray) {
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
  /* Undelivered messages occupy the transcript tail, not fixed composer
     chrome. The transcript's own scrollbar can therefore move a large queue
     out of the way while the pending state remains visible at the live end. */
  .pending {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-top: 8px;
    padding-bottom: 10px;
  }
  /* A pending bubble is half-present — not in the conversation yet (claude's
     native mid-turn queue / a codex steer in flight). Reuses .msg.user's
     right-alignment so the queued→sent transition is visually continuous:
     the same bubble un-fades and moves up into the transcript on delivery.
     Tighter margins than an inline turn (the stack sets its own gap). */
  .pending-msg {
    margin-top: 0;
    margin-bottom: 0;
  }
  .pending-msg .bubble {
    opacity: 0.55;
    /* Outline, not border: follows the radius with zero layout shift. */
    outline: 1px dashed color-mix(in srgb, var(--fg) 30%, transparent);
    outline-offset: -1px;
  }
  /* Dropped (e.g. the agent process died before delivery): the text stays
     readable — no strikethrough — so it can be copied and re-sent by hand. */
  .pending-msg.dropped .bubble {
    outline-style: solid;
    outline-color: color-mix(in srgb, var(--err) 45%, transparent);
  }
  .steer-btn {
    flex: none;
    background: none;
    border: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
    padding: 3px 4px;
    border-radius: 6px;
    transition:
      color 0.12s ease,
      background 0.12s ease;
  }
  .steer-btn:hover,
  .steer-btn:focus-visible {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 9%, transparent);
  }
  .delivery {
    color: var(--muted);
    font-size: var(--text-xs);
    margin-top: 2px;
  }
  .delivery.dropped {
    color: var(--err);
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
  /* Elapsed counter: a still, muted number beside the pulsing label (only
     appears past 5s). No animation — reduced-motion safe by construction. */
  .status-elapsed {
    font-family: var(--mono, monospace);
    font-variant-numeric: tabular-nums;
    color: color-mix(in srgb, var(--muted) 80%, transparent);
  }
  /* Compaction has no honest percentage on either agent wire. Show a bounded
     indeterminate track instead of inventing one; start/completion still come
     from journaled protocol events, so reconnect never resets the truth. */
  .compaction-progress {
    position: relative;
    width: clamp(48px, 12vw, 112px);
    height: 2px;
    overflow: hidden;
    border-radius: 999px;
    background: color-mix(in srgb, var(--edge) 65%, transparent);
  }
  .compaction-progress > span {
    position: absolute;
    inset-block: 0;
    width: 42%;
    border-radius: inherit;
    background: var(--accent);
    animation: compact-sweep 1.35s ease-in-out infinite;
  }
  @keyframes compact-sweep {
    from {
      transform: translateX(-110%);
    }
    to {
      transform: translateX(340%);
    }
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
    .status-label,
    .compaction-progress > span {
      animation: none;
    }
    .compaction-progress > span {
      inset-inline-start: 29%;
    }
  }
  /* The strip chrome (border, padding, collapse header, bounded scroll) now
     comes from the shared WorkTray, so only the rows are styled here. */
  .plan-fold {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 1px 0 3px;
    background: none;
    border: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    text-align: left;
    cursor: pointer;
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
  /* Blocked reads as "waiting", not "active": the mark drops to muted so a
     ⊘ can't be mistaken for progress at a glance. */
  .plan-row.blocked .plan-mark {
    color: var(--muted);
  }
  .plan-body {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .plan-line {
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
  }
  /* Every text span clips rather than wraps — the panel is a glance surface,
     and an agent subject can be arbitrarily long. */
  .plan-subject,
  .plan-desc {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .plan-owner,
  .plan-blocked {
    flex: none;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  /* Mixed toward --fg, not --muted: accent-over-muted lands near 3.5:1 on the
     light background, too weak for an 11px chip. Same blend as .plan-active. */
  .plan-owner {
    color: color-mix(in srgb, var(--accent) 70%, var(--fg));
  }
  .plan-desc {
    color: var(--muted);
    font-size: var(--text-xs);
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
  /* The ✕ on a queued bubble: pull it back before the agent sees it. Quiet by
     default (mirrors .rewind-btn), reveals on hover/focus of the pending row,
     and warms to --err on hover since it discards. */
  .cancel-btn {
    background: none;
    border: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-sm);
    line-height: 1;
    cursor: pointer;
    padding: 0 2px;
    opacity: 0;
    transition:
      opacity 0.12s ease,
      color 0.12s ease;
  }
  .pending-msg:hover .cancel-btn,
  .cancel-btn:focus-visible {
    opacity: 1;
  }
  .cancel-btn:hover {
    color: var(--err);
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
