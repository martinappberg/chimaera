import { sendSocketAuth } from "./placement";
import { daemonSocketUrl, gatewayWorkspace, isBrowserGateway } from "./base";
import { getToken } from "./api";
import { nudgeReconnectors, ownerAwake, retryDelayMs } from "./reconnect";
import { noteServed } from "./projectMoving";
import type { Link } from "../workspace/agentLinks";
import type { Session } from "../workspace/sessions";
import type { Notice } from "../workspace/notices";
import type { PlatformFrame } from "../plugins/platform";
import { parseAgentBrowserOpen, type AgentBrowserOpen } from "../browser/agentOpen";
import { parseUpdateStatus, type UpdateStatus } from "../workspace/update.svelte";

const INITIAL_BACKOFF_MS = 500;
const MAX_BACKOFF_MS = 10_000;
/** Error codes that describe a project connection in motion, never a
 *  rejected socket: reconnect instead of giving up. */
const RECONNECTING_CODES = new Set(["remote_unavailable", "workspace_scope_changed", "worker_asleep"]);

export interface EventsSocketHandlers {
  /**
   * Full session-list snapshot pushed by the daemon; `links` rides the same
   * frame (undefined from a daemon predating linked terminals).
   */
  onSessions(sessions: Session[], links?: Link[]): void;
  /**
   * Full settings map (settings.json ground truth), pushed after auth and
   * again whenever it changes — a PUT from any window or a hand-edit of the
   * file on disk.
   */
  onSettings?(settings: Record<string, unknown>): void;
  /**
   * Per-workspace git epoch map (invalidate-and-pull): fired after auth and
   * whenever any workspace's git state may have changed. The caller refetches
   * `GET /git/status` for its active workspace iff that workspace's epoch moved.
   */
  /** `repos` (additive): per workspace, each repository's own epoch. */
  onGit?(
    epochs: Record<string, number>,
    repos?: Record<string, Record<string, number>>,
  ): void;
  /**
   * Per-workspace Timeline epoch map (the git idiom): fired after auth and
   * whenever a workspace's timeline gained an entry. The caller refetches
   * `GET /workspaces/{id}/timeline?since=` for its active workspace iff that
   * workspace's epoch moved.
   */
  onTimeline?(epochs: Record<string, number>): void;
  /**
   * Per-workspace agent-communication epoch map (the timeline frame's shape):
   * fired whenever a workspace's unread counts or wake requests changed. The
   * caller refetches `GET /workspaces/{id}/comms` for its active workspace
   * iff that workspace's epoch moved.
   */
  onComms?(epochs: Record<string, number>): void;
  /**
   * The daemon's release knowledge (same shape as GET /api/v1/update),
   * pushed after auth and whenever it changes.
   */
  onUpdate?(status: UpdateStatus): void;
  /**
   * Recents invalidate (a conversation retired somewhere): fired after auth
   * and whenever the store changes. The caller refetches GET /recents for
   * its own workspace iff the epoch moved.
   */
  onRecents?(epoch: number): void;
  /** An agent-plugin install or hook-trust write invalidated the reports. */
  onAgentPlugins?(epoch: number): void;
  /** Exact mounted paths whose disk metadata/listing changed. */
  onFs?(change: {
    files: string[];
    removed: string[];
    dirs: string[];
    removedDirs: string[];
  }): void;
  /**
   * Discrete alerts (an agent finished, needs you, or sent a message) since
   * this socket connected — the browser's notification source. Never
   * replayed across reconnects: a reload must not re-alert old news.
   */
  onNotices?(notices: Notice[]): void;
  /**
   * A plugin frame for this window's workspace: a view to render again
   * (`view`), published data to fetch again (`surface`), or a plugin's own
   * `emit` (`plugin`).
   */
  onPlatform?(frame: PlatformFrame): void;
  /**
   * An agent asked for a browser pane (the MCP `open_browser` tool): sent to
   * every window connected at that moment, never replayed; the window
   * decides whether it is the one to act (`browser/agentOpen.ts`).
   */
  onBrowserOpen?(open: AgentBrowserOpen): void;
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
  settings?: Record<string, unknown>;
  epochs?: Record<string, number>;
  /** The git frame's per-repository epochs (workspace → top level → epoch). */
  repos?: Record<string, Record<string, number>>;
  epoch?: number;
  /** An `update` frame's discriminator; the rest is `parseUpdateStatus`'s. */
  available?: boolean;
  message?: string;
  code?: string;
  files?: string[];
  removed?: string[];
  dirs?: string[];
  removed_dirs?: string[];
  notices?: Notice[];
}

/**
 * The daemon-wide events socket, per the /ws/events contract: auth text
 * frame ({"type":"auth","token"}) -> {"type":"sessions","sessions":[...]}
 * full snapshots, re-sent whenever any session appears/disappears or changes
 * state/title/name. Replaces the sessions poll while connected; reconnects
 * forever with exponential backoff on unclean closes.
 *
 * An open socket that says nothing is healthy, however long: no timer watches
 * for silence and nothing is retried. That matters in a browser view behind a
 * keeper that keeps a sleeping cloud machine's sockets open (VIEWING.md, "A
 * sleeping cloud machine's sockets") and attaches them again by itself once
 * the machine wakes: the machine's first frames after each attach are the
 * same full snapshots as after a connect, so state is fresh again, and the
 * `settings` frame among them re-sends this window's registration (it lives
 * on the daemon's side of one attach).
 */
export class EventsSocket {
  private ws: WebSocket | null = null;
  private authenticatedSocket: WebSocket | null = null;
  private closed = false;
  private fatal = false;
  /** A fatal socket has been revived by the health cross-nudge (once ever —
   *  a daemon that fatals every revival must not turn health ticks into a
   *  reconnect loop). */
  private fatalRevived = false;
  private connected = false;
  private backoffMs = INITIAL_BACKOFF_MS;
  /** Consecutive connect failures (the hidden-floor grace counter). */
  private attempts = 0;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  /** The workspace this window shows; re-sent after every (re)connect. */
  private watching: string | null = null;
  /** Mounted previews + visible listings. The daemon caps both arrays. */
  private watchedFiles: string[] = [];
  private watchedDirs: string[] = [];
  private watchedRepos: string[] = [];

  constructor(private readonly handlers: EventsSocketHandlers) {
    // Reconnect delays take the slow tier while the document is hidden
    // (see scheduleReconnect); this catch-up keeps that tier from delaying
    // recovery once someone is looking again.
    if (typeof document !== "undefined") {
      document.addEventListener("visibilitychange", this.onVisibility);
    }
    this.connect();
  }

  /** Visibility return with a retry pending: probe NOW, not in up to 60s. */
  private readonly onVisibility = (): void => {
    if (document.visibilityState !== "visible" || this.retryTimer === null) return;
    clearTimeout(this.retryTimer);
    this.retryTimer = null;
    this.connect();
  };

  /**
   * The health cross-nudge: a SUCCESSFUL /health probe while this socket is
   * down proves the daemon is reachable — pull a pending retry in to now
   * instead of letting two independent 60s clocks ignore each other. Also
   * revives a fatal socket exactly once (a server "error" frame gave up
   * permanently; HTTP auth succeeding afterwards is strong evidence the
   * fatal was transient — a restarting daemon mid-handshake). A closed
   * socket stays closed.
   */
  retryNow(): void {
    if (this.closed) return;
    if (this.fatal) {
      if (this.fatalRevived) return;
      this.fatalRevived = true;
      this.fatal = false;
      this.connect();
      return;
    }
    if (this.retryTimer === null) return;
    clearTimeout(this.retryTimer);
    this.retryTimer = null;
    this.connect();
  }

  /**
   * Tell the daemon which workspace this window is looking at. That registration
   * — not "pulled recently" — is what gates the daemon's git backstop poll, so a
   * quiet repo keeps being watched while a window is open, and nothing is polled
   * once every window is closed.
   */
  watch(workspaceId: string | null): void {
    this.watching = workspaceId;
    this.sendWatch();
  }

  watchFs(files: string[], dirs: string[]): void {
    this.watchedFiles = [...files];
    this.watchedDirs = [...dirs];
    this.sendWatch();
  }

  /** The repositories below the root this window watches (sections open,
   *  files mounted): only these ride the daemon's git backstop. */
  watchGitRepos(repos: string[]): void {
    const next = [...repos].sort();
    if (next.join("\n") === this.watchedRepos.join("\n")) return;
    this.watchedRepos = next;
    this.sendWatch();
  }

  private sendWatch(): void {
    if (this.ws?.readyState !== WebSocket.OPEN || this.authenticatedSocket !== this.ws) return;
    this.ws.send(
      JSON.stringify({
        type: "watch",
        workspace_id: this.watching,
        files: this.watchedFiles,
        dirs: this.watchedDirs,
        git_repos: this.watchedRepos,
      }),
    );
  }

  private connect(): void {
    if (this.closed) return;
    const ws = new WebSocket(daemonSocketUrl("/ws/events"));
    this.ws = ws;

    ws.onopen = () => {
      sendSocketAuth(ws, { type: "auth", token: getToken() ?? "" },
        () => this.ws === ws && !this.closed, () => {
          this.authenticatedSocket = ws;
          // Re-assert interest only after the scoped authentication frame.
          this.sendWatch();
        });
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
      } else if (
        msg.type === "settings" &&
        typeof msg.settings === "object" &&
        msg.settings !== null
      ) {
        this.backoffMs = INITIAL_BACKOFF_MS;
        this.handlers.onSettings?.(msg.settings);
        // A daemon sends its settings once per attach (and when they change,
        // which is rare), and a keeper that kept this socket open across
        // its machine's sleep has just attached it afresh: register again.
        // The first one after a connect repeats the registration sent with
        // authentication, which may not have reached a sleeping machine;
        // repeating an unchanged one changes nothing on the daemon. Only a
        // gateway view's socket can be attached twice.
        if (isBrowserGateway()) {
          this.sendWatch();
          // The project's owner answers: whatever waited for it reads again.
          if (gatewayWorkspace() !== null) ownerAwake();
        }
      } else if (
        msg.type === "git" &&
        typeof msg.epochs === "object" &&
        msg.epochs !== null
      ) {
        this.backoffMs = INITIAL_BACKOFF_MS;
        this.handlers.onGit?.(
          msg.epochs,
          typeof msg.repos === "object" && msg.repos !== null ? msg.repos : undefined,
        );
      } else if (
        msg.type === "timeline" &&
        typeof msg.epochs === "object" &&
        msg.epochs !== null
      ) {
        this.backoffMs = INITIAL_BACKOFF_MS;
        this.handlers.onTimeline?.(msg.epochs);
      } else if (
        msg.type === "comms" &&
        typeof msg.epochs === "object" &&
        msg.epochs !== null
      ) {
        this.backoffMs = INITIAL_BACKOFF_MS;
        this.handlers.onComms?.(msg.epochs);
      } else if (msg.type === "update" && typeof msg.available === "boolean") {
        this.backoffMs = INITIAL_BACKOFF_MS;
        const status = parseUpdateStatus(msg);
        if (status !== null) this.handlers.onUpdate?.(status);
      } else if (msg.type === "recents" && typeof msg.epoch === "number") {
        this.backoffMs = INITIAL_BACKOFF_MS;
        this.handlers.onRecents?.(msg.epoch);
      } else if (msg.type === "agent_plugins" && typeof msg.epoch === "number") {
        this.backoffMs = INITIAL_BACKOFF_MS;
        this.handlers.onAgentPlugins?.(msg.epoch);
      } else if (msg.type === "fs") {
        this.backoffMs = INITIAL_BACKOFF_MS;
        this.handlers.onFs?.({
          files: Array.isArray(msg.files) ? msg.files.filter(isString) : [],
          removed: Array.isArray(msg.removed) ? msg.removed.filter(isString) : [],
          dirs: Array.isArray(msg.dirs) ? msg.dirs.filter(isString) : [],
          removedDirs: Array.isArray(msg.removed_dirs)
            ? msg.removed_dirs.filter(isString)
            : [],
        });
      } else if (
        (msg.type === "view" || msg.type === "surface" || msg.type === "plugin" || msg.type === "job") &&
        typeof (msg as { plugin?: unknown }).plugin === "string"
      ) {
        this.handlers.onPlatform?.(msg as unknown as PlatformFrame);
      } else if (msg.type === "notices" && Array.isArray(msg.notices)) {
        this.backoffMs = INITIAL_BACKOFF_MS;
        this.handlers.onNotices?.(msg.notices);
      } else if (msg.type === "browser_open") {
        const open = parseAgentBrowserOpen(msg);
        if (open !== null) this.handlers.onBrowserOpen?.(open);
      } else if (msg.type === "error") {
        // A project's connection changing (it moved, or its owner is
        // unreachable) is not a rejection: the daemon closes this socket and
        // the ordinary reconnect below picks up the new route.
        if (msg.code !== undefined && RECONNECTING_CODES.has(msg.code)) return;
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
    if (up) this.attempts = 0; // a real frame arrived: grace restored
    if (this.connected !== up) {
      this.connected = up;
      this.handlers.onStatus(up);
      // The daemon is demonstrably reachable again: nudge every session
      // socket sitting out a backoff (including the hidden slow tier) — one
      // recovery signal instead of ~20 independent probe schedules. The
      // nudge is deferred/spread/damped inside nudgeReconnectors, so this
      // message handler finishes applying the snapshot first and a
      // crash-looping daemon can't turn its flaps into connect herds.
      if (up) nudgeReconnectors();
      // In a project view this socket reaches the project's owner itself:
      // it answering means a sleeping owner woke, so parked sockets dial.
      // It answering is also the project's machine serving it: a move it
      // was on has arrived.
      if (up && gatewayWorkspace() !== null) { ownerAwake(); noteServed(); }
    }
  }

  private scheduleReconnect(): void {
    // Jitter + the hidden slow tier (shared with the per-session sockets):
    // a resumed laptop with a dead tunnel must not probe 12x/min per window
    // forever while nobody is even looking. The first two retries keep the
    // fast backoff even hidden (grace attempts — a transient blip must not
    // cost a minute); onVisibility and the health cross-nudge (retryNow)
    // restore the fast path with an immediate attempt.
    const hidden =
      typeof document !== "undefined" && document.visibilityState === "hidden";
    this.attempts += 1;
    this.retryTimer = setTimeout(
      () => {
        this.retryTimer = null;
        this.connect();
      },
      retryDelayMs(this.backoffMs, hidden, this.attempts),
    );
    this.backoffMs = Math.min(this.backoffMs * 2, MAX_BACKOFF_MS);
  }

  /** Permanently close the socket (no reconnect). */
  close(): void {
    this.closed = true;
    if (typeof document !== "undefined") {
      document.removeEventListener("visibilitychange", this.onVisibility);
    }
    if (this.retryTimer !== null) {
      clearTimeout(this.retryTimer);
      this.retryTimer = null;
    }
    this.ws?.close();
    this.ws = null;
  }
}

function isString(value: unknown): value is string {
  return typeof value === "string";
}
