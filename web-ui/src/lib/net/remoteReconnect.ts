import type { HostStatus, HostStatusEvent } from "./native";

export type RemoteReconnectSurface = "hidden" | "status" | "failure" | "retry";

interface RemoteReconnectViewState {
  /** The automatic status or blocking failure dialog has not been dismissed. */
  open: boolean;
  /** The most recent native connect failure, if the attempt failed. */
  error: string | null;
  /** This native window is still rejected by its remote daemon. */
  authBlocked: boolean;
}

export interface ReconnectListenerGate {
  /** Settles only after the native host-status listener is attached. */
  ready: Promise<void>;
  attached: () => void;
  failed: (reason: unknown) => void;
}

/**
 * Gate an SSH reconnect on its non-replayed native status listener.
 *
 * The shell emits `host-status: connected` before its connect command
 * resolves. A stale-token page can discover its 401 while Tauri is still
 * attaching the asynchronous listener; starting the command in that gap
 * loses the endpoint/token event and strands the page on stale credentials.
 */
export function createReconnectListenerGate(): ReconnectListenerGate {
  let attached!: () => void;
  let failed!: (reason: unknown) => void;
  const ready = new Promise<void>((resolve, reject) => {
    attached = resolve;
    failed = reject;
  });
  // The listener can fail before a reconnect needs the gate. Mark the
  // rejection observed while preserving it for a later `await ready`.
  void ready.catch(() => {});
  return { ready, attached, failed };
}

/**
 * Select the reconnect presentation without allowing a failed native remote
 * window to lose its only recovery action. Dismissal downgrades a failure to
 * an ambient Retry; an unauthorized window also retains Retry while its SSH
 * attempt is hidden or between attempts.
 */
export function selectRemoteReconnectSurface({
  open,
  error,
  authBlocked,
}: RemoteReconnectViewState): RemoteReconnectSurface {
  if (open) return error === null ? "status" : "failure";
  if (error !== null || authBlocked) return "retry";
  return "hidden";
}

/** What one `host-status` event means for this window, as `Connection` acts on it. */
export type HostStatusAction =
  /** Not this window's tunnel, not a native remote window, or a status that
   *  changes nothing here (a connect's own failure surfaces through its call). */
  | { kind: "ignore" }
  /** A job window's job left the queue: nothing to reconnect to. */
  | { kind: "jobEnded"; reason: string | null }
  /** The shell confirmed the forward is dead: reconnect now. */
  | { kind: "reconnect"; reason: string }
  /** No tunnel will come back on its own: keep the failure dialog with Retry. */
  | { kind: "stranded"; message: string }
  /** The tunnel is back. `moved` = the origin or token changed, so the window
   *  must re-home to `port` with `token` (null omits it); otherwise the heal
   *  is in place. */
  | { kind: "healed"; port: number | string; token: string | null; moved: boolean };

export interface HostStatusContext {
  /** A native remote window (only the shell can re-run ssh). */
  canReconnect: boolean;
  /** The key this window's tunnel reports under (composite for a job window). */
  statusAlias: string;
  /** The login host alias, for messages. */
  hostAlias: string;
  isJobWindow: boolean;
  /** This job window's job already ended. */
  jobEnded: boolean;
  /** The current origin's port (`location.port`). */
  port: string;
  /** This window's own daemon token. */
  token: string | null;
}

/** The window's daemon became a cluster that runs workspaces as jobs: no
 *  login-node daemon or tunnel is left for this window to heal onto. */
export function clusterStrandedMessage(alias: string): string {
  return `${alias} now runs workspaces as jobs. Open this one again from its page in Home.`;
}

/**
 * Decide what a `host-status` event does to this window. Only `connected`
 * heals; `cluster` and any status this build does not know leave the window
 * unhealed, so an overlay never closes onto an origin with no tunnel behind it.
 */
export function hostStatusAction(e: HostStatusEvent, ctx: HostStatusContext): HostStatusAction {
  if (!ctx.canReconnect || e.alias !== ctx.statusAlias) return { kind: "ignore" };
  const status: string = e.status;
  if (status === "ended") {
    // Only a job window's composite key ends; a "down" without it is still
    // just a connection blip.
    return ctx.isJobWindow ? { kind: "jobEnded", reason: e.reason ?? null } : { kind: "ignore" };
  }
  if (ctx.jobEnded) return { kind: "ignore" };
  switch (status) {
    case "down":
      return { kind: "reconnect", reason: e.reason ?? "The remote connection stopped responding." };
    case "cluster":
      return { kind: "stranded", message: clusterStrandedMessage(ctx.hostAlias) };
    case "connected": {
      const port = e.local_port;
      // Compute-status events carry no token (compute tokens stay in the
      // shell's Rust side); a rebuilt job tunnel lands on the SAME daemon,
      // so this window's own token still holds — carry it across the origin
      // move ourselves.
      const token = e.token ?? (ctx.isJobWindow ? ctx.token : null);
      const portMoved = port !== null && String(port) !== ctx.port;
      // A login window's tunnel on a new port always brings its token; a
      // copy without one is the public status a window mid-navigation gets
      // (its next document receives the full one), never a URL to re-home to.
      if (portMoved && token === null) return { kind: "ignore" };
      const tokenMoved = token !== null && token !== ctx.token;
      return { kind: "healed", port: port ?? ctx.port, token, moved: portMoved || tokenMoved };
    }
    default:
      // "error": the attempt that failed reports through its own call.
      return { kind: "ignore" };
  }
}

/**
 * The failure a resolved `connect_host` still means for a remote window. The
 * shell resolves a cluster verdict as success (and a joiner of that flight
 * gets no event at all), so anything but `connected` must keep the dialog.
 */
export function connectResultError(
  result: { status: HostStatus; error: string | null },
  alias: string,
): string | null {
  const status: string = result.status;
  if (status === "connected") return null;
  if (status === "cluster") return clusterStrandedMessage(alias);
  return result.error ?? `${alias} did not connect.`;
}
