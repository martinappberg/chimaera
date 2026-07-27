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
