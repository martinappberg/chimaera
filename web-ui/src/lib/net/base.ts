/** Logical workspace or explicit host identity belongs to each tab URL. */
export function gatewayPrefix(path = location.pathname): string {
  return /^\/(?:app|workspace)\/[A-Za-z0-9_-]{1,128}(?:\/|$)/.exec(path)?.[0].replace(/\/$/, "") ?? "";
}
export function gatewayWorkspace(path = location.pathname): string | null {
  return /^\/workspace\/([A-Za-z0-9_-]{1,128})(?:\/|$)/.exec(path)?.[1] ?? null;
}
export function isBrowserGateway(): boolean { return gatewayPrefix() !== ""; }
/** The account's own signed-in page at `/`, the web's Home: the account
 *  serves this UI there with a `chimaera-surface` marker. No daemon stands
 *  behind it; only project views (`/workspace/{id}/`) and host views have one. */
export function isAccountHome(): boolean {
  if (typeof document === "undefined" || gatewayPrefix() !== "") return false;
  return document.querySelector('meta[name="chimaera-surface"]')?.getAttribute("content") === "account-home";
}
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
