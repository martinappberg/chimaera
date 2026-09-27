/** Host identity is carried by the tab URL, never a shared cookie or storage key. */
export function gatewayPrefix(path = location.pathname): string {
  return /^\/app\/[A-Za-z0-9_-]{1,128}(?:\/|$)/.exec(path)?.[0].replace(/\/$/, "") ?? "";
}
export function isBrowserGateway(): boolean { return gatewayPrefix() !== ""; }
export function workbenchPath(): string { return `${gatewayPrefix()}/`; }
export function daemonPath(path: string): string {
  if (!path.startsWith("/") || path.startsWith("//")) throw new Error("daemon path must be local");
  return `${gatewayPrefix()}${path}`;
}
export function daemonSocketUrl(path: string): string {
  const url = new URL(daemonPath(path), location.origin);
  url.protocol = location.protocol === "https:" ? "wss:" : "ws:";
  return url.href;
}
