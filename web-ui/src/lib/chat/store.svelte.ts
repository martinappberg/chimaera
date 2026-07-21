/**
 * Per-session chat state: a pure reducer over the normalized agent-event
 * stream. Journal replay and live frames go through the SAME `apply` path
 * (dedup by seq), so reconnects and multi-window views are correct by
 * construction — there is no separate "catch up" code to get wrong.
 */

import { canInlinePreview, isImagePath } from "../previews/files";
import type { AgentEvent, ChatSessionInfo, SeqEvent } from "./chatWs";

/** The single leading notice a client-side transcript trim leaves behind. */
const TRIM_NOTICE = "earlier history trimmed";

export interface ToolContent {
  kind: "output" | "diff" | "batch";
  text?: string;
  path?: string;
  old_text?: string | null;
  new_text?: string;
  truncated?: boolean;
  diffs?: ToolContent[];
}

/** One plan row. `content` is the display text every agent fills; the rest is
 *  richness only claude's `Task*` family carries, so it is all nullable — a
 *  TodoWrite or codex plan simply has none of it. */
export interface PlanEntry {
  content: string;
  status: "todo" | "in_progress" | "done";
  /** The agent's own task id ("1", "2", …) — what `blockedBy` refers to. */
  id: string | null;
  /** Present-continuous form ("Running tests"), shown while in progress. */
  activeForm: string | null;
  description: string | null;
  /** Owning agent name, once claimed. */
  owner: string | null;
  /** Ids of still-open tasks that must finish first (server-filtered to the
   *  ones still open). Orthogonal to `status` — a blocked task is still todo. */
  blockedBy: string[];
}

export interface PermissionOption {
  id: string;
  label: string;
  kind: "allow_once" | "allow_always" | "reject_once" | "reject_always";
}

export interface PendingPermission {
  requestId: string;
  toolCallId: string | null;
  title: string;
  options: PermissionOption[];
  inputPreview: unknown;
  /** Plan markdown when this is a plan approval (claude ExitPlanMode) —
   *  non-null ⇒ rendered as the plan-approval card. */
  plan: string | null;
}

export interface QuestionOption {
  label: string;
  description: string;
}

export interface Question {
  id: string;
  header: string;
  question: string;
  options: QuestionOption[];
  multiSelect: boolean;
}

export interface PendingQuestion {
  requestId: string;
  questions: Question[];
  /** Absolute Unix-millisecond auto-skip deadline (Codex only). Absolute so
   *  journal replay preserves the original countdown rather than restarting. */
  expiresAtMs: number | null;
}

export interface CheckpointRef {
  /** The user message's own uuid — the rewind_files key. */
  id: string;
  /** The message preceding it — the conversation-fork resume point. */
  preceding: string | null;
}

/** A user message the agent has NOT consumed yet — shown in the scrollable
 *  transcript tail, deliberately kept OUT of transcript `blocks` so it can
 *  never splice into a running turn's output. Rebuilt purely from journaled
 *  events (`UserMessage{queued}` + its `UserMessageUpdate`), so replay agrees:
 *  `sent` moves it into `blocks`, `cancelled` removes it, `dropped` keeps it
 *  here marked "not delivered". */
export interface PendingSend {
  /** Delivery key (the wire's client-minted uuid) — the `UserMessageUpdate` /
   *  `CancelQueued` match key. */
  id: string;
  text: string;
  attachments: number;
  /** Its checkpoint anchor arrives (claude) right after the queued echo; kept
   *  here so it rides along when the send is promoted into `blocks`. */
  checkpoint: CheckpointRef | null;
  /** queued = still waiting for the agent (a Stop does NOT drop it — the queue
   *  delivers after the aborted turn; its ✕ is how you discard one); dropped =
   *  genuinely undeliverable (the agent process died / a codex steer failed for
   *  good) — kept visible as "not delivered" so the text can be copied and
   *  re-sent, until its ✕ dismisses it (the same `cancel_queued` command; the
   *  driver's tombstone `Cancelled` makes the dismissal survive replay). */
  state: "queued" | "dropped";
}

export type ChatBlock =
  | {
      /** A DELIVERED user message, in transcript order. A queued send is NOT a
       *  block — it lives in `pendingSends` until it resolves `sent`, then it
       *  is appended here at the current end (after the turn it waited behind),
       *  never spliced into a running turn's output. So a user block in `blocks`
       *  is always one the agent received. */
      kind: "user";
      text: string;
      attachments: number;
      checkpoint: CheckpointRef | null;
      /** Delivery key (the wire's client-minted uuid); null on old journals,
       *  transcript-seeded messages, and permission-feedback echoes. */
      id: string | null;
      /** Inclusive journal boundary for a portable fork through this row. */
      forkSeq: number;
    }
  | {
      kind: "message";
      text: string;
      turnId: string;
      /** Wall-clock timestamp of the first text chunk in this assistant block.
       *  Journal-backed so replay and reconnect preserve the original time. */
      sentAtMs: number;
      /** Inclusive journal boundary for a portable fork through this row. */
      forkSeq: number;
      /** Codex thread/fork can use turnId only after this is the completed
       *  turn's final assistant block. */
      nativeTurnComplete: boolean;
    }
  | { kind: "thought"; text: string; turnId: string }
  | {
      /** A structured question, folded into history at the position it was
       *  asked. While pending it renders nothing here (the interactive card
       *  is the overlay); once resolved it renders as a quiet answered card —
       *  question + chosen labels — and replay rebuilds the same. */
      kind: "question";
      id: string;
      questions: Question[];
      /** Chosen labels per question id; empty = resolved without an answer
       *  (cancelled/expired, or a pre-answers journal). */
      answers: Record<string, string[]>;
      resolved: boolean;
    }
  | {
      kind: "tool";
      id: string;
      tool: string;
      title: string;
      locations: string[];
      status: "pending" | "in_progress" | "completed" | "failed";
      content: ToolContent | null;
      denied: boolean;
      /** The user answered this tool's permission prompt with an allow option
       *  (claude `allow_*`, codex `accept*`) — distinct from a plain completion
       *  so the card can read "allowed" rather than dumping the command. */
      allowed: boolean;
      /** Live output streamed ahead of the authoritative result. */
      streaming: boolean;
      /** Process-owned work that may outlive its parent turn (Codex collab
       *  agents). Turn-end reconciliation must leave this row live until its
       *  own completion event or driver exit. */
      crossTurn: boolean;
    }
  | { kind: "notice"; text: string; tone: "info" | "error" }
  | {
      kind: "turn_end";
      costUsd: number | null;
      outputTokens: number;
      durationMs: number;
      /** Previewable files this turn produced (absolute paths) — rendered as
       *  a small gallery after the closing prose. */
      artifacts: string[];
    }
  | { kind: "usage"; windows: UsageWindow[] };

export interface RateLimitInfo {
  utilization: number;
  label: string | null;
  resetsAt: string | null;
  limitReached: boolean;
}

export interface RewindReport {
  userMessageId: string;
  canRewind: boolean;
  filesChanged: string[];
  applied: boolean;
  error: string | null;
}

export interface McpServer {
  name: string;
  status: string;
  tools: number;
  error: string | null;
}

export interface UsageWindow {
  label: string;
  /** 0-100. */
  utilization: number;
  resets_at?: string | null;
}

/** One running background task (claude's backgrounded Bash / workflows).
 *  Mirrors the wire's `background_tasks` set member. */
export interface BackgroundTask {
  /** The agent's own task key — sent back verbatim as stop_task's task_id. */
  id: string;
  /** Lane name, verbatim (local_bash, local_workflow, …). */
  taskType: string;
  description: string;
  /** The agent's own status word (`running` until it patches it). */
  status: string;
  /** Driver-stamped epoch ms at first sight — the elapsed display's anchor. */
  startedAtMs: number;
  /** The workflow's meta.name (local_workflow lanes) — the row's title. */
  workflowName: string | null;
  /** Per-agent progress (capped server-side, newest kept) — the dot row. */
  agents: WorkflowAgent[];
  /** Aggregates counted over the WHOLE wire list — honest beyond the cap. */
  agentsTotal: number;
  agentsDone: number;
}

/** One workflow agent's progress (a `BackgroundTask.agents` member). */
export interface WorkflowAgent {
  /** The workflow's own 1-based agent number. */
  index: number;
  label: string;
  /** The wire's state word verbatim (start | done | …) — never remapped. */
  state: string;
  /** Head of the agent's final text, once done. */
  resultPreview: string | null;
}

export interface ModeInfo {
  id: string;
  label: string;
}

export interface SlashCommand {
  name: string;
  description?: string;
  /** Codex `skills/list` path. Present only for real skill entries, so the
   *  composer can add the protocol-native skill block when the slash token is
   *  used inline or as a whole message. */
  skill_path?: string;
}

export class ChatStore {
  blocks = $state<ChatBlock[]>([]);
  pending = $state<PendingPermission[]>([]);
  /** Structured questions from the agent (claude AskUserQuestion / codex
   *  requestUserInput) — rendered as QuestionCards. */
  questions = $state<PendingQuestion[]>([]);
  plan = $state<PlanEntry[]>([]);
  model = $state<string | null>(null);
  modes = $state<ModeInfo[]>([]);
  currentMode = $state<string | null>(null);
  slashCommands = $state<SlashCommand[]>([]);
  running = $state(false);
  /** The agent is summarizing conversation history to reclaim its context
   *  window. Journal-derived (not an optimistic button state), so automatic
   *  compaction and reconnect/replay behave exactly like manual /compact. */
  compacting = $state(false);
  /** What the agent is doing RIGHT NOW, for the live status row:
   *  thinking (reasoning deltas), writing (prose deltas), a tool title,
   *  or waiting (turn open, nothing streaming yet). */
  activity = $state<null | { kind: "thinking" | "writing" | "tool" | "waiting"; detail: string }>(
    null,
  );
  exited = $state<null | { status: number | null }>(null);
  degraded = $state(false);
  connected = $state(false);
  fatalError = $state<string | null>(null);
  /** Context-window occupancy, 0-100 (claude get_context_usage after each
   *  turn; codex tokenUsage vs modelContextWindow). */
  contextPct = $state<number | null>(null);
  contextTokens = $state<{ total: number; max: number } | null>(null);
  /** Streamed account rate-limit telemetry (header chip). */
  rateLimit = $state<RateLimitInfo | null>(null);
  /** Agent's own model catalog (claude initialize.models / codex
   *  model/list), with per-model efforts. `resolved` is the runtime id a
   *  picker value maps to (claude's `model` reports resolvedModel). */
  models = $state<
    {
      id: string;
      label: string;
      description: string | null;
      resolved: string | null;
      efforts: string[];
      defaultEffort: string | null;
    }[]
  >([]);
  /** Applied effort/ultracode truth (claude get_settings read-back). */
  effort = $state<string | null>(null);
  ultracode = $state(false);
  /** Latest rewind_files answer; the view gates dialogs on its own intent
   *  flag so replayed reports never reopen UI. */
  rewind = $state<RewindReport | null>(null);
  /** MCP inventory (the /mcp panel). */
  mcpServers = $state<McpServer[] | null>(null);
  /** CLI-suggested next prompt (claude prompt_suggestion) — composer ghost
   *  chip; cleared when the user sends anything. */
  promptSuggestion = $state<string | null>(null);
  /** The agent's live background tasks (the `background_tasks` level-set) —
   *  the background tray. Survives turn ends (background work is cross-turn);
   *  dies with the driver process (cleared on init/exited: the tasks are the
   *  CLI's children). */
  backgroundTasks = $state<BackgroundTask[]>([]);

  /** Extended-thinking preference (claude). NOT journal-derived — the CLI has
   *  no read-back — but kept HERE (pooled per session, surviving a ChatView
   *  tab remount) rather than in the view, so switching tabs can neither reset
   *  the chip nor override a choice the user already made. `null` = the user
   *  hasn't chosen, so the default (on for claude) applies; a bool is explicit.
   *  The view reads the EFFECTIVE value, toggles it, and pushes it to the live
   *  driver — it never re-forces a value, so a toggle-off always sticks. */
  thinkingEnabled = $state<boolean | null>(null);
  /** Whether the current preference has been pushed to the CURRENT driver
   *  process. Reset on every `init` (a fresh process — respawn/resume/rewind/
   *  view-toggle round-trip — starts with thinking OFF), so the view re-syncs
   *  it to that driver; the view only marks it once the `set_thinking` frame
   *  actually left the socket, so an undelivered push isn't silently lost. A
   *  plain reconnect (same process, no new `init`) keeps it, so it isn't
   *  re-pushed needlessly. */
  thinkingPushed = $state(false);
  setThinking(enabled: boolean): void {
    this.thinkingEnabled = enabled;
  }
  markThinkingPushed(): void {
    this.thinkingPushed = true;
  }
  /** A preference change missed the socket. Re-arm the reconnect effect so
   *  the pooled UI state cannot claim a thinking setting the driver never
   *  received. */
  markThinkingPending(): void {
    this.thinkingPushed = false;
  }

  /** Queued/undelivered user messages, in order — rendered in a holding stack
   *  at the scrollable transcript tail, NEVER inserted in `blocks`: a queued
   *  message is one you've typed and are waiting on. This is its OWN
   *  list, not a slice of `blocks`, so a queued send can't splice into a
   *  running turn's output. A `user_message_update{sent}` moves the entry into
   *  `blocks` at the current end (the reducer, so replay agrees); `cancelled`
   *  removes it; `dropped` marks it "not delivered" and it stays here. */
  pendingSends = $state<PendingSend[]>([]);

  /** Highest seq applied; the reconnect auth carries it for gap replay.
   *  Reactive so views can track "any event applied" (in-place chunk appends
   *  and tool patches change no collection lengths). */
  lastSeq = $state(0);
  /** True only while a fresh/reset store is folding its initial journal gap.
   *  The reducer stays authoritative, but the view waits for the advertised
   *  head before mounting transcript DOM so replay cannot visibly paint from
   *  the oldest message down or eagerly load every historical artifact. */
  hydrating = $state(true);
  private replayHead = 0;
  /** tool_call id -> index into blocks, for in-place status/content patches. */
  private toolIndex = new Map<string, number>();
  /** user-message delivery id -> index into blocks (user_message_update). */
  private userIndex = new Map<string, number>();
  /** question request_id -> index into blocks, for the resolution fold. */
  private questionIndex = new Map<string, number>();

  onReady(session: ChatSessionInfo, _replayFrom: number, head: number | undefined): void {
    this.connected = true;
    // The journal's head is below our own lastSeq: it was pruned/recreated and
    // numbering restarted, so every replayed and live event would be dropped by
    // the seq-dedupe guard, freezing the pane. Rebuild from the new journal.
    if (head !== undefined && head < this.lastSeq) {
      this.resetTranscript();
    }
    if (head !== undefined) {
      // Preserve an interrupted initial hydration across reconnects. An
      // ordinary reconnect with an already-rendered store never hides it.
      const initial = this.hydrating || this.lastSeq === 0;
      this.replayHead = head;
      this.hydrating = initial && this.lastSeq < head;
    } else {
      // Compatibility with a server that predates the additive head field:
      // it cannot give us a hydration boundary, so preserve the old live fold.
      this.hydrating = false;
    }
    if (session.model !== null) this.model = session.model;
    if (session.current_mode !== null) this.currentMode = session.current_mode;
    if (!session.alive && this.exited === null) {
      this.exited = { status: session.exit_status };
    }
  }

  /** The socket dropped; we are no longer live until the next `ready`. */
  onDisconnected(): void {
    this.connected = false;
  }

  /** The structured driver fell back to its terminal surface. */
  onDegraded(): void {
    this.hydrating = false;
    this.degraded = true;
  }

  /** The driver closed before (or after) an initial journal replay. */
  onExited(status: number | null): void {
    this.hydrating = false;
    this.exited = { status };
  }

  /** A socket/handshake failure can precede `ready`; reveal it immediately. */
  onFatalError(message: string): void {
    this.hydrating = false;
    this.fatalError = message;
  }

  /** Drop the rendered transcript and seq cursor so a fresh replay rebuilds it
   *  (a server-side journal reset — see {@link onReady}). */
  private resetTranscript(): void {
    this.blocks = [];
    this.toolIndex.clear();
    this.userIndex.clear();
    this.questionIndex.clear();
    // Pending asks and sends belong to the journal being rebuilt; the fresh
    // replay re-delivers any that are still live.
    this.pending = [];
    this.pendingSends = [];
    this.questions = [];
    this.lastSeq = 0;
    this.hydrating = false;
    this.replayHead = 0;
    this.exited = null;
    this.degraded = false;
    // Turn state and the plan belong to the dead journal too — a stale plan
    // (or a stuck "running") must not outlive the reset; the replay rebuilds
    // whatever is genuinely current.
    this.plan = [];
    this.backgroundTasks = [];
    this.running = false;
    this.compacting = false;
    this.activity = null;
    // The rebuilt replay re-drives the driver's `init`, but reset here too so
    // the preference is re-pushed even if this reset races ahead of it.
    this.thinkingPushed = false;
  }

  apply(entry: SeqEvent): void {
    if (entry.seq <= this.lastSeq) return;
    this.lastSeq = entry.seq;
    if (this.hydrating && this.lastSeq >= this.replayHead) this.hydrating = false;
    const ev = entry.ev;
    switch (ev.type) {
      case "init": {
        // A fresh driver handshake: the session is live again whatever a
        // replayed exit said (toggle round-trips, resumes).
        this.exited = null;
        this.degraded = false;
        // This is a NEW driver process — the CLI defaults thinking OFF, so the
        // view must re-push the user's preference to it (seq-dedupe means only
        // a genuinely-new init re-applies here; a plain reconnect doesn't).
        this.thinkingPushed = false;
        // A new driver process cannot own work launched by the previous one.
        // Turn-end reconciliation deliberately preserves cross-turn Codex
        // agent rows, so close any that survived in the reused journal before
        // this process starts contributing events. Scan across turn markers:
        // the stale row may precede its parent's completed-turn boundary.
        this.reconcileOpenTools();
        // Deliberately NOT clearing backgroundTasks here: the manager journals
        // an empty level-set immediately before every new driver's Init, and
        // that event owns the process-boundary reset. Runtime model changes use
        // ModelSwitched and leave the live set alone; exit/fatal paths below
        // also clear it, so replay agrees without this case guessing.
        // Any ask still pending predates this driver process — its reply
        // route died with the old one, so an answer could never land. Seq
        // ordering makes this safe: a live driver's Init is journaled BEFORE
        // its asks, so only stale ones are cleared. (A still-parked claude
        // prompt is re-delivered as a fresh request right after this Init.)
        this.expirePendingAsks();
        // Init is a complete catalog snapshot. Optional/empty serde fields are
        // omitted on the wire, so absence must CLEAR prior process state — a
        // resumed agent with no commands/models must not inherit stale rows.
        this.model = typeof ev.model === "string" ? ev.model : null;
        this.currentMode = typeof ev.current_mode === "string" ? ev.current_mode : null;
        this.modes = Array.isArray(ev.modes) ? (ev.modes as ModeInfo[]) : [];
        this.slashCommands = Array.isArray(ev.slash_commands)
          ? (ev.slash_commands as SlashCommand[])
          : [];
        this.models = Array.isArray(ev.models)
          ? (ev.models as Record<string, unknown>[]).map((m) => ({
            id: m.id as string,
            label: (m.label as string) ?? (m.id as string),
            description: (m.description as string) ?? null,
            resolved: (m.resolved as string) ?? null,
            efforts: (m.efforts as string[]) ?? [],
            defaultEffort: (m.default_effort as string) ?? null,
          }))
          : [];
        break;
      }
      case "user_message": {
        const id = (ev.id as string) ?? null;
        const text = ev.text as string;
        const attachments = (ev.attachments as number) ?? 0;
        if (ev.queued === true && id !== null) {
          // Queued: park it in the pending stack, NOT in the transcript at its
          // mid-turn send position (that splice would split the agent's live
          // message in two). It enters `blocks` only once delivery resolves.
          this.pendingSends.push({ id, text, attachments, checkpoint: null, state: "queued" });
        } else {
          // A fresh (turn-opening) send, or a permission-feedback echo — it was
          // received, so it goes straight into history.
          this.blocks.push({
            kind: "user",
            text,
            attachments,
            checkpoint: null,
            id,
            forkSeq: entry.seq,
          });
          if (id !== null) this.userIndex.set(id, this.blocks.length - 1);
        }
        this.promptSuggestion = null;
        break;
      }
      case "background_tasks": {
        // LEVEL-SET: the event carries the whole set — replace, never patch,
        // so replay's final state is simply the last event seen. An id-less
        // entry (a corrupt journal line) is dropped rather than fed to the
        // tray's keyed render, where a duplicate/undefined key throws.
        this.backgroundTasks = ((ev.tasks as Record<string, unknown>[]) ?? [])
          .map((t) => ({
            id: (t.id as string) ?? "",
            taskType: (t.task_type as string) ?? "",
            description: (t.description as string) ?? "",
            status: (t.status as string) ?? "running",
            startedAtMs: (t.started_at_ms as number) ?? 0,
            workflowName: (t.workflow_name as string) ?? null,
            // Same keyed-render defense as the task ids above, one level
            // down: the dot row is keyed by agent.index, and a duplicate
            // (corrupt line, a journal from an older build) would throw.
            agents: ((t.agents as Record<string, unknown>[]) ?? [])
              .map((a) => ({
                index: (a.index as number) ?? 0,
                label: (a.label as string) ?? "",
                state: (a.state as string) ?? "start",
                resultPreview: (a.result_preview as string) ?? null,
              }))
              .filter((a, i, arr) => arr.findIndex((b) => b.index === a.index) === i),
            agentsTotal: (t.agents_total as number) ?? 0,
            agentsDone: (t.agents_done as number) ?? 0,
          }))
          .filter((t) => t.id !== "");
        // Tasks that left the set WITH a verdict fold into history as quiet
        // notices — completion is transcript-worthy; a set change alone is
        // not. A summary that names the verdict is the CLI's own full
        // sentence ('Background command "…" completed (exit code 0)') —
        // render it alone rather than saying everything twice. Matching on
        // the status word (not the description) keeps that working when the
        // driver truncated a long description. A summary that merely echoes
        // the description (a stop's shape, on pre-fix journals — the driver
        // drops the echo at construction now) adds nothing and is dropped.
        for (const c of (ev.closed as Record<string, unknown>[]) ?? []) {
          const desc = (c.description as string) ?? "background task";
          const status = (c.status as string) ?? "completed";
          const summary = (c.summary as string) ?? "";
          const selfContained =
            summary !== "" && summary.toLowerCase().includes(status.toLowerCase());
          this.notice(
            selfContained
              ? summary
              : `background “${desc}” ${status}${summary !== "" && summary !== desc ? ` — ${summary}` : ""}`,
            status === "failed" ? "error" : "info",
          );
        }
        break;
      }
      case "user_message_update": {
        // Delivery resolution for a queued send. Driven purely by the reducer,
        // so live and replay build the identical transcript.
        const id = ev.id as string;
        const pIdx = this.pendingSends.findIndex((p) => p.id === id);
        if (pIdx === -1) break; // unknown / already resolved
        const pending = this.pendingSends[pIdx];
        const state = ev.state as string;
        if (state === "sent") {
          // Delivered: leave the pending stack and enter the transcript at the
          // CURRENT end — after the turn it was queued behind, never spliced
          // into it. appendText only inspects the tail, so a following agent
          // chunk starts a fresh block: the agent's message is never split.
          this.pendingSends.splice(pIdx, 1);
          this.blocks.push({
            kind: "user",
            text: pending.text,
            attachments: pending.attachments,
            checkpoint: pending.checkpoint,
            id: pending.id,
            forkSeq: entry.seq,
          });
          this.userIndex.set(pending.id, this.blocks.length - 1);
        } else if (state === "cancelled") {
          // Pulled back before the agent saw it — it never happened. Vanish
          // entirely (it was never in `blocks`), so nothing to clean up there.
          this.pendingSends.splice(pIdx, 1);
        } else {
          // dropped: the agent never got it — keep it visible as "not
          // delivered" so the text can be copied and re-sent.
          pending.state = "dropped";
        }
        break;
      }
      case "prompt_suggestion":
        this.promptSuggestion = ev.text as string;
        break;
      case "checkpoint": {
        // Stamps the user message it belongs to, matched by uuid (the driver
        // emits the checkpoint right after that message's echo, carrying its
        // id). A queued send is still in the pending stack at this point, so
        // check there first; the anchor rides along when it promotes to a block.
        const umid = ev.user_message_id as string;
        const cp: CheckpointRef = { id: umid, preceding: (ev.preceding_uuid as string) ?? null };
        const p = this.pendingSends.find((s) => s.id === umid);
        if (p !== undefined) {
          p.checkpoint = cp;
          break;
        }
        const idx = this.userIndex.get(umid);
        if (idx !== undefined) {
          const block = this.blocks[idx];
          if (block.kind === "user") {
            block.checkpoint = cp;
            block.forkSeq = entry.seq;
            break;
          }
        }
        // Fallback for pre-id journals (the user echo carried no id to match):
        // stamp the last delivered user block, the message this followed.
        for (let i = this.blocks.length - 1; i >= 0; i--) {
          const block = this.blocks[i];
          if (block.kind === "user") {
            block.checkpoint = cp;
            block.forkSeq = entry.seq;
            break;
          }
        }
        break;
      }
      case "turn_started":
        this.running = true;
        this.activity = { kind: "waiting", detail: "starting" };
        break;
      case "message_chunk":
        this.appendText("message", ev, entry.seq, entry.ts);
        this.activity = { kind: "writing", detail: "" };
        break;
      case "thought_chunk":
        this.appendText("thought", ev, entry.seq, entry.ts);
        this.activity = { kind: "thinking", detail: "" };
        break;
      case "thinking_tokens": {
        // Fires even when no thought text streams (summarized display) —
        // the status row shows a live token estimate instead of "starting".
        const tokens = (ev.tokens as number) ?? 0;
        this.activity = {
          kind: "thinking",
          detail: tokens >= 1000 ? `~${(tokens / 1000).toFixed(1)}k tokens` : `~${tokens} tokens`,
        };
        break;
      }
      case "tool_call": {
        // Drivers may re-emit a call to enrich it (e.g. an image's saved
        // path arrives at completion) — upsert by id, never duplicate.
        const existing = this.toolIndex.get(ev.id as string);
        const row = existing !== undefined ? this.blocks[existing] : undefined;
        if (row !== undefined && row.kind === "tool") {
          row.title = ev.title as string;
          row.locations = (ev.locations as string[]) ?? [];
          // A late enriching re-emit must never walk a finished tool back to
          // pending/in_progress — the authoritative result already landed.
          if (row.status !== "completed" && row.status !== "failed") {
            row.status = ev.status as "pending" | "in_progress";
          }
          if (ev.cross_turn === true) row.crossTurn = true;
        } else {
          this.blocks.push({
            kind: "tool",
            id: ev.id as string,
            tool: ev.kind as string,
            title: ev.title as string,
            locations: (ev.locations as string[]) ?? [],
            status: ev.status as "pending" | "in_progress",
            content: null,
            denied: false,
            allowed: false,
            streaming: false,
            crossTurn: ev.cross_turn === true,
          });
          this.toolIndex.set(ev.id as string, this.blocks.length - 1);
        }
        this.activity = { kind: "tool", detail: ev.title as string };
        break;
      }
      case "tool_call_update": {
        const idx = this.toolIndex.get(ev.id as string);
        if (idx === undefined) break;
        const block = this.blocks[idx];
        if (block.kind !== "tool") break;
        // A late in_progress update (e.g. straggling subagent progress) must
        // never walk a finished tool back to running — mirror the tool_call
        // re-emit guard above. Content still applies below.
        const status = ev.status as "completed" | "failed" | "in_progress";
        const wasTerminal = block.status === "completed" || block.status === "failed";
        if (status !== "in_progress" || !wasTerminal) {
          block.status = status;
        }
        const content = ev.content as ToolContent | null | undefined;
        // An Edit card's diff (from tool inputs) must not be clobbered by a
        // later status-only update.
        if (content != null) {
          block.content = content;
          block.streaming = false;
        }
        // A tool that JUST finished hands the floor back to the model: the
        // status row returns to "working" until the next delta names a
        // phase. Gated on the transition — updates to an already-terminal
        // card (a background workflow's "N/M agents done" ticks and its
        // close verdict, landing on the long-completed launch card) must
        // not flick the live activity of an unrelated running turn.
        if (
          this.running &&
          !wasTerminal &&
          (block.status === "completed" || block.status === "failed") &&
          this.activity?.kind === "tool"
        ) {
          this.activity = { kind: "waiting", detail: "" };
        }
        break;
      }
      case "tool_output_delta": {
        // Live output ahead of the authoritative result (which replaces it).
        const idx = this.toolIndex.get(ev.id as string);
        if (idx === undefined) break;
        const block = this.blocks[idx];
        if (block.kind !== "tool") break;
        const text = ev.text as string;
        if (block.content !== null && block.content.kind === "output" && block.streaming) {
          block.content.text = (block.content.text ?? "") + text;
        } else {
          block.content = { kind: "output", text };
          block.streaming = true;
        }
        break;
      }
      case "plan":
        // LEVEL-SET, like background_tasks: every Plan carries the whole list,
        // so replace. Mapped field-by-field (not cast) because the rich fields
        // are omitted entirely by older journals, TodoWrite, and codex — the
        // panel reads `blockedBy.length`, which must never be undefined.
        this.plan = ((ev.entries as Record<string, unknown>[]) ?? [])
          .map((e) => ({
            content: (e.content as string) ?? "",
            status: (e.status as PlanEntry["status"]) ?? "todo",
            id: (e.id as string) ?? null,
            activeForm: (e.active_form as string) ?? null,
            description: (e.description as string) ?? null,
            owner: (e.owner as string) ?? null,
            blockedBy: (e.blocked_by as string[]) ?? [],
          }))
          // Same keyed-render defense as backgroundTasks: both the plan panel
          // and the dashboard card key rows by `id`, and a duplicate (a
          // reworded TaskList line parsed twice, a journal from another build)
          // would throw and take the whole view down.
          .filter((e, i, arr) => e.id === null || arr.findIndex((o) => o.id === e.id) === i);
        break;
      case "permission_request":
        this.pending.push({
          requestId: ev.request_id as string,
          toolCallId: (ev.tool_call_id as string) ?? null,
          title: ev.title as string,
          options: (ev.options as PermissionOption[]) ?? [],
          inputPreview: ev.input_preview,
          plan: (ev.plan as string) ?? null,
        });
        break;
      case "question_request": {
        const requestId = ev.request_id as string;
        const questions = ((ev.questions as Record<string, unknown>[]) ?? []).map((q) => ({
          id: q.id as string,
          header: (q.header as string) ?? "",
          question: (q.question as string) ?? "",
          options: ((q.options as Record<string, unknown>[]) ?? []).map((o) => ({
            label: o.label as string,
            description: (o.description as string) ?? "",
          })),
          multiSelect: q.multi_select === true,
        }));
        // Twice: the overlay is the answerable card, the block is the
        // transcript's memory of it (invisible while pending, an answered
        // card once resolved) — so an answered question never just vanishes.
        const expiresAtMs =
          typeof ev.expires_at_ms === "number" && Number.isFinite(ev.expires_at_ms)
            ? ev.expires_at_ms
            : null;
        this.questions.push({ requestId, questions, expiresAtMs });
        this.blocks.push({ kind: "question", id: requestId, questions, answers: {}, resolved: false });
        this.questionIndex.set(requestId, this.blocks.length - 1);
        break;
      }
      case "question_resolved": {
        const requestId = ev.request_id as string;
        this.questions = this.questions.filter((q) => q.requestId !== requestId);
        const idx = this.questionIndex.get(requestId);
        const block = idx !== undefined ? this.blocks[idx] : undefined;
        if (block !== undefined && block.kind === "question") {
          block.resolved = true;
          block.answers = (ev.answers as Record<string, string[]>) ?? {};
        }
        break;
      }
      case "permission_resolved": {
        const req = this.pending.find((p) => p.requestId === ev.request_id);
        this.pending = this.pending.filter((p) => p.requestId !== ev.request_id);
        const option = ev.option_id as string;
        // Allow vs deny reads the resolved option's ACP KIND, never the raw id:
        // claude allows are `allow_*` but codex allows are `accept*`, so the old
        // id-prefix check silently marked every ALLOWED codex command "denied"
        // (→ the group counted it "1 command failed"). `cancelled`/`expired` are
        // synthetic ids with no option entry — fall back to the id check, which
        // keeps their prior "not allowed" (denied) treatment unchanged.
        const kind = req?.options.find((o) => o.id === option)?.kind ?? null;
        const allowed = kind !== null ? kind.startsWith("allow") : option.startsWith("allow");
        if (req !== undefined) {
          // Fold the decision into history as a quiet row — the card itself
          // is overlay-only, and "what did I allow?" must survive a reload.
          const label =
            req.options.find((o) => o.id === option)?.label ??
            (option === "cancelled" || option === "expired" ? "no longer active" : option);
          this.notice(`${req.title} — ${label}`, "info");
        }
        if (req?.toolCallId != null) {
          const idx = this.toolIndex.get(req.toolCallId);
          const block = idx !== undefined ? this.blocks[idx] : undefined;
          if (block !== undefined && block.kind === "tool") {
            if (allowed) block.allowed = true;
            else block.denied = true;
          }
        }
        break;
      }
      case "mode_changed":
        this.currentMode = ev.mode_id as string;
        break;
      case "effort_state":
        this.effort = (ev.effort as string) ?? null;
        this.ultracode = ev.ultracode === true;
        break;
      case "context_usage": {
        this.contextPct = ev.percentage as number;
        this.contextTokens = {
          total: (ev.total_tokens as number) ?? 0,
          max: (ev.max_tokens as number) ?? 0,
        };
        break;
      }
      case "context_compaction": {
        const phase = ev.phase as string;
        if (phase === "started") {
          this.compacting = true;
        } else {
          this.compacting = false;
          if (phase === "completed") {
            const pre = ev.pre_tokens as number | undefined;
            this.notice(
              pre !== undefined
                ? `context compacted · ${pre.toLocaleString()} tokens summarized`
                : "context compacted",
              "info",
            );
          }
          // The agent's normal turn output owns failure details. The lifecycle
          // event only settles progress, avoiding a duplicate Claude message.
        }
        break;
      }
      case "usage_report":
        this.blocks.push({ kind: "usage", windows: (ev.windows as UsageWindow[]) ?? [] });
        break;
      case "rate_limit":
        this.rateLimit = {
          utilization: ev.utilization as number,
          label: (ev.label as string) ?? null,
          resetsAt: (ev.resets_at as string) ?? null,
          limitReached: ev.limit_reached === true,
        };
        break;
      case "model_switched": {
        // The serving model changed under us (safety reroute, Fable credit
        // fallback): the chip follows the truth, and a retracting switch
        // withdraws the current turn's trailing prose before the retry.
        this.model = ev.to as string;
        if (ev.retract_current_turn === true) this.dropTrailingProse();
        break;
      }
      case "messages_superseded":
        this.dropTrailingProse();
        break;
      case "rewind_result":
        this.rewind = {
          userMessageId: ev.user_message_id as string,
          canRewind: ev.can_rewind === true,
          filesChanged: (ev.files_changed as string[]) ?? [],
          applied: ev.applied === true,
          error: (ev.error as string) ?? null,
        };
        break;
      case "mcp_servers":
        this.mcpServers = ((ev.servers as Record<string, unknown>[]) ?? []).map((s) => ({
          name: s.name as string,
          status: (s.status as string) ?? "unknown",
          tools: (s.tools as number) ?? 0,
          error: (s.error as string) ?? null,
        }));
        break;
      case "notice":
        this.notice(ev.text as string, "info");
        break;
      case "turn_completed": {
        this.running = false;
        this.compacting = false;
        this.activity = null;
        // Close any tool row this turn left dangling (a dropped result frame)
        // BEFORE the turn_end block lands, so the scan stops at the previous
        // boundary and the end-of-turn artifact scan sees their final state.
        this.reconcileOpenTools(true);
        // Codex's native thread/fork boundary is a COMPLETED turn id. Only
        // the final assistant block in the turn can claim that exact point;
        // earlier prose segments separated by tools still use the portable
        // normalized handoff.
        for (let i = this.blocks.length - 1; i >= 0; i--) {
          const block = this.blocks[i];
          if (block.kind === "message" && block.turnId === (ev.turn_id as string)) {
            block.forkSeq = entry.seq;
            block.nativeTurnComplete = true;
            break;
          }
        }
        const usage = ev.usage as {
          cost_usd?: number;
          output_tokens?: number;
          duration_ms?: number;
        };
        this.blocks.push({
          kind: "turn_end",
          costUsd: usage.cost_usd ?? null,
          outputTokens: usage.output_tokens ?? 0,
          durationMs: usage.duration_ms ?? 0,
          artifacts: this.collectTurnArtifacts(),
        });
        break;
      }
      case "turn_aborted": {
        this.running = false;
        this.compacting = false;
        this.activity = null;
        this.reconcileOpenTools(true);
        // A deliberate stop (Esc / stop chip) is not an error state: the
        // wire's `interrupted` flag is the drivers' structural signal
        // (claude's free-text result string never reliably said so); the
        // reason regex survives only for pre-flag journal replays. Render a
        // calm muted "stopped", keep --err for real failures. Both drivers'
        // fallback reason is literally "turn failed", so don't prefix it
        // into "turn failed: turn failed".
        const reason = ev.reason as string;
        if (ev.interrupted === true || /interrupt/i.test(reason)) {
          this.notice("stopped", "info");
        } else {
          this.notice(reason === "turn failed" ? "turn failed" : `turn failed: ${reason}`, "error");
        }
        break;
      }
      case "truncated":
        this.notice("earlier history was trimmed (the agent's own transcript keeps it)", "info");
        break;
      case "mode_switch":
        this.notice(ev.to === "term" ? "continued in terminal" : "continued in chat", "info");
        // The process death right before this marker was the toggle's
        // mechanism, not a conversation ending.
        this.exited = null;
        this.degraded = false;
        break;
      case "forked": {
        const native = ev.native === true;
        // The copied prefix is transcript history, not live work owned by the
        // new process. Close transient rows before its Init/first turn arrives.
        this.running = false;
        this.activity = null;
        this.reconcileOpenTools();
        this.expirePendingAsks();
        this.pendingSends = [];
        this.backgroundTasks = [];
        // A portable target received the old conversation as one hidden
        // primer. Its copied source UUIDs/turn ids do NOT exist in the fresh
        // native session, so they must remain display-only. The server applies
        // the same provenance floor independently.
        if (!native) {
          for (const block of this.blocks) {
            if (block.kind === "user") block.checkpoint = null;
            if (block.kind === "message") block.nativeTurnComplete = false;
          }
          // Everything above was replayed from a DIFFERENT process (and may
          // be a different vendor). Keep transcript blocks, but never let its
          // model catalog, limits, context meter, controls, or error state
          // masquerade as destination telemetry while the target initializes.
          this.model = null;
          this.modes = [];
          this.currentMode = null;
          this.slashCommands = [];
          this.models = [];
          this.effort = null;
          this.ultracode = false;
          this.contextPct = null;
          this.contextTokens = null;
          this.rateLimit = null;
          this.rewind = null;
          this.mcpServers = null;
          this.promptSuggestion = null;
          this.fatalError = null;
          this.plan = [];
          this.exited = null;
          this.degraded = false;
        }
        const source =
          ev.source_agent === "claude"
            ? "Claude Code"
            : ev.source_agent === "codex"
              ? "Codex"
              : String(ev.source_agent ?? "agent");
        this.notice(
          `${native ? "native" : "portable"} fork from ${source} · source session unchanged`,
          "info",
        );
        break;
      }
      case "error":
        this.notice(ev.message as string, "error");
        if (ev.fatal === true) {
          this.fatalError = ev.message as string;
          // A dead driver is not running — don't strand the stop button and
          // the "starting…" row waiting on a turn end that can't come.
          this.running = false;
          this.compacting = false;
          this.activity = null;
          // A fatal error is a terminal path like the others — a tool left
          // in_progress must not spin forever when no `exited` follows (a kept-
          // visible ProtocolError session emits no exit). Background tasks
          // died with the process too.
          this.reconcileOpenTools();
          this.backgroundTasks = [];
        }
        break;
      case "exited":
        this.running = false;
        this.compacting = false;
        this.activity = null;
        this.reconcileOpenTools();
        this.exited = { status: (ev.status as number | null) ?? null };
        // Background tasks are the CLI's children — they died with it (the
        // CLI SIGTERMs its tracked shells on exit).
        this.backgroundTasks = [];
        // The reply route for any pending ask died with the process. The
        // driver drains resolutions before Exited, so this is usually a
        // no-op — it covers old journals recorded before that fix.
        this.expirePendingAsks();
        break;
      default:
        break;
    }
    this.trimBlocks();
  }

  /** Client-side reducer cap. The daemon journal compacts its own history;
   *  this in-memory `blocks` array needs its own bound even though ChatView now
   *  mounts only a paged DOM window. Beyond a generous cap we drop the oldest
   *  blocks behind one "earlier history trimmed" notice, mirroring server
   *  compaction. The cap is far above the live tail, so the streaming message
   *  and its tool cards are never touched. */
  private static readonly BLOCK_CAP = 2000;
  private trimBlocks(): void {
    const cap = ChatStore.BLOCK_CAP;
    if (this.blocks.length <= cap) return;
    // Drop the oldest overflow plus one more, and land a single leading notice
    // in their place (an earlier trim's notice is among the dropped, so it
    // never stacks). Result length settles at exactly the cap.
    const drop = this.blocks.length - cap + 1;
    const notice: ChatBlock = { kind: "notice", text: TRIM_NOTICE, tone: "info" };
    this.blocks.splice(0, drop, notice);
    // The front-splice invalidated every id→index position — rebuild them all.
    this.rebuildIndexes();
  }

  private appendText(
    kind: "message" | "thought",
    ev: AgentEvent,
    seq: number,
    timestampMs: number,
  ): void {
    const text = ev.text as string;
    const turnId = ev.turn_id as string;
    const last = this.blocks[this.blocks.length - 1];
    if (last !== undefined && last.kind === kind && last.turnId === turnId) {
      last.text += text;
      if (last.kind === "message") last.forkSeq = seq;
      return;
    }
    if (kind === "message") {
      this.blocks.push({
        kind,
        text,
        turnId,
        sentAtMs: timestampMs,
        forkSeq: seq,
        nativeTurnComplete: false,
      });
    } else {
      this.blocks.push({ kind, text, turnId });
    }
  }

  /** Also the client's own channel for local notices (usage summaries,
   *  intercepted commands) — they are NOT journaled, deliberately. */
  notice(text: string, tone: "info" | "error"): void {
    this.blocks.push({ kind: "notice", text, tone });
    // Local notices do not pass through `apply`, whose epilogue normally
    // enforces the transcript cap. Repeated disconnected-action feedback must
    // not become an unbounded side channel around that cap.
    this.trimBlocks();
  }

  /** Withdraw every pending ask whose reply route is gone (driver exit or a
   *  fresh handshake): the cards leave the overlay, and their history blocks
   *  fold to a quiet "no longer active" so the user sees WHY they vanished.
   *  Deterministic from journaled events (init/exited), so replay agrees. */
  private expirePendingAsks(): void {
    for (const q of this.questions) {
      const idx = this.questionIndex.get(q.requestId);
      const block = idx !== undefined ? this.blocks[idx] : undefined;
      if (block !== undefined && block.kind === "question") block.resolved = true;
    }
    this.questions = [];
    for (const p of this.pending) {
      this.notice(`${p.title} — no longer active`, "info");
    }
    this.pending = [];
  }

  /** A turn (or the session) has ended, so reconcile any ordinary tool row
   *  still `in_progress`/`pending` to a terminal state. Cross-turn rows are
   *  preserved at TURN boundaries: Codex delegated threads may outlive the
   *  parent answer and keep streaming progress. Session/process boundaries
   *  still close them. An ordinary dangling row most often means
   *  the result frame was too large to parse and was dropped BELOW the event
   *  layer (a big image `Read` blows the transport's per-line byte cap, so its
   *  `tool_result` never reaches the driver), and nothing else ever closes it.
   *  Left alone it spins "running…" forever and keeps its ToolGroup from
   *  collapsing — the phantom the user sees after the turn is plainly over.
   *  Scans back only over the just-ended turn (stopping at the previous
   *  `turn_end`), and is a pure reducer over `blocks`, so replay rebuilds the
   *  identical transcript. Marks `completed`, not `failed`: the tool most
   *  likely DID finish (we simply never captured its output), and inventing a
   *  red failure would be the louder lie. */
  private reconcileOpenTools(preserveCrossTurn = false): void {
    for (let i = this.blocks.length - 1; i >= 0; i--) {
      const b = this.blocks[i];
      if (b.kind === "turn_end") {
        // Turn reconciliation is local. Process reconciliation crosses old
        // turn markers to find any surviving cross-turn agent rows.
        if (preserveCrossTurn) break;
        continue;
      }
      if (
        b.kind === "tool" &&
        (!preserveCrossTurn || !b.crossTurn) &&
        (b.status === "in_progress" || b.status === "pending")
      ) {
        b.status = "completed";
      }
    }
  }

  /** The previewable files THIS turn produced, for the end-of-turn gallery.
   *  Scans back to the turn boundary (previous user message / turn_end) and
   *  keeps previewable locations from writes (edit kind) plus any image a
   *  tool touched — a CSV the agent merely READ is not an artifact. Absolute
   *  paths from the tool itself, so the gallery is always openable regardless
   *  of how the prose spelled the name. */
  private collectTurnArtifacts(): string[] {
    const out: string[] = [];
    const seen = new Set<string>();
    for (let i = this.blocks.length - 1; i >= 0; i--) {
      const b = this.blocks[i];
      // Every user block here is delivered (queued sends live in pendingSends),
      // so a user block IS this turn's opening boundary — stop the scan.
      if (b.kind === "user" || b.kind === "turn_end") break;
      if (b.kind !== "tool" || b.status !== "completed" || b.denied) continue;
      for (const loc of b.locations) {
        if (seen.has(loc) || !canInlinePreview(loc)) continue;
        if (b.tool === "edit" || isImagePath(loc)) {
          seen.add(loc);
          out.push(loc);
        }
      }
    }
    out.reverse(); // chronological
    return out.slice(0, 8);
  }

  /** Rebuild every id→index map from `blocks` after a non-tail splice
   *  invalidated positions. */
  private rebuildIndexes(): void {
    this.toolIndex.clear();
    this.userIndex.clear();
    this.questionIndex.clear();
    this.blocks.forEach((b, i) => {
      if (b.kind === "tool") this.toolIndex.set(b.id, i);
      if (b.kind === "user" && b.id !== null) this.userIndex.set(b.id, i);
      if (b.kind === "question") this.questionIndex.set(b.id, i);
    });
  }

  /** Withdraw the current turn's trailing agent prose (refusal retries and
   *  superseding messages REPLACE it). Tool cards and user messages stay. Only
   *  delivered user messages live in `blocks` now (queued sends are in the
   *  pending stack), so the trailing prose run is always at the very tail — a
   *  plain tail splice. A codex steer that resolved `sent` mid-turn is a real
   *  boundary and correctly stops the scan. A non-tail splice → rebuild. */
  private dropTrailingProse(): void {
    const end = this.blocks.length;
    let start = end;
    while (start > 0) {
      const kind = this.blocks[start - 1].kind;
      if (kind !== "message" && kind !== "thought") break;
      start--;
    }
    if (start < end) {
      this.blocks.splice(start, end - start);
      if (end < this.blocks.length) this.rebuildIndexes();
    }
  }
}
