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

export interface PlanEntry {
  content: string;
  status: "todo" | "in_progress" | "done";
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
}

export interface CheckpointRef {
  /** The user message's own uuid — the rewind_files key. */
  id: string;
  /** The message preceding it — the conversation-fork resume point. */
  preceding: string | null;
}

export type ChatBlock =
  | { kind: "user"; text: string; attachments: number; checkpoint: CheckpointRef | null }
  | { kind: "message"; text: string; turnId: string }
  | { kind: "thought"; text: string; turnId: string }
  | {
      kind: "tool";
      id: string;
      tool: string;
      title: string;
      locations: string[];
      status: "pending" | "in_progress" | "completed" | "failed";
      content: ToolContent | null;
      denied: boolean;
      /** Live output streamed ahead of the authoritative result. */
      streaming: boolean;
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

export interface ModeInfo {
  id: string;
  label: string;
}

export interface SlashCommand {
  name: string;
  description?: string;
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

  /** Highest seq applied; the reconnect auth carries it for gap replay.
   *  Reactive so views can track "any event applied" (in-place chunk appends
   *  and tool patches change no collection lengths). */
  lastSeq = $state(0);
  /** tool_call id -> index into blocks, for in-place status/content patches. */
  private toolIndex = new Map<string, number>();

  onReady(session: ChatSessionInfo, _replayFrom: number, head: number | undefined): void {
    this.connected = true;
    // The journal's head is below our own lastSeq: it was pruned/recreated and
    // numbering restarted, so every replayed and live event would be dropped by
    // the seq-dedupe guard, freezing the pane. Rebuild from the new journal.
    if (head !== undefined && head < this.lastSeq) {
      this.resetTranscript();
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

  /** Drop the rendered transcript and seq cursor so a fresh replay rebuilds it
   *  (a server-side journal reset — see {@link onReady}). */
  private resetTranscript(): void {
    this.blocks = [];
    this.toolIndex.clear();
    this.lastSeq = 0;
    this.exited = null;
    this.degraded = false;
  }

  apply(entry: SeqEvent): void {
    if (entry.seq <= this.lastSeq) return;
    this.lastSeq = entry.seq;
    const ev = entry.ev;
    switch (ev.type) {
      case "init": {
        // A fresh driver handshake: the session is live again whatever a
        // replayed exit said (toggle round-trips, resumes).
        this.exited = null;
        this.degraded = false;
        if (typeof ev.model === "string") this.model = ev.model;
        if (typeof ev.current_mode === "string") this.currentMode = ev.current_mode;
        if (Array.isArray(ev.modes)) this.modes = ev.modes as ModeInfo[];
        if (Array.isArray(ev.slash_commands)) {
          this.slashCommands = ev.slash_commands as SlashCommand[];
        }
        if (Array.isArray(ev.models)) {
          this.models = (ev.models as Record<string, unknown>[]).map((m) => ({
            id: m.id as string,
            label: (m.label as string) ?? (m.id as string),
            description: (m.description as string) ?? null,
            resolved: (m.resolved as string) ?? null,
            efforts: (m.efforts as string[]) ?? [],
            defaultEffort: (m.default_effort as string) ?? null,
          }));
        }
        break;
      }
      case "user_message":
        this.blocks.push({
          kind: "user",
          text: ev.text as string,
          attachments: (ev.attachments as number) ?? 0,
          checkpoint: null,
        });
        this.promptSuggestion = null;
        break;
      case "prompt_suggestion":
        this.promptSuggestion = ev.text as string;
        break;
      case "checkpoint": {
        // Stamps the user message it directly follows.
        for (let i = this.blocks.length - 1; i >= 0; i--) {
          const block = this.blocks[i];
          if (block.kind === "user") {
            block.checkpoint = {
              id: ev.user_message_id as string,
              preceding: (ev.preceding_uuid as string) ?? null,
            };
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
        this.appendText("message", ev);
        this.activity = { kind: "writing", detail: "" };
        break;
      case "thought_chunk":
        this.appendText("thought", ev);
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
            streaming: false,
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
        block.status = ev.status as "completed" | "failed" | "in_progress";
        const content = ev.content as ToolContent | null | undefined;
        // An Edit card's diff (from tool inputs) must not be clobbered by a
        // later status-only update.
        if (content != null) {
          block.content = content;
          block.streaming = false;
        }
        // A finished tool hands the floor back to the model: the status row
        // returns to "working" until the next delta names a phase.
        if (
          this.running &&
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
        this.plan = (ev.entries as PlanEntry[]) ?? [];
        break;
      case "permission_request":
        this.pending.push({
          requestId: ev.request_id as string,
          toolCallId: (ev.tool_call_id as string) ?? null,
          title: ev.title as string,
          options: (ev.options as PermissionOption[]) ?? [],
          inputPreview: ev.input_preview,
        });
        break;
      case "question_request":
        this.questions.push({
          requestId: ev.request_id as string,
          questions: ((ev.questions as Record<string, unknown>[]) ?? []).map((q) => ({
            id: q.id as string,
            header: (q.header as string) ?? "",
            question: (q.question as string) ?? "",
            options: ((q.options as Record<string, unknown>[]) ?? []).map((o) => ({
              label: o.label as string,
              description: (o.description as string) ?? "",
            })),
            multiSelect: q.multi_select === true,
          })),
        });
        break;
      case "question_resolved":
        this.questions = this.questions.filter((q) => q.requestId !== ev.request_id);
        break;
      case "permission_resolved": {
        const req = this.pending.find((p) => p.requestId === ev.request_id);
        this.pending = this.pending.filter((p) => p.requestId !== ev.request_id);
        const option = ev.option_id as string;
        if (req?.toolCallId != null && !option.startsWith("allow")) {
          const idx = this.toolIndex.get(req.toolCallId);
          const block = idx !== undefined ? this.blocks[idx] : undefined;
          if (block !== undefined && block.kind === "tool") block.denied = true;
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
        this.activity = null;
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
        this.activity = null;
        // A deliberate stop (Esc / stop button) is not an error state: codex
        // reports reason "interrupted", claude's result string contains
        // "interrupted by user" — render those quiet, keep --err for real
        // failures. Both drivers' fallback reason is literally "turn failed",
        // so don't prefix it into "turn stopped: turn failed".
        const reason = ev.reason as string;
        if (/interrupt/i.test(reason)) {
          this.notice("interrupted", "info");
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
      case "error":
        this.notice(ev.message as string, "error");
        if (ev.fatal === true) this.fatalError = ev.message as string;
        break;
      case "exited":
        this.running = false;
        this.activity = null;
        this.exited = { status: (ev.status as number | null) ?? null };
        break;
      default:
        break;
    }
    this.trimBlocks();
  }

  /** Client-side transcript cap. The daemon journal compacts its own history;
   *  the rendered `blocks` array has no such bound, so a very long session
   *  would grow it (and the DOM) without limit. Beyond a generous cap we drop
   *  the oldest blocks behind a single "earlier history trimmed" notice,
   *  mirroring the server's compaction. The cap is far above the live tail, so
   *  the streaming message and its tool cards are never touched. */
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
    // The front-splice invalidated every toolIndex position — rebuild it.
    this.toolIndex.clear();
    this.blocks.forEach((b, i) => {
      if (b.kind === "tool") this.toolIndex.set(b.id, i);
    });
  }

  private appendText(kind: "message" | "thought", ev: AgentEvent): void {
    const text = ev.text as string;
    const turnId = ev.turn_id as string;
    const last = this.blocks[this.blocks.length - 1];
    if (last !== undefined && last.kind === kind && last.turnId === turnId) {
      last.text += text;
      return;
    }
    this.blocks.push({ kind, text, turnId });
  }

  /** Also the client's own channel for local notices (usage summaries,
   *  intercepted commands) — they are NOT journaled, deliberately. */
  notice(text: string, tone: "info" | "error"): void {
    this.blocks.push({ kind: "notice", text, tone });
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

  /** Withdraw the current turn's trailing agent prose (refusal retries and
   *  superseding messages REPLACE it). Tool cards and user messages stay;
   *  removal is tail-only so toolIndex positions remain valid. */
  private dropTrailingProse(): void {
    let end = this.blocks.length;
    while (end > 0) {
      const kind = this.blocks[end - 1].kind;
      if (kind !== "message" && kind !== "thought") break;
      end--;
    }
    if (end < this.blocks.length) this.blocks.splice(end);
  }
}
