import { daemonSocketUrl, isBrowserGateway } from "../net/base";
import { movedTo, ownerSuspended, parsePause, sendSocketAuth, type MovedTo, type SessionPause } from "../net/placement";
import { getToken } from "../net/api";
import { OwnerWait, ownerAwake, QUIET_OPEN_MS, Reconnector, UNKNOWN_SESSION_RETRIES } from "../net/reconnect";
import { CooperativeQueue } from "./cooperativeQueue";
import { NativeUiTransport, isNativeUiAction } from "./nativeUi";

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

/** What a `ready` says about its attach, beyond the session. */
export interface ReadyAttach {
  /** The daemon accepts a send at most once under its `client_id` and
   *  answers `cancel_send` (`ready.send_ids`, additive): unconfirmed sends
   *  may be sent again. False for a daemon that predates it. */
  sendIds: boolean;
  /** Not this socket's first `ready`: whoever keeps the socket attached it
   *  again after the owner slept. */
  reattach: boolean;
  /** Client IDs still owned by this driver's queue at ready.head. Absent on
   * older daemons; at most 64, never a claim about unkeyed legacy messages. */
  activeQueuedIds?: string[];
}

export interface ChatSocketHandlers {
  /** `head` is the journal's highest seq now; when it is below our own
   *  lastSeq the journal was recreated (seq reset) and we must hard-reset. */
  onReady(session: ChatSessionInfo, replayFrom: number, head: number | undefined, attach: ReadyAttach): void;
  onEvent(entry: SeqEvent): void;
  /** The session degraded (or toggled) to a terminal under the same id. */
  onDegraded(): void;
  onExited(status: number | null): void;
  /** Fatal server-side error; the socket will not reconnect. */
  onError(message: string): void;
  /** One command was refused (`command_failed` or `invalid_command`): the
   *  socket stays up and keeps reconnecting — surface it, don't die.
   *  `command` names the refused command (`send`, `interrupt`…) when the
   *  daemon tagged it (additive); null from older daemons. `clientId` is the
   *  id that command was sent under (additive): which send was refused. */
  onCommandFailed(message: string, command: string | null, reason?: string | null, clientId?: string | null): void;
  /** The daemon's answer to `cancel_send`: whether the send under that id
   *  was withdrawn (true: none will run) or had already been accepted. */
  onSendCancelled?(clientId: string, cancelled: boolean): void;
  /** Delivery may have happened; never treat this as an unsent draft. */
  onSendUncertain?(clientId: string, message: string): void;
  onSendConfirmed?(clientId: string): void;
  /** The conversation's project is paused; the next send picks it back up. */
  onAsleep?(): void;
  /** A send picked the paused project back up: it is waking and the send is
   *  delivered once it answers. */
  onWaking?(): void;
  /** The socket is open and authenticated and its owner has said nothing
   *  yet: the owner's side keeps the connection (a keeper that keeps a
   *  sleeping cloud machine's sockets, directly or behind this computer's
   *  relay) and delivers what is sent once the owner answers. Not live, and
   *  not reconnecting. */
  onHeld?(): void;
  /** The owner cannot be reached right now (`remote_unavailable`): the
   *  socket stays open, either while a relay keeps trying (nothing follows
   *  until it gets through) or kept as before by whoever just handed back a
   *  wake that did not arrive (`onHeld` follows). */
  onUnreachable?(): void;
  /** Acting here brings the work to this computer: the send that asked waits
   *  for it rather than for the current owner. */
  onBringing?(): void;
  /** The conversation is continuing on another machine: stay mounted and
   *  keep reconnecting; it did not exit. */
  onMoved?(to: MovedTo): void;
  /** The conversation has no process here yet and resumes on its own
   *  (after an update, once its agent is signed in on the cloud machine,
   *  while its transfer opens it): stay mounted, keep reconnecting. */
  onPaused?(pause: SessionPause): void;
  /** The socket dropped and is reconnecting; the UI is no longer live. */
  onDisconnected(): void;
  /** Highest seq applied so far — sent with auth so reconnects replay only the gap. */
  lastSeq(): number;
}

type ChatDelivery =
  | {
      kind: "ready";
      session: ChatSessionInfo;
      replayFrom: number;
      head: number | undefined;
      attach: ReadyAttach;
      nativeUiGeneration: number;
    }
  | { kind: "event"; entry: SeqEvent }
  | { kind: "degraded" }
  | { kind: "exited"; status: number | null }
  | { kind: "error"; message: string }
  | { kind: "command_failed"; message: string; command: string | null; reason: string | null; clientId: string | null }
  | { kind: "send_cancelled"; clientId: string; cancelled: boolean }
  | { kind: "send_uncertain"; clientId: string; message: string }
  | { kind: "send_confirmed"; clientId: string }
  | { kind: "asleep" }
  | { kind: "waking" }
  | { kind: "held" }
  | { kind: "unreachable" }
  | { kind: "bringing" }
  | { kind: "moved"; to: MovedTo }
  | { kind: "paused"; pause: SessionPause }
  | { kind: "disconnected" };

/**
 * One WebSocket per attached chat session, per the /ws/chat/{id} contract:
 * auth (with last_seq) -> ready -> batched journal replay -> live seq-tagged
 * events; AgentCommand frames flow up. Reconnects forever with exponential
 * backoff — the journal gap-replay makes reconnects lossless.
 */
export class ChatSocket {
  readonly nativeUi = new NativeUiTransport((frame) =>
    isNativeUiAction(frame.request) ? this.send(frame) : this.sendQuietly(frame));
  private nativeUiGeneration = 0;
  private resetNativeUi(): void {
    this.nativeUiGeneration++;
    this.nativeUi.reset(true);
  }
  private ws: WebSocket | null = null;
  private authenticatedSocket: WebSocket | null = null;
  private closed = false;
  private fatal = false;
  private ended = false;
  /** A wake-carrying reconnect is in flight (at most one per drop). */
  private waking = false;
  /** The owner said it is asleep (`worker_asleep`) and nothing has woken it
   *  since (a `ready`, `waking`, a move or a pause ends it). */
  private asleep = false;
  /** Parked while the owner is asleep (no retry timer: a user action with
   *  wake intent or a sign the owner answers again dials it), and the quiet
   *  wait after authenticating. */
  private readonly wait = new OwnerWait();
  /** This connection was answered (`ready`) or stayed open quietly
   *  ({@link QUIET_OPEN_MS}): its owner's side keeps it. One that closes
   *  before either was refused. */
  private kept = false;
  private unknownRetries = 0;
  private readonly recon = new Reconnector(() => this.connect());
  /** Replay, live events, and terminal frames share one cooperative FIFO.
   *  Keeping the control frames in the same queue preserves wire ordering —
   *  an `exited` frame cannot overtake the final replay slice. */
  private readonly deliveries: CooperativeQueue<ChatDelivery>;

  constructor(
    private readonly sessionId: string,
    private readonly handlers: ChatSocketHandlers,
  ) {
    this.deliveries = new CooperativeQueue((delivery) => this.deliver(delivery));
    this.connect();
  }

  private deliver(delivery: ChatDelivery): void {
    try {
      switch (delivery.kind) {
        case "ready":
          this.handlers.onReady(delivery.session, delivery.replayFrom, delivery.head, delivery.attach);
          if (delivery.nativeUiGeneration === this.nativeUiGeneration
            && this.ws?.readyState === WebSocket.OPEN && this.authenticatedSocket === this.ws
            && this.healthy) this.nativeUi.connected();
          break;
        case "event":
          this.handlers.onEvent(delivery.entry);
          break;
        case "degraded":
          this.handlers.onDegraded();
          break;
        case "exited":
          this.handlers.onExited(delivery.status);
          break;
        case "error":
          this.handlers.onError(delivery.message);
          break;
        case "command_failed":
          this.handlers.onCommandFailed(delivery.message, delivery.command, delivery.reason, delivery.clientId);
          break;
        case "send_cancelled":
          this.handlers.onSendCancelled?.(delivery.clientId, delivery.cancelled);
          break;
        case "send_uncertain":
          this.handlers.onSendUncertain?.(delivery.clientId, delivery.message);
          break;
        case "send_confirmed":
          this.handlers.onSendConfirmed?.(delivery.clientId);
          break;
        case "asleep":
          this.handlers.onAsleep?.();
          break;
        case "waking":
          this.handlers.onWaking?.();
          break;
        case "held":
          this.handlers.onHeld?.();
          break;
        case "unreachable":
          this.handlers.onUnreachable?.();
          break;
        case "bringing":
          this.handlers.onBringing?.();
          break;
        case "moved":
          this.handlers.onMoved?.(delivery.to);
          break;
        case "paused":
          this.handlers.onPaused?.(delivery.pause);
          break;
        case "disconnected":
          this.handlers.onDisconnected();
          break;
      }
    } catch (error) {
      const suffix = delivery.kind === "event" ? ` seq=${delivery.entry.seq}` : "";
      console.warn(`chat: dropping unapplyable ${delivery.kind}${suffix}`, error);
    }
  }

  /** Attaching is passive: only a user action ({@link wakeOnInput}) may ask
   *  a paused project's owner to wake. */
  private connect(interaction = false): void {
    if (this.closed) return;
    this.wait.stopPark();
    this.wait.stopQuiet();
    this.kept = false;
    const ws = new WebSocket(daemonSocketUrl(`/ws/chat/${this.sessionId}${interaction ? "?wake=interaction" : ""}`));
    this.ws = ws;
    // `ready` frames heard on this socket: a second one is a reattach.
    let readies = 0;

    ws.onopen = () => {
      sendSocketAuth(ws, {
        type: "auth", token: getToken() ?? "", last_seq: this.handlers.lastSeq(),
      }, () => this.ws === ws && !this.closed, () => {
        this.authenticatedSocket = ws;
        this.awaitQuiet(ws);
      });
    };

    ws.onmessage = (ev: MessageEvent) => {
      if (typeof ev.data !== "string") return;
      let msg: Record<string, unknown>;
      try {
        msg = JSON.parse(ev.data) as Record<string, unknown>;
      } catch {
        return;
      }
      // Any frame ends the quiet wait: the owner's side spoke.
      this.wait.stopQuiet();
      switch (msg.type) {
        case "native_ui":
          this.nativeUi.receive(msg.event);
          break;
        case "native_ui_reset":
          this.nativeUi.reset();
          break;
        case "ready":
          // Also a kept socket's later `ready`: the account attached it to
          // the woken machine again with `last_seq` raised, so only the gap
          // follows and the reducer's seq guard drops anything it has.
          this.recon.succeeded();
          this.unknownRetries = 0;
          this.waking = false;
          this.asleep = false;
          this.kept = true;
          // The project answers on this socket: whatever waited for it (a
          // parked socket, a panel that met "asleep" or "unreachable") reads again.
          ownerAwake();
          this.deliveries.push({
            kind: "ready",
            nativeUiGeneration: ++this.nativeUiGeneration,
            session: msg.session as ChatSessionInfo,
            replayFrom: (msg.replay_from as number) ?? 0,
            head: msg.head as number | undefined,
            attach: { sendIds: msg.send_ids === true, reattach: readies++ > 0,
              ...(Array.isArray(msg.active_queued_ids) && msg.active_queued_ids.length <= 64
                && msg.active_queued_ids.every((id) => typeof id === "string" && /^[A-Za-z0-9_-]{8,64}$/.test(id))
                ? { activeQueuedIds: msg.active_queued_ids as string[] } : {}) },
          });
          break;
        case "send_cancelled":
          if (typeof msg.client_id === "string") {
            this.deliveries.push({ kind: "send_cancelled", clientId: msg.client_id, cancelled: msg.cancelled === true });
          }
          break;
        case "send_confirmed":
          if (typeof msg.client_id === "string") {
            this.deliveries.push({ kind: "send_confirmed", clientId: msg.client_id });
          }
          break;
        case "batch":
          this.deliveries.pushMany(
            ((msg.events as SeqEvent[]) ?? []).map((entry) => ({
              kind: "event" as const,
              entry,
            })),
          );
          break;
        case "ev":
          this.deliveries.push({
            kind: "event",
            entry: {
              seq: msg.seq as number,
              ts: msg.ts as number,
              ev: msg.ev as AgentEvent,
            },
          });
          break;
        case "degraded":
          this.ended = true;
          this.resetNativeUi();
          this.deliveries.push({ kind: "degraded" });
          break;
        case "exited":
          this.ended = true;
          this.resetNativeUi();
          this.deliveries.push({
            kind: "exited",
            status: (msg.status as number | null) ?? null,
          });
          break;
        case "moved":
          this.resetNativeUi();
          // Continuing elsewhere: never `ended`. The daemon closes this
          // socket next and the ordinary reconnect follows the new owner.
          // Sends stop here, before that close lands.
          this.authenticatedSocket = null;
          this.asleep = false;
          this.deliveries.push({ kind: "moved", to: movedTo(msg) });
          break;
        case "waking":
          this.resetNativeUi();
          this.asleep = false;
          this.deliveries.push({ kind: "waking" });
          break;
        case "bringing":
          this.resetNativeUi();
          this.asleep = false;
          this.deliveries.push({ kind: "bringing" });
          break;
        case "paused": {
          this.resetNativeUi();
          // Not an exit either: the daemon closes this socket next and the
          // ordinary reconnect finds the conversation once it runs again.
          const pause = parsePause(msg);
          this.authenticatedSocket = null;
          this.asleep = false;
          if (pause !== null) this.deliveries.push({ kind: "paused", pause });
          break;
        }
        case "error":
          // Connection states, never fatal: the socket stays (or reconnects)
          // and the next send carries wake intent.
          if (msg.code === "worker_asleep") {
            this.resetNativeUi();
            // Said by a relay or gateway that keeps no socket for the owner,
            // possibly after this one had counted as kept (a slow answer).
            this.kept = false;
            this.asleep = true;
            this.deliveries.push({ kind: "asleep" });
            break;
          }
          if (msg.code === "remote_unavailable") {
            this.resetNativeUi();
            this.kept = false;
            this.deliveries.push({ kind: "unreachable" });
            // A relay that cannot reach the owner says it is retrying
            // (`reason:"reconnecting"`) and stays silent meanwhile: that
            // lasts until its next frame. Anyone else saying it has handed
            // back what it held for a wake that did not arrive and still
            // keeps this socket: quiet from here on is kept again.
            if (msg.reason !== "reconnecting") this.awaitQuiet(ws);
            break;
          }
          if (msg.code === "workspace_scope_changed") { this.resetNativeUi(); break; }
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
          if (msg.code === "send_uncertain" && typeof msg.client_id === "string") {
            this.deliveries.push({ kind: "send_uncertain", clientId: msg.client_id,
              message: typeof msg.message === "string" ? msg.message : "Delivery could not be confirmed." });
            break;
          }
          if (msg.code === "command_failed" || msg.code === "invalid_command" || msg.code === "read_only") {
            this.deliveries.push({
              kind: "command_failed",
              message: (msg.message as string) ?? "command failed",
              command: typeof msg.command === "string" ? msg.command : null,
              reason: typeof msg.reason === "string" ? msg.reason : null,
              clientId: typeof msg.client_id === "string" ? msg.client_id : null,
            });
            break;
          }
          this.fatal = true;
          this.resetNativeUi();
          this.deliveries.push({
            kind: "error",
            message: (msg.message as string) ?? "unknown error",
          });
          break;
        default:
          break;
      }
    };

    ws.onclose = () => {
      this.resetNativeUi();
      if (this.ws === ws) this.ws = null;
      this.waking = false;
      const kept = this.kept;
      this.kept = false;
      this.wait.stopQuiet();
      if (this.closed || this.fatal || this.ended) {
        this.recon.clear();
        return;
      }
      // Live no longer: the composer must stop claiming the agent hears us and
      // stop clearing drafts into a closed socket until we reconnect.
      this.deliveries.push({ kind: "disconnected" });
      // A project view's placement read may say the owner sleeps before any
      // frame did (a gateway can close without one). That holds for a
      // connection the owner's side refused. One it kept (answered, or open
      // and quiet) and then lost is dialed again first: a keeper that keeps
      // a sleeping machine's sockets takes it, and one that does not refuses
      // that dial, which parks it here.
      if (!this.asleep && !kept && ownerSuspended()) {
        this.asleep = true;
        this.deliveries.push({ kind: "asleep" });
      }
      if (this.asleep) {
        this.waitForOwner();
        return;
      }
      this.recon.schedule();
    };
  }

  /** The owner is asleep: retrying on a backoff would only hear "asleep"
   *  again. Wait with no timer until a send or {@link retrySoon} /
   *  `ownerAwake` dials. */
  private waitForOwner(): void {
    this.recon.cancel();
    this.recon.clear();
    this.wait.park(() => {
      if (!this.closed && !this.fatal && !this.ended && this.ws === null) this.connect();
    });
  }

  /** Authenticated: if nothing is heard for {@link QUIET_OPEN_MS} the socket
   *  is kept open for an owner that has not answered. That is healthy: it
   *  leaves the reconnecting indicator (keeping its backoff, which a later
   *  drop continues) and the view stops counting it as down. */
  private awaitQuiet(ws: WebSocket): void {
    this.wait.awaitQuiet(() => {
      if (this.ws !== ws || this.closed) return;
      this.kept = true;
      this.recon.clear();
      this.deliveries.push({ kind: "held" });
    });
  }

  /** Waiting for a sleeping owner (no socket, no retry timer). */
  get waitingForOwner(): boolean {
    return this.wait.parked;
  }

  /**
   * True while this socket is still a going concern: not deliberately closed,
   * not fatally errored, and not ended (exited/degraded). A pooled socket that
   * is no longer healthy is recreated on the next acquire (its store's lastSeq
   * is preserved, so the re-attach gap-replays from the ring, not from 0).
   */
  get healthy(): boolean {
    return !this.closed && !this.fatal && !this.ended;
  }

  /** Send an AgentCommand frame; false when the socket is not open. An open
   *  socket takes it whether or not its owner has answered: whoever keeps
   *  the connection for a sleeping owner holds an acting command, wakes the
   *  owner and delivers it (or refuses it, which hands a send's text back). */
  send(command: Record<string, unknown>): boolean {
    if (this.ws?.readyState !== WebSocket.OPEN || this.authenticatedSocket !== this.ws) {
      this.wakeOnInput();
      return false;
    }
    this.ws.send(JSON.stringify(command));
    return true;
  }

  /** {@link send} for a frame nobody typed (the store sending an unconfirmed
   *  send again, or withdrawing one): the same frame on the same socket, but
   *  a socket that is not open is simply not written to. It must never redial
   *  with wake intent: only the user acting may wake a machine. */
  sendQuietly(command: Record<string, unknown>): boolean {
    if (this.ws?.readyState !== WebSocket.OPEN || this.authenticatedSocket !== this.ws) return false;
    this.ws.send(JSON.stringify(command));
    return true;
  }

  /**
   * A user action on a browser view whose socket is down, or on any socket
   * waiting for a sleeping owner: reconnect now, once, carrying wake intent,
   * instead of waiting out the backoff. The action itself is NOT queued — the
   * caller keeps it (a composer keeps its draft). An open socket never needs
   * this: whoever keeps it (a native window's daemon, or a keeper that keeps
   * a sleeping cloud machine's sockets) holds the input itself while the
   * owner wakes.
   */
  private wakeOnInput(): void {
    if (!(isBrowserGateway() || this.wait.parked) || this.closed || this.fatal || this.ended || this.waking) return;
    this.waking = true;
    this.resetNativeUi();
    this.recon.cancel();
    if (this.ws !== null) {
      this.ws.onclose = null;
      this.ws.onmessage = null;
      this.ws.close();
      this.ws = null;
    }
    this.connect(true);
  }

  /**
   * The conversation became reachable again (its row stopped being paused,
   * or it now runs somewhere else): retry now instead of sitting out the rest
   * of a backoff that grew while it was paused. No-op while connected.
   */
  retrySoon(): void {
    if (this.closed || this.fatal || this.ended) return;
    // Waiting for a sleeping owner: dial once, passively (a send wakes it).
    if (this.wait.parked) this.connect();
    else this.recon.nudge(0);
  }

  close(): void {
    this.closed = true;
    this.wait.stopPark();
    this.wait.stopQuiet();
    this.resetNativeUi();
    this.recon.cancel();
    this.recon.clear();
    this.deliveries.clear();
    this.ws?.close();
    this.ws = null;
  }
}
