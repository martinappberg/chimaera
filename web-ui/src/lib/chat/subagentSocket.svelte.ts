import { api } from "../net/api";
import { acquireChat, provideSubagentTransport, releaseChat } from "./chatPool";
import type { ChatSessionInfo, ChatSocketHandlers, SeqEvent } from "./chatWs";
import { CooperativeQueue } from "./cooperativeQueue";
import { NativeUiTransport } from "./nativeUi";
import {
  EMPTY_CURSOR,
  advanceCursor,
  type SubagentCursor,
  subagentRunning,
  type SubagentRead,
  type SubagentRef,
} from "./subagentView";
import type { ChatStore } from "./store.svelte";

/** How often a watched, working subagent is re-read. Its events arrive a
 *  step at a time (a tool call, a reply), so this is the view's latency. */
const WATCH_MS = 2_500;
/** How often a finished subagent's view checks whether it started working
 *  again (the parent can send it a follow-up). A local check, no request. */
const IDLE_MS = 5_000;
/** A read is one local file or one RPC; past this it is abandoned so the
 *  chain re-arms (a hung fetch must not stall it — rules/web-ui.md). */
const READ_TIMEOUT_MS = 20_000;

type Delivery =
  | { kind: "restart"; model: string | null; total: number }
  | { kind: "event"; seq: number; ts: number; ev: SubagentRead["events"][number] };

/**
 * The chat-socket stand-in behind a subagent view: it feeds a ChatStore the
 * subagent's own conversation by polling the daemon's transcript route, and
 * accepts no commands (the view is read-only — a subagent answers to its
 * parent, not to the user).
 *
 * It re-reads only while somebody is looking (`setWatched`, plus document
 * visibility) and the subagent is working (`running`, from the parent
 * chat); a finished subagent gets one closing read and then costs nothing.
 * Reads are incremental: see `advanceCursor`.
 */
export class SubagentSocket {
  /** A subagent view hosts no Mods; the transport exists for ChatView's
   *  shape and never sends. */
  readonly nativeUi = new NativeUiTransport(() => false);
  /** Why there is nothing to show yet, in the daemon's words (the
   *  subagent has not written anything, the parent chat is gone). */
  problem = $state<string | null>(null);
  /** The model serving the subagent, as its own conversation names it. */
  model = $state<string | null>(null);

  private cursor: SubagentCursor = EMPTY_CURSOR;
  private closedRead = false;
  private watched = false;
  private stopped = false;
  private reading = false;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private readonly deliveries: CooperativeQueue<Delivery>;
  private readonly onVisibility = (): void => {
    if (document.visibilityState === "visible") this.poke();
  };

  constructor(
    private readonly ref: SubagentRef,
    private readonly handlers: ChatSocketHandlers,
    /** Whether the parent still has the subagent working; null while the
     *  parent's own state is unknown (still replaying). */
    private readonly running: () => boolean | null,
  ) {
    this.deliveries = new CooperativeQueue((delivery) => this.deliver(delivery));
    if (typeof document !== "undefined") {
      document.addEventListener("visibilitychange", this.onVisibility);
    }
  }

  get healthy(): boolean {
    return !this.stopped;
  }

  /** Read-only: every command is refused. */
  send(_command?: Record<string, unknown>): boolean {
    return false;
  }

  /** A mounted, visible view is (or stopped) showing this subagent. */
  setWatched(on: boolean): void {
    if (this.watched === on) return;
    this.watched = on;
    if (on) this.poke();
  }

  close(): void {
    this.stopped = true;
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    this.deliveries.clear();
    if (typeof document !== "undefined") {
      document.removeEventListener("visibilitychange", this.onVisibility);
    }
  }

  /** Read now (unless one is in flight), replacing any armed wait. */
  private poke(): void {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    void this.tick();
  }

  private idle(): boolean {
    return (
      this.stopped ||
      !this.watched ||
      (typeof document !== "undefined" && document.visibilityState === "hidden")
    );
  }

  private async tick(): Promise<void> {
    if (this.reading || this.idle()) return;
    const working = this.running() !== false;
    // A working subagent after its closing read: the parent resumed it.
    // The closing events are no longer true, so start over.
    if (working && this.closedRead) {
      this.cursor = EMPTY_CURSOR;
      this.closedRead = false;
    }
    let delay = WATCH_MS;
    if (!working && this.closedRead) {
      delay = IDLE_MS;
    } else {
      this.reading = true;
      try {
        await this.read(working);
      } finally {
        this.reading = false;
      }
    }
    if (this.idle() || this.timer !== null) return;
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.tick();
    }, delay);
  }

  private async read(working: boolean): Promise<void> {
    const q = new URLSearchParams();
    if (working) q.set("live", "true");
    if (this.cursor.epoch !== null) {
      q.set("epoch", this.cursor.epoch);
      q.set("after", String(this.cursor.held));
      if (working && this.cursor.stamp !== null) q.set("stamp", this.cursor.stamp);
    }
    const path =
      `/sessions/${encodeURIComponent(this.ref.parentId)}` +
      `/subagents/${encodeURIComponent(this.ref.agentId)}/transcript?${q.toString()}`;
    let answer: SubagentRead;
    try {
      const response = await api(path, { signal: AbortSignal.timeout(READ_TIMEOUT_MS) });
      if (!response.ok) {
        const body = (await response.json().catch(() => null)) as { error?: string } | null;
        this.problem = body?.error ?? `could not read this subagent (${response.status})`;
        // The daemon's own refusal of a FINISHED subagent (no transcript, an
        // agent whose children cannot be read) will not change by asking
        // again: count it as the closing read, so the chain idles until the
        // parent says the subagent is working. A gateway's 5xx is transient.
        if (!working && response.status < 500) this.closedRead = true;
        return;
      }
      answer = (await response.json()) as SubagentRead;
    } catch {
      // Unreachable daemon or a timeout: the next tick tries again, and
      // what is already rendered stays.
      return;
    }
    if (this.stopped) return;
    const step = advanceCursor(this.cursor, answer);
    if (step === null) return;
    this.problem = null;
    if (typeof answer.model === "string" && answer.model !== "") this.model = answer.model;
    const base = step.restart ? 0 : this.cursor.held;
    this.cursor = step.cursor;
    this.closedRead = !working;
    if (step.restart) {
      this.deliveries.push({ kind: "restart", model: this.model, total: step.events.length });
    }
    // Seq 1 is the synthetic `init` a restart delivers; event i of the
    // epoch is seq i + 2.
    this.deliveries.pushMany(
      step.events.map((ev, i) => ({
        kind: "event" as const,
        seq: base + i + 2,
        ts: step.timestamps[i] ?? 0,
        ev,
      })),
    );
  }

  private deliver(delivery: Delivery): void {
    try {
      if (delivery.kind === "event") {
        this.handlers.onEvent({ seq: delivery.seq, ts: delivery.ts, ev: delivery.ev });
        return;
      }
      const info = {
        id: this.ref.agentId,
        agent: "subagent",
        alive: true,
        exit_status: null,
        native_session_id: null,
        model: delivery.model,
        current_mode: null,
        pending_permission: false,
      };
      // A head below the store's own position is the chat protocol's
      // "journal was recreated" signal: the store drops what it rendered.
      if (this.handlers.lastSeq() > 0) this.handlers.onReady(info, 0, 0);
      this.handlers.onReady(info, 0, delivery.total + 1);
      this.handlers.onEvent({ seq: 1, ts: 0, ev: { type: "init", model: delivery.model } });
    } catch (error) {
      console.warn("subagent view: dropping an unapplyable event", error);
    }
  }
}

/** A subagent view's reader. It holds the PARENT chat for as long as it
 *  lives: the parent's store is what says whether the subagent is still
 *  working (and so whether to keep reading), and the hold keeps that store
 *  live even when the parent's own tab is not mounted in this window. */
function makeSubagentSocket(ref: SubagentRef, store: ChatStore): SubagentSocket {
  const parent = acquireChat(ref.parentId);
  const socket = new SubagentSocket(
    ref,
    {
      onReady: (info: ChatSessionInfo, replayFrom: number, head: number | undefined) =>
        store.onReady(info, replayFrom, head),
      onEvent: (entry: SeqEvent) => store.apply(entry),
      onDegraded: () => {},
      onExited: () => {},
      onError: () => {},
      onCommandFailed: () => {},
      onDisconnected: () => {},
      lastSeq: () => store.lastSeq,
    },
    // Until the parent has replayed its journal its sets are empty, which
    // would read as "finished" and close a working subagent's turn.
    () =>
      !parent.store.connected || parent.store.hydrating
        ? null
        : subagentRunning(parent.store, ref.agentId),
  );
  const close = socket.close.bind(socket);
  socket.close = () => {
    close();
    releaseChat(ref.parentId);
  };
  return socket;
}

provideSubagentTransport(makeSubagentSocket);
