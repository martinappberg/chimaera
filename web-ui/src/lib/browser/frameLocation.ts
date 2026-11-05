/**
 * Where a proxied page is, in the app's own terms: the frame's (or native
 * view's) location minus the proxy prefix, and the address an agent is told
 * about. Pure, so both transports read a location the same way.
 */

/** The parts of a `Location` (or `URL`) this reads. */
export interface LocationParts {
  protocol: string;
  pathname: string;
  search: string;
  hash: string;
}

/**
 * The in-app path (path + query + hash) of a page at `loc`, under the proxy
 * session `base` (`/proxy/{id}`); null when it isn't this app's page.
 * Prefixed navigations carry `base`; rescued (absolute-path) ones land on
 * root-form paths, and both name the same app location. The initial
 * about:blank document has pathname "blank" (recording it would re-point
 * the tab), and a `/proxy/` path under a DIFFERENT id is the previous
 * target's page still on screen mid-retarget.
 */
export function inAppPath(loc: LocationParts, base: string): string | null {
  if (loc.protocol === "about:") return null;
  // The hash is kept so a target path that carried one (a hash-router
  // route) matches, rather than reading as a navigation on every settle.
  const raw = `${loc.pathname}${loc.search}${loc.hash}`;
  if (raw.startsWith("/proxy/") && !raw.startsWith(base)) return null;
  return raw.startsWith(base) ? raw.slice(base.length) || "/" : raw;
}

/** Whether the page sits on a root-form (rescued) path rather than under
 *  its proxy prefix: reloading that URL as a document is not proxied. */
export function onRootForm(loc: LocationParts, base: string): boolean {
  return loc.protocol !== "about:" && !`${loc.pathname}${loc.search}${loc.hash}`.startsWith(base);
}

/**
 * The page's address as an agent should read it: the app's own
 * `http://host:port/path`, never the proxy URL (its ticket is not the
 * agent's business) and without the query, which can carry a credential
 * (Jupyter's `?token=`). The hash stays: a client-side route is a place.
 */
export function pageAddress(host: string, port: number, inApp: string): string {
  const h = host.includes(":") && !host.startsWith("[") ? `[${host}]` : host;
  const hashAt = inApp.indexOf("#");
  const beforeHash = hashAt === -1 ? inApp : inApp.slice(0, hashAt);
  const hash = hashAt === -1 ? "" : inApp.slice(hashAt);
  const q = beforeHash.indexOf("?");
  const pathOnly = q === -1 ? beforeHash : beforeHash.slice(0, q);
  const path = pathOnly.startsWith("/") ? pathOnly : `/${pathOnly}`;
  return `http://${h}:${port}${path}${hash}`;
}
