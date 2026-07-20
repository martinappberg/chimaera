import { getToken } from "./api";
import type { Link } from "../workspace/agentLinks";
import type { Session } from "../workspace/sessions";
import type { UpdateStatus } from "../workspace/update.svelte";

const INITIAL_BACKOFF_MS = 500;
const MAX_BACKOFF_MS = 10_000;

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
  onGit?(epochs: Record<string, number>): void;
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
  /** Exact mounted paths whose disk metadata/listing changed. */
  onFs?(change: {
    files: string[];
    removed: string[];
    dirs: string[];
    removedDirs: string[];
  }): void;
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
  epoch?: number;
  available?: boolean;
  current?: string;
  build?: string;
  latest?: UpdateStatus["latest"];
  message?: string;
  files?: string[];
  removed?: string[];
  dirs?: string[];
  removed_dirs?: string[];
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
  /** The workspace this window shows; re-sent after every (re)connect. */
  private watching: string | null = null;
  /** Mounted previews + visible listings. The daemon caps both arrays. */
  private watchedFiles: string[] = [];
  private watchedDirs: string[] = [];

  constructor(private readonly handlers: EventsSocketHandlers) {
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

  private sendWatch(): void {
    if (this.ws?.readyState !== WebSocket.OPEN) return;
    this.ws.send(
      JSON.stringify({
        type: "watch",
        workspace_id: this.watching,
        files: this.watchedFiles,
        dirs: this.watchedDirs,
      }),
    );
  }

  private connect(): void {
    if (this.closed) return;
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const ws = new WebSocket(`${proto}://${location.host}/ws/events`);
    this.ws = ws;

    ws.onopen = () => {
      ws.send(JSON.stringify({ type: "auth", token: getToken() ?? "" }));
      // Re-assert interest: a reconnect starts a fresh watcher registration.
      this.sendWatch();
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
      } else if (
        msg.type === "git" &&
        typeof msg.epochs === "object" &&
        msg.epochs !== null
      ) {
        this.backoffMs = INITIAL_BACKOFF_MS;
        this.handlers.onGit?.(msg.epochs);
      } else if (msg.type === "update" && typeof msg.available === "boolean") {
        this.backoffMs = INITIAL_BACKOFF_MS;
        this.handlers.onUpdate?.({
          current: msg.current ?? "",
          build: msg.build ?? null,
          available: msg.available,
          latest: msg.latest ?? null,
        });
      } else if (msg.type === "recents" && typeof msg.epoch === "number") {
        this.backoffMs = INITIAL_BACKOFF_MS;
        this.handlers.onRecents?.(msg.epoch);
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

function isString(value: unknown): value is string {
  return typeof value === "string";
}
