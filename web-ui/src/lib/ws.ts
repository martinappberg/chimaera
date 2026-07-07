import { writable } from "svelte/store";
import { getToken } from "./api";

/**
 * Number of session sockets currently trying to reconnect. The daemon dot in
 * the rail pulses while this is non-zero.
 */
export const reconnectingSockets = writable(0);

export interface SessionSocketHandlers {
  /** Raw PTY output (including the initial snapshot). Feed to term.write(). */
  onBinary(data: Uint8Array): void;
  /**
   * Reset the terminal before the next binary frame (a fresh snapshot
   * follows). Fired on server resync and on successful reconnect. When the
   * server tags the resync with the grid the snapshot was rendered at,
   * resize to it BEFORE resetting — a snapshot replayed at any other width
   * re-wraps every soft-wrapped row at the wrong column.
   */
  onReset(cols?: number, rows?: number): void;
  /**
   * The client's current grid, sent with the auth frame so the server adopts
   * it before rendering the snapshot. Without it, a resize that happened
   * while the socket was down (sendResize is dropped, and ResizeObserver
   * never re-fires for an unchanged container) leaves the PTY at stale dims
   * forever.
   */
  dims?(): { cols: number; rows: number } | null;
  onTitle(title: string): void;
  onResized(cols: number, rows: number): void;
  onExited(status: number | null): void;
  /** Server-side error, surfaced quietly. The socket will not reconnect. */
  onError(message: string): void;
}

interface ServerTextFrame {
  type: string;
  title?: string;
  cols?: number;
  rows?: number;
  status?: number | null;
  message?: string;
}

const INITIAL_BACKOFF_MS = 500;
const MAX_BACKOFF_MS = 10_000;

/**
 * One WebSocket per attached session, per the /ws/sessions/{id} contract:
 * auth text frame -> ready text frame -> snapshot binary frame -> live
 * binary output + JSON event text frames. Reconnects forever with
 * exponential backoff on unclean closes (the close-the-laptop path).
 */
export class SessionSocket {
  private ws: WebSocket | null = null;
  private closed = false;
  private fatal = false;
  private exited = false;
  private everReady = false;
  private reconnecting = false;
  private backoffMs = INITIAL_BACKOFF_MS;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private readonly encoder = new TextEncoder();

  constructor(
    private readonly sessionId: string,
    private readonly handlers: SessionSocketHandlers,
  ) {
    this.connect();
  }

  private connect(): void {
    if (this.closed) return;
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const ws = new WebSocket(`${proto}://${location.host}/ws/sessions/${this.sessionId}`);
    ws.binaryType = "arraybuffer";
    this.ws = ws;

    ws.onopen = () => {
      // Carry the client grid so the server resizes BEFORE rendering the
      // snapshot; the frame then always matches what the terminal displays.
      const dims = this.handlers.dims?.() ?? null;
      ws.send(JSON.stringify({ type: "auth", token: getToken() ?? "", ...(dims ?? {}) }));
    };

    ws.onmessage = (ev: MessageEvent) => {
      if (typeof ev.data === "string") {
        this.handleTextFrame(ev.data);
      } else {
        this.handlers.onBinary(new Uint8Array(ev.data as ArrayBuffer));
      }
    };

    ws.onclose = () => {
      if (this.ws === ws) this.ws = null;
      if (this.closed || this.fatal || this.exited) {
        this.clearReconnecting();
        return;
      }
      this.scheduleReconnect();
    };
  }

  private handleTextFrame(raw: string): void {
    let msg: ServerTextFrame;
    try {
      msg = JSON.parse(raw) as ServerTextFrame;
    } catch {
      return;
    }
    switch (msg.type) {
      case "ready":
        this.backoffMs = INITIAL_BACKOFF_MS;
        this.clearReconnecting();
        // On a reconnect the server re-sends a full snapshot; wipe the stale
        // screen so the snapshot reconstructs state exactly.
        if (this.everReady) this.handlers.onReset();
        this.everReady = true;
        break;
      case "resync":
        this.handlers.onReset(msg.cols, msg.rows);
        break;
      case "title":
        if (typeof msg.title === "string") this.handlers.onTitle(msg.title);
        break;
      case "resized":
        if (typeof msg.cols === "number" && typeof msg.rows === "number") {
          this.handlers.onResized(msg.cols, msg.rows);
        }
        break;
      case "exited":
        this.exited = true;
        this.handlers.onExited(msg.status ?? null);
        break;
      case "error":
        this.fatal = true;
        this.handlers.onError(msg.message ?? "unknown error");
        break;
      default:
        break;
    }
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

  /** True while the socket is connected and can accept input frames. */
  get isOpen(): boolean {
    return this.ws?.readyState === WebSocket.OPEN;
  }

  /** Send raw keyboard input (from term.onData) as a binary frame. */
  sendInput(data: string): void {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(this.encoder.encode(data));
    }
  }

  /** Send a resize request as a text frame. */
  sendResize(cols: number, rows: number): void {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify({ type: "resize", cols, rows }));
    }
  }

  /** Permanently close the socket (no reconnect). */
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

/**
 * Type `text` into a session that has no pooled terminal attached (context
 * bridge fallback): open a one-shot socket, send the input once the server
 * is provably ready (the snapshot binary frame has arrived), and close.
 * The text is raw input — callers guarantee it carries no newline, so this
 * can never submit anything.
 */
export function typeIntoDetachedSession(sessionId: string, text: string): void {
  let sent = false;
  const socket = new SessionSocket(sessionId, {
    onBinary: () => {
      if (sent) return;
      sent = true;
      socket.sendInput(text);
      // close() lets the buffered frame flush before the close handshake.
      setTimeout(() => socket.close(), 250);
    },
    onReset: () => {},
    onTitle: () => {},
    onResized: () => {},
    onExited: () => socket.close(),
    onError: () => socket.close(),
  });
  // Give up quietly if the session never produces a snapshot.
  setTimeout(() => {
    if (!sent) socket.close();
  }, 5000);
}
