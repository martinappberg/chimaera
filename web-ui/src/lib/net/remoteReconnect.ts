export type RemoteReconnectSurface = "hidden" | "status" | "failure" | "retry";

interface RemoteReconnectViewState {
  /** The automatic status or blocking failure dialog has not been dismissed. */
  open: boolean;
  /** The most recent native connect failure, if the attempt failed. */
  error: string | null;
  /** This native window is still rejected by its remote daemon. */
  authBlocked: boolean;
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
