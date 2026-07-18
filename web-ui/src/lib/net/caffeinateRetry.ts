/**
 * Caffeinate retries only transport-shaped reconnect failures unattended.
 * Keep auth/host-key/config/deploy errors out: those can prompt or make a
 * materially different decision and must stay on the manual Retry path.
 */
export function retryableCaffeinateError(message: string, online: boolean): boolean {
  if (!online) return true;
  const text = message.toLowerCase();
  return [
    "could not resolve hostname",
    "name or service not known",
    "nodename nor servname",
    "network is unreachable",
    "no route to host",
    "connection timed out",
    "operation timed out",
    "connection refused",
    "connection closed",
    "connection reset",
    "broken pipe",
    "ssh tunnel exited early",
    "tunnel did not come up",
  ].some((needle) => text.includes(needle));
}
