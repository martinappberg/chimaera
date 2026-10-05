import { ownerSuspended, sendSocketAuth } from "../net/placement";
import { daemonSocketUrl, isBrowserGateway } from "../net/base";
import { getToken } from "../net/api";
import { ownerAwake, parkUntilAwake, QUIET_OPEN_MS, Reconnector, UNKNOWN_SESSION_RETRIES } from "../net/reconnect";

export interface SessionSocketHandlers {
  readOnly?(): boolean;
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
  /** Input the daemon refused (watching, busy, running elsewhere). The socket
   *  stays; the refusal is said inline, never in the scrollback. */
  onRefused?(reason: string | null, message: string | null): void;
  /** A lasting connection state to say over the pane (`asleep`: the
   *  project's owner is asleep and a keystroke wakes it; `waking`: the first
   *  input is waking it), or null once it is live. */
  onStatus?(status: TerminalStatus | null): void;
  /** The connection is answered, or open and kept for an owner that has not
   *  answered yet (by a keeper that keeps a sleeping cloud machine's sockets
   *  and delivers typing once it wakes): `true`. It dropped, was told the
   *  owner is asleep, or cannot reach the owner: `false`. The pane's label
   *  says "reconnecting" only while this is false. */
  onKept?(kept: boolean): void;
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
  /**
   * The socket dropped uncleanly (a reconnect is scheduled). The output gap
   * begins HERE, not at the eventual ready frame — the pool desyncs a parked
   * buffer immediately, so an adopt racing the reconnect handshake resyncs
   * instead of flushing pre-gap bytes into a visible grid.
   */
  onDrop?(): void;
}

/** `bringing`: typing here is bringing the work to this computer from the
 *  other one. The typing that asked is held until it arrives. */
export type TerminalStatus = "asleep" | "waking" | "bringing";

interface ServerTextFrame {
  type: string;
  title?: string;
  cols?: number;
  rows?: number;
  status?: number | null;
  message?: string;
  code?: string;
  reason?: string;
  to?: string;
}

/**
 * One WebSocket per attached session, per the /ws/sessions/{id} contract:
 * auth text frame -> ready text frame -> snapshot binary frame -> live
 * binary output + JSON event text frames. Reconnects forever with
 * exponential backoff on unclean closes (the close-the-laptop path).
 */
export class SessionSocket {
  private ws: WebSocket | null = null;
  private authenticatedSocket: WebSocket | null = null;
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
  /** Server grid adoption synchronously emits xterm.onResize. It is an
   * observation, not a new request: echoing an older event can undo a newer
   * resize and start an endless feedback loop across attached clients. */
  private adoptingServerGrid = false;
  /** Whether the owner was attached parked on this connection, as far as
   *  this socket can know: what its auth frame said, then every `park` /
   *  `unpark` it sent since. A keeper that keeps the socket folds exactly
   *  those frames into the auth frame it attaches with, so this is what the
   *  next `ready` answers; a daemon reached directly applies them itself. */
  private wireParked = false;
  private unknownRetries = 0;
  /** A wake-carrying reconnect is in flight (at most one per drop). */
  private waking = false;
  /** This connection's `ready` arrived: the owner hears input now. */
  private live = false;
  /** The owner said it is asleep (`worker_asleep`). Its state, not this
   *  socket's: it outlives a dropped connection (a gateway may close after
   *  saying so) and ends with a wake, a move or the next `ready`. */
  private asleep = false;
  /** Set while this socket is down because its owner is asleep: no retry
   *  timer runs; a keystroke (with wake intent) or a sign the owner answers
   *  again dials it. Calling it leaves the waiting set. */
  private leaveSleepWait: (() => void) | null = null;
  /** This connection was answered (`ready`) or stayed open quietly
   *  ({@link QUIET_OPEN_MS}): its owner's side keeps it. One that closes
   *  before either was refused. */
  private kept = false;
  private quietTimer: ReturnType<typeof setTimeout> | null = null;
  private readonly recon = new Reconnector(() => this.connect());
  private readonly encoder = new TextEncoder();

  constructor(
    private readonly sessionId: string,
    private readonly handlers: SessionSocketHandlers,
  ) {
    // Opening a terminal is viewing, never interaction: it must not wake a
    // paused project. Typing does ({@link wakeOnInput}).
    this.connect();
  }

  private connect(interaction = false): void {
    if (this.closed) return;
    this.stopSleepWait();
    this.setKept(false);
    const readOnly = this.handlers.readOnly?.() ?? false;
    const query = readOnly ? "?read_only=true" : interaction && !this.handlers.parked?.() ? "?wake=interaction" : "";
    const ws = new WebSocket(daemonSocketUrl(`/ws/sessions/${this.sessionId}${query}`));
    ws.binaryType = "arraybuffer";
    this.ws = ws;
    this.live = false;

    ws.onopen = () => {
      // Parked attach: the server withholds output + snapshot until unpark,
      // and must not adopt this hidden window's stale dims.
      const parked = this.handlers.parked?.() ?? false;
      this.wireParked = parked;
      // Carry the client grid so the server resizes BEFORE rendering the
      // snapshot; the frame then always matches what the terminal displays.
      const dims = parked || readOnly ? null : (this.handlers.dims?.() ?? null);
      sendSocketAuth(ws, { type: "auth", token: getToken() ?? "", parked, ...(dims ?? {}) },
        () => this.ws === ws && !this.closed, () => {
          this.authenticatedSocket = ws;
          this.awaitQuiet(ws);
        });
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
      this.waking = false;
      this.live = false;
      const kept = this.kept;
      this.setKept(false);
      // A project view's placement read may say the owner sleeps before any
      // frame did (a gateway can close without one). That holds for a
      // connection the owner's side refused. One it kept (answered, or open
      // and quiet) and then lost is dialed again first: a keeper that keeps
      // a sleeping machine's sockets takes it, and one that does not refuses
      // that dial, which parks it here.
      if (!this.asleep && !kept && !this.closed && !this.fatal && !this.exited && ownerSuspended()) this.asleep = true;
      this.handlers.onStatus?.(this.asleep ? "asleep" : null);
      if (this.closed || this.fatal || this.exited) {
        this.recon.clear();
        return;
      }
      this.handlers.onDrop?.();
      if (this.asleep) {
        this.waitForOwner();
        return;
      }
      this.recon.schedule();
    };
  }

  /** The owner is asleep: retrying on a backoff would only hear "asleep"
   *  again. Wait with no timer until a keystroke or {@link retrySoon} /
   *  `ownerAwake` dials. */
  private waitForOwner(): void {
    this.recon.cancel();
    this.recon.clear();
    this.stopSleepWait();
    this.leaveSleepWait = parkUntilAwake(() => {
      this.leaveSleepWait = null;
      if (!this.closed && !this.fatal && !this.exited && this.ws === null) this.connect();
    });
  }

  private stopSleepWait(): void {
    this.leaveSleepWait?.();
    this.leaveSleepWait = null;
  }

  /** Authenticated: if nothing is heard for {@link QUIET_OPEN_MS} the socket
   *  is kept open for an owner that has not answered. That is healthy: it
   *  leaves the reconnecting indicator (keeping its backoff, which a later
   *  drop continues). */
  private awaitQuiet(ws: WebSocket): void {
    this.stopQuiet();
    this.quietTimer = setTimeout(() => {
      this.quietTimer = null;
      if (this.ws !== ws || this.closed) return;
      this.recon.clear();
      this.setKept(true);
    }, QUIET_OPEN_MS);
  }

  private stopQuiet(): void {
    if (this.quietTimer !== null) clearTimeout(this.quietTimer);
    this.quietTimer = null;
  }

  /** Settles the quiet wait either way; tells the pane only on a change. */
  private setKept(kept: boolean): void {
    this.stopQuiet();
    if (this.kept === kept) return;
    this.kept = kept;
    this.handlers.onKept?.(kept);
  }

  /** Waiting for a sleeping owner (no socket, no retry timer). */
  get waitingForOwner(): boolean {
    return this.leaveSleepWait !== null;
  }

  /** Its row says the owner may answer again (reachable, or a new owner):
   *  a socket waiting for it dials now (passively); one sitting out a
   *  backoff retries at once. */
  retrySoon(): void {
    if (this.closed || this.fatal || this.exited) return;
    if (this.leaveSleepWait !== null) this.connect();
    else this.recon.nudge(0);
  }

  /**
   * Keystrokes into a browser view whose socket is down, or into any socket
   * waiting for a sleeping owner: reconnect now, once, carrying wake intent.
   * The keystrokes themselves are not queued. An open socket never needs
   * this: whoever keeps it (a native window's daemon, or a keeper that keeps
   * a sleeping cloud machine's sockets) holds early typing itself while the
   * owner wakes.
   */
  private wakeOnInput(): void {
    if (!(isBrowserGateway() || this.leaveSleepWait !== null) || this.closed || this.fatal || this.exited || this.waking) return;
    if (this.handlers.readOnly?.() ?? false) return;
    this.waking = true;
    this.asleep = false;
    this.handlers.onStatus?.(null);
    this.dropSocket();
    this.recon.cancel();
    this.connect(true);
  }

  private handleTextFrame(raw: string): void {
    let msg: ServerTextFrame;
    try {
      msg = JSON.parse(raw) as ServerTextFrame;
    } catch {
      return;
    }
    // Any frame ends the quiet wait: the owner's side spoke.
    this.stopQuiet();
    switch (msg.type) {
      case "ready": {
        this.recon.succeeded();
        this.unknownRetries = 0;
        this.waking = false;
        this.asleep = false;
        this.live = true;
        this.setKept(true);
        // The project answers on this socket: whatever waited for it reads again.
        ownerAwake();
        this.handlers.onStatus?.(null);
        // `ready` answers the frame the owner was attached with: the auth
        // frame, with the `park`/`unpark` sent since folded in by a keeper
        // that kept this socket across its owner's sleep (`wireParked`). The
        // cases below settle what the terminal is now against that.
        const parkedNow = this.handlers.parked?.() ?? false;
        const reset = this.everReady || (this.handlers.readOnly?.() ?? false);
        if (this.wireParked) {
          // A parked attach: no snapshot follows and output is withheld.
          this.everReady = true;
          if (parkedNow) {
            // Never reset the grid for it, and send no dims.
            this.handlers.onParkedReady?.();
            break;
          }
          // Shown, but its `unpark` never went out on this socket. Settle
          // the grid first, then unpark, which after a parked attach always
          // repaints (resync + snapshot). Reset first: should a snapshot
          // follow after all, it must not land on the old screen.
          const shown = this.handlers.dims?.() ?? null;
          if (reset) this.adoptServerGrid(() => this.handlers.onReset(msg.cols, msg.rows));
          if (shown !== null && (msg.cols !== shown.cols || msg.rows !== shown.rows)) {
            this.sendResize(shown.cols, shown.rows);
          }
          this.sendUnpark();
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
        if (reset) this.adoptServerGrid(() => this.handlers.onReset(msg.cols, msg.rows));
        this.everReady = true;
        if (parkedNow) {
          // Parked, and its `park` never went out on this socket: the owner
          // would stream to a hidden terminal. Stop it (the snapshot still
          // lands) and send no grid: a hidden pooled terminal never resizes
          // the PTY, least of all over the window that shows it.
          this.sendPark();
          break;
        }
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
        this.adoptServerGrid(() => this.handlers.onReset(msg.cols, msg.rows));
        break;
      case "title":
        if (typeof msg.title === "string") this.handlers.onTitle(msg.title);
        break;
      case "resized":
        if (typeof msg.cols === "number" && typeof msg.rows === "number") {
          const { cols, rows } = msg;
          this.adoptServerGrid(() => this.handlers.onResized(cols, rows));
        }
        break;
      case "exited":
        this.exited = true;
        this.sawExited = true;
        this.handlers.onExited(msg.status ?? null);
        break;
      case "waking":
        // The typing that asked is held until the owner answers, and nothing
        // echoes before `ready`: not live meanwhile, also on a socket that
        // was (the machine went to sleep behind a kept connection).
        this.asleep = false;
        this.live = false;
        this.handlers.onStatus?.("waking");
        break;
      case "bringing":
        // The typing that asked is held until the work arrives; the socket
        // then closes and the reconnect finds the terminal where it runs.
        this.asleep = false;
        this.handlers.onStatus?.("bringing");
        break;
      case "moved":
      case "paused":
        // Continuing on another machine, or resuming here on its own: not an
        // exit. Keep the screen; the daemon closes this socket and the
        // ordinary reconnect follows it (the pane says why from the row).
        if (this.asleep) {
          this.asleep = false;
          this.handlers.onStatus?.(null);
        }
        break;
      case "error":
        if (msg.code === "read_only") {
          // The other computer kept the work: nothing is coming any more.
          if (msg.reason === "still_working") this.handlers.onStatus?.(null);
          if (this.handlers.onRefused !== undefined) {
            this.handlers.onRefused(msg.reason ?? null, msg.message ?? null);
          } else {
            this.handlers.onError(msg.message ?? "Just watching");
          }
          break;
        }
        if (msg.code === "worker_asleep") {
          // Not an error: the pane says so until a keystroke wakes it. Said
          // by a relay or gateway that keeps no socket for the owner,
          // possibly after this one had counted as kept (a slow answer).
          this.setKept(false);
          this.asleep = true;
          this.handlers.onStatus?.("asleep");
          break;
        }
        if (msg.code === "remote_unavailable") {
          // Nothing typed now is heard, and a wake that was under way did
          // not arrive.
          this.live = false;
          this.handlers.onStatus?.(null);
          this.setKept(false);
          // A relay that cannot reach the owner says it is retrying
          // (`reason:"reconnecting"`) and stays silent meanwhile: that lasts
          // until its next frame. Anyone else saying it has handed back what
          // it held and still keeps this socket: quiet from here on is kept
          // again.
          if (msg.reason !== "reconnecting" && this.ws !== null) this.awaitQuiet(this.ws);
          break;
        }
        if (msg.code === "workspace_scope_changed") { break; }
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

  private adoptServerGrid(update: () => void): void {
    this.adoptingServerGrid = true;
    try {
      update();
    } finally {
      this.adoptingServerGrid = false;
    }
  }

  /** True while the socket is connected and can accept input frames. */
  get isOpen(): boolean {
    return this.ws?.readyState === WebSocket.OPEN && this.authenticatedSocket === this.ws;
  }

  /** Open AND answered by the session (`ready`): typing reaches it now. A
   *  connection to a paused or waking owner is open but not live, and must
   *  not predict echo for input the owner has not received. */
  get isLive(): boolean {
    return this.isOpen && this.live;
  }

  /** Send raw keyboard input (from term.onData) as a binary frame. An open
   *  socket takes it whether or not its owner has answered: whoever keeps the
   *  connection for a sleeping owner holds the typing, wakes the owner and
   *  delivers it once (or refuses it, which the pane says). */
  sendInput(data: string): void {
    if (this.handlers.readOnly?.()) return;
    if (this.ws?.readyState === WebSocket.OPEN && this.authenticatedSocket === this.ws) {
      this.ws.send(this.encoder.encode(data));
    } else if (isBrowserGateway() || this.leaveSleepWait !== null) {
      // A browser view's socket is down, or it waits for a sleeping owner:
      // the keystroke is dropped (never queued) and a wake-carrying
      // reconnect starts; say so over the pane.
      this.wakeOnInput();
      if (!this.closed && !this.fatal && !this.exited) this.handlers.onRefused?.("waking", null);
    }
  }

  /** One guard for every control frame: silently dropped when the socket is
   *  down — reconnect re-establishes the state these frames carry (dims via
   *  the ready reconcile, parked via the auth flag). */
  private sendJson(msg: unknown): boolean {
    if (this.ws?.readyState !== WebSocket.OPEN || this.authenticatedSocket !== this.ws) return false;
    this.ws.send(JSON.stringify(msg));
    return true;
  }

  /** Send a resize request as a text frame. */
  sendResize(cols: number, rows: number): void {
    if (!this.adoptingServerGrid && !this.handlers.readOnly?.()) this.sendJson({ type: "resize", cols, rows });
  }

  /**
   * Tell the server this terminal parked: output forwarding stops (the
   * session's server-side ring buffers the stream) until unpark. Old servers
   * ignore the frame and keep streaming; the client-side ParkedBuffer still
   * handles that stream, so both directions degrade gracefully.
   */
  sendPark(): void {
    if (this.sendJson({ type: "park" })) this.wireParked = true;
  }

  /** Resume after park: the server catches up from its ring, or repaints
   *  (resync + snapshot) when the ring can't cover the gap. */
  sendUnpark(): void {
    if (this.sendJson({ type: "unpark" })) this.wireParked = false;
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

  /** Switching between watching and control reconnects with the new access.
   *  Taking control is still not interaction: the first keystroke is. */
  accessChanged(): void {
    if (this.closed) return;
    this.fatal = false;
    this.exited = false;
    this.dropSocket();
    this.recon.cancel();
    this.connect();
  }

  /** Permanently close the socket (no reconnect). */
  close(): void {
    this.closed = true;
    this.stopSleepWait();
    this.setKept(false);
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
