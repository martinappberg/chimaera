import { getToken } from "../net/api";
import { Reconnector, UNKNOWN_SESSION_RETRIES } from "../net/reconnect";

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
  /** `head` is the journal's highest seq now; when it is below our own
   *  lastSeq the journal was recreated (seq reset) and we must hard-reset. */
  onReady(session: ChatSessionInfo, replayFrom: number, head: number | undefined): void;
  onEvent(entry: SeqEvent): void;
  /** The session degraded (or toggled) to a terminal under the same id. */
  onDegraded(): void;
  onExited(status: number | null): void;
  /** Fatal server-side error; the socket will not reconnect. */
  onError(message: string): void;
  /** One command was refused (code=command_failed, e.g. the driver is gone):
   *  the socket stays up and keeps reconnecting — surface it, don't die. */
  onCommandFailed(message: string): void;
  /** The socket dropped and is reconnecting; the UI is no longer live. */
  onDisconnected(): void;
  /** Highest seq applied so far — sent with auth so reconnects replay only the gap. */
  lastSeq(): number;
}

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
  private unknownRetries = 0;
  private readonly recon = new Reconnector(() => this.connect());

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
          this.recon.succeeded();
          this.unknownRetries = 0;
          this.handlers.onReady(
            msg.session as ChatSessionInfo,
            (msg.replay_from as number) ?? 0,
            msg.head as number | undefined,
          );
          break;
        case "batch":
          // One malformed event (unversioned wire / old journal) must cost one
          // event, not the rest of the batch: lastSeq already advanced, so an
          // uncaught throw here would strand the tail forever.
          for (const entry of (msg.events as SeqEvent[]) ?? []) {
            this.safeEvent(entry);
          }
          break;
        case "ev":
          this.safeEvent({
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
          // One refused command must not kill the pane: the socket is still
          // healthy and the session may come back (respawn, toggle). Going
          // fatal here permanently stopped reconnects after a single answer
          // sent into a dead driver.
          if (msg.code === "command_failed") {
            this.handlers.onCommandFailed((msg.message as string) ?? "command failed");
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
        this.recon.clear();
        return;
      }
      // Live no longer: the composer must stop claiming the agent hears us and
      // stop clearing drafts into a closed socket until we reconnect.
      this.handlers.onDisconnected();
      this.recon.schedule();
    };
  }

  /** Apply one event, isolating a reducer throw so it can't strand the batch. */
  private safeEvent(entry: SeqEvent): void {
    try {
      this.handlers.onEvent(entry);
    } catch (err) {
      console.warn(`chat: dropping unapplyable event seq=${entry.seq}`, err);
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
    this.recon.cancel();
    this.recon.clear();
    this.ws?.close();
    this.ws = null;
  }
}
