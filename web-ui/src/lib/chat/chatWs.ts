import { getToken } from "../api";
import { reconnectingSockets, UNKNOWN_SESSION_RETRIES } from "../ws";

/**
 * Normalized agent events from the daemon (chimaera-agent's AgentEvent,
 * serde-tagged). The store's reducer is the one consumer; fields here stay
 * loose (index into ev by type) to avoid a parallel type hierarchy drifting
 * from the Rust source of truth.
 */
export interface AgentEvent {
  type: string;
  [key: string]: unknown;
}

export interface SeqEvent {
  seq: number;
  ts: number;
  ev: AgentEvent;
}

/** Session info as the ready frame carries it (ChatInfo on the daemon). */
export interface ChatSessionInfo {
  id: string;
  agent: string;
  alive: boolean;
  exit_status: number | null;
  native_session_id: string | null;
  model: string | null;
  current_mode: string | null;
  pending_permission: boolean;
}

export interface ChatSocketHandlers {
  onReady(session: ChatSessionInfo, replayFrom: number): void;
  onEvent(entry: SeqEvent): void;
  /** The session degraded (or toggled) to a terminal under the same id. */
  onDegraded(): void;
  onExited(status: number | null): void;
  /** Fatal server-side error; the socket will not reconnect. */
  onError(message: string): void;
  /** Highest seq applied so far — sent with auth so reconnects replay only the gap. */
  lastSeq(): number;
}

const INITIAL_BACKOFF_MS = 500;
const MAX_BACKOFF_MS = 10_000;

/**
 * One WebSocket per attached chat session, per the /ws/chat/{id} contract:
 * auth (with last_seq) -> ready -> batched journal replay -> live seq-tagged
 * events; AgentCommand frames flow up. Reconnects forever with exponential
 * backoff — the journal gap-replay makes reconnects lossless.
 */
export class ChatSocket {
  private ws: WebSocket | null = null;
  private closed = false;
  private fatal = false;
  private ended = false;
  private reconnecting = false;
  private unknownRetries = 0;
  private backoffMs = INITIAL_BACKOFF_MS;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    private readonly sessionId: string,
    private readonly handlers: ChatSocketHandlers,
  ) {
    this.connect();
  }

  private connect(): void {
    if (this.closed) return;
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const ws = new WebSocket(`${proto}://${location.host}/ws/chat/${this.sessionId}`);
    this.ws = ws;

    ws.onopen = () => {
      ws.send(
        JSON.stringify({
          type: "auth",
          token: getToken() ?? "",
          last_seq: this.handlers.lastSeq(),
        }),
      );
    };

    ws.onmessage = (ev: MessageEvent) => {
      if (typeof ev.data !== "string") return;
      let msg: Record<string, unknown>;
      try {
        msg = JSON.parse(ev.data) as Record<string, unknown>;
      } catch {
        return;
      }
      switch (msg.type) {
        case "ready":
          this.backoffMs = INITIAL_BACKOFF_MS;
          this.unknownRetries = 0;
          this.clearReconnecting();
          this.handlers.onReady(
            msg.session as ChatSessionInfo,
            (msg.replay_from as number) ?? 0,
          );
          break;
        case "batch":
          for (const entry of (msg.events as SeqEvent[]) ?? []) {
            this.handlers.onEvent(entry);
          }
          break;
        case "ev":
          this.handlers.onEvent({
            seq: msg.seq as number,
            ts: msg.ts as number,
            ev: msg.ev as AgentEvent,
          });
          break;
        case "degraded":
          this.ended = true;
          this.handlers.onDegraded();
          break;
        case "exited":
          this.ended = true;
          this.handlers.onExited((msg.status as number | null) ?? null);
          break;
        case "error":
          // Mid view-switch the driver may not be registered yet — the
          // normal onclose reconnect path retries before this goes fatal.
          if (
            msg.code === "unknown_session" &&
            this.unknownRetries < UNKNOWN_SESSION_RETRIES
          ) {
            this.unknownRetries += 1;
            break;
          }
          this.fatal = true;
          this.handlers.onError((msg.message as string) ?? "unknown error");
          break;
        default:
          break;
      }
    };

    ws.onclose = () => {
      if (this.ws === ws) this.ws = null;
      if (this.closed || this.fatal || this.ended) {
        this.clearReconnecting();
        return;
      }
      this.scheduleReconnect();
    };
  }

  private scheduleReconnect(): void {
    if (!this.reconnecting) {
      this.reconnecting = true;
      reconnectingSockets.update((n) => n + 1);
    }
    this.retryTimer = setTimeout(() => {
      this.retryTimer = null;
      this.connect();
    }, this.backoffMs);
    this.backoffMs = Math.min(this.backoffMs * 2, MAX_BACKOFF_MS);
  }

  private clearReconnecting(): void {
    if (this.reconnecting) {
      this.reconnecting = false;
      reconnectingSockets.update((n) => Math.max(0, n - 1));
    }
  }

  /** Send an AgentCommand frame; false when the socket is not open. */
  send(command: Record<string, unknown>): boolean {
    if (this.ws?.readyState !== WebSocket.OPEN) return false;
    this.ws.send(JSON.stringify(command));
    return true;
  }

  close(): void {
    this.closed = true;
    if (this.retryTimer !== null) {
      clearTimeout(this.retryTimer);
      this.retryTimer = null;
    }
    this.clearReconnecting();
    this.ws?.close();
    this.ws = null;
  }
}
