import { getToken } from "../net/api";
import { Reconnector, UNKNOWN_SESSION_RETRIES } from "../net/reconnect";

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
  /**
   * Whether the terminal is currently parked (hidden pooled instance). Read
   * at every (re)connect: a parked attach tells the server to withhold
   * output and skip the snapshot (`auth.parked`), and omits the grid dims —
   * a hidden window's stale dims must never reflow the server grid.
   */
  parked?(): boolean;
  /**
   * A connection that authenticated parked became ready: no snapshot is
   * coming on this connection, and any bytes buffered before it dropped
   * predate an output gap — the pool desyncs the buffer so adopt resyncs
   * into a fresh visible attach.
   */
  onParkedReady?(): void;
}

interface ServerTextFrame {
  type: string;
  title?: string;
  cols?: number;
  rows?: number;
  status?: number | null;
  message?: string;
  code?: string;
}

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
  /**
   * The session reported exited at least once. Unlike `exited` (which
   * resync() clears to allow a last-words reconnect), this never resets:
   * it distinguishes "unknown session" after a witnessed exit (the daemon
   * simply forgot the dead session — terminal-graceful) from a genuinely
   * missing session (fatal after retries).
   */
  private sawExited = false;
  private everReady = false;
  /** The auth frame of the CURRENT connection carried `parked: true`. */
  private sentParkedAuth = false;
  private unknownRetries = 0;
  private readonly recon = new Reconnector(() => this.connect());
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
      // Parked attach: the server withholds output + snapshot until unpark,
      // and must not adopt this hidden window's stale dims.
      const parked = this.handlers.parked?.() ?? false;
      this.sentParkedAuth = parked;
      // Carry the client grid so the server resizes BEFORE rendering the
      // snapshot; the frame then always matches what the terminal displays.
      const dims = parked ? null : (this.handlers.dims?.() ?? null);
      ws.send(
        JSON.stringify({ type: "auth", token: getToken() ?? "", parked, ...(dims ?? {}) }),
      );
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
        this.recon.clear();
        return;
      }
      this.recon.schedule();
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
      case "ready": {
        this.recon.succeeded();
        this.unknownRetries = 0;
        if (this.sentParkedAuth) {
          // No snapshot follows on a parked connection — never reset the
          // grid for it, and skip the dims reconcile (no dims were sent).
          this.everReady = true;
          this.handlers.onParkedReady?.();
          break;
        }
        // Grid truth BEFORE the reset below may resize the terminal: a fit
        // that landed mid-handshake is what the reconcile must preserve.
        const d = this.handlers.dims?.() ?? null;
        // On a reconnect the server re-sends a full snapshot; wipe the stale
        // screen so the snapshot reconstructs state exactly. The ready frame
        // carries the dims the snapshot was rendered at — for a live session
        // the server already adopted the auth-frame grid, and for a dead
        // session's last-words replay these are the death-time dims the
        // final screen must parse at — so adopt them like a resync's.
        if (this.everReady) this.handlers.onReset(msg.cols, msg.rows);
        this.everReady = true;
        // Reconcile grids: resizes are silently dropped while the socket is
        // down or mid-handshake (the first fit often lands during CONNECTING),
        // and ResizeObserver never re-fires for an unchanged container. The
        // ready frame carries the server's dims — correct any drift exactly
        // once, here.
        if (
          d !== null &&
          typeof msg.cols === "number" &&
          typeof msg.rows === "number" &&
          (msg.cols !== d.cols || msg.rows !== d.rows)
        ) {
          this.sendResize(d.cols, d.rows);
        }
        break;
      }
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
        this.sawExited = true;
        this.handlers.onExited(msg.status ?? null);
        break;
      case "error":
        if (msg.code === "unknown_session") {
          // After a witnessed exit, "unknown" means even the session's
          // last words are gone (bounded server-side memory) — terminal-
          // graceful: keep the grid + [exited] marker, stop reconnecting,
          // never surface an error for a session that merely finished.
          if (this.sawExited) {
            this.exited = true;
            break;
          }
          // Otherwise it may just be mid view-switch: let the normal
          // onclose reconnect path retry before giving up.
          if (this.unknownRetries < UNKNOWN_SESSION_RETRIES) {
            this.unknownRetries += 1;
            break;
          }
        }
        this.fatal = true;
        this.handlers.onError(msg.message ?? "unknown error");
        break;
      default:
        break;
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

  /**
   * Tell the server this terminal parked: output forwarding stops (the
   * session's server-side ring buffers the stream) until unpark. Dropped
   * when the socket is down — the reconnect's parked auth carries the state.
   * Old servers ignore the frame and keep streaming; the client-side
   * ParkedBuffer still handles that stream, so both directions degrade
   * gracefully.
   */
  sendPark(): void {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify({ type: "park" }));
    }
  }

  /** Resume after park: the server catches up from its ring, or repaints
   *  (resync + snapshot) when the ring can't cover the gap. */
  sendUnpark(): void {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify({ type: "unpark" }));
    }
  }

  /**
   * Force a clean re-attach: drop the current socket and reconnect now. The
   * server fully re-snapshots on a fresh attach (ready → reset → snapshot),
   * which is the recovery path for a parked terminal whose buffered stream
   * was discarded. A session that exited while parked gets one more connect
   * so the server's last-words replay can paint the final screen; it
   * re-closes on the replayed exited frame. Returns whether a reconnect was
   * actually initiated — false on a fatal/closed socket, so callers don't
   * clear recovery latches for a resync that never happened.
   */
  resync(): boolean {
    if (this.closed || this.fatal) return false;
    this.exited = false;
    this.dropSocket();
    this.recon.cancel();
    this.connect();
    return true;
  }

  /** Permanently close the socket (no reconnect). */
  close(): void {
    this.closed = true;
    this.recon.cancel();
    this.recon.clear();
    this.dropSocket();
  }

  /**
   * Abandon the current WebSocket, handlers detached first: these closes
   * are intentional (not reconnect triggers), and an already-queued frame
   * or open event must not fire into a socket we no longer own.
   */
  private dropSocket(): void {
    const ws = this.ws;
    this.ws = null;
    if (ws !== null) {
      ws.onopen = null;
      ws.onmessage = null;
      ws.onerror = null;
      ws.onclose = null;
      ws.close();
    }
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
