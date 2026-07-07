import { getToken } from "./api";
import type { Link } from "./links";
import type { Session } from "./sessions";

const INITIAL_BACKOFF_MS = 500;
const MAX_BACKOFF_MS = 10_000;

export interface EventsSocketHandlers {
  /**
   * Full session-list snapshot pushed by the daemon; `links` rides the same
   * frame (undefined from a daemon predating linked terminals).
   */
  onSessions(sessions: Session[], links?: Link[]): void;
  /**
   * Connection state. While false the caller should fall back to polling;
   * fired only on transitions.
   */
  onStatus(connected: boolean): void;
  /**
   * The daemon rejected the socket (bad auth or server failure); the socket
   * gives up permanently. `message` is the server's error string
   * ("unauthorized" on a token mismatch).
   */
  onFatal?(message: string): void;
}

interface ServerEventFrame {
  type: string;
  sessions?: Session[];
  links?: Link[];
  message?: string;
}

/**
 * The daemon-wide events socket, per the /ws/events contract: auth text
 * frame ({"type":"auth","token"}) -> {"type":"sessions","sessions":[...]}
 * full snapshots, re-sent whenever any session appears/disappears or changes
 * state/title/name. Replaces the sessions poll while connected; reconnects
 * forever with exponential backoff on unclean closes.
 */
export class EventsSocket {
  private ws: WebSocket | null = null;
  private closed = false;
  private fatal = false;
  private connected = false;
  private backoffMs = INITIAL_BACKOFF_MS;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(private readonly handlers: EventsSocketHandlers) {
    this.connect();
  }

  private connect(): void {
    if (this.closed) return;
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const ws = new WebSocket(`${proto}://${location.host}/ws/events`);
    this.ws = ws;

    ws.onopen = () => {
      ws.send(JSON.stringify({ type: "auth", token: getToken() ?? "" }));
    };

    ws.onmessage = (ev: MessageEvent) => {
      if (typeof ev.data !== "string") return;
      let msg: ServerEventFrame;
      try {
        msg = JSON.parse(ev.data) as ServerEventFrame;
      } catch {
        return;
      }
      if (msg.type === "sessions" && Array.isArray(msg.sessions)) {
        this.backoffMs = INITIAL_BACKOFF_MS;
        this.setConnected(true);
        this.handlers.onSessions(
          msg.sessions,
          Array.isArray(msg.links) ? msg.links : undefined,
        );
      } else if (msg.type === "error") {
        // Bad auth or a server-side failure; give up and surface it (the
        // app shows the blocking re-auth overlay on "unauthorized").
        this.fatal = true;
        this.handlers.onFatal?.(msg.message ?? "connection rejected");
        ws.close();
      }
    };

    ws.onclose = () => {
      if (this.ws === ws) this.ws = null;
      this.setConnected(false);
      if (this.closed || this.fatal) return;
      this.scheduleReconnect();
    };
  }

  private setConnected(up: boolean): void {
    if (this.connected !== up) {
      this.connected = up;
      this.handlers.onStatus(up);
    }
  }

  private scheduleReconnect(): void {
    this.retryTimer = setTimeout(() => {
      this.retryTimer = null;
      this.connect();
    }, this.backoffMs);
    this.backoffMs = Math.min(this.backoffMs * 2, MAX_BACKOFF_MS);
  }

  /** Permanently close the socket (no reconnect). */
  close(): void {
    this.closed = true;
    if (this.retryTimer !== null) {
      clearTimeout(this.retryTimer);
      this.retryTimer = null;
    }
    this.ws?.close();
    this.ws = null;
  }
}
