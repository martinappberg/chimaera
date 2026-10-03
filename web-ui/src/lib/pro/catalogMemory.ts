import { gatewayPrefix } from "../net/base";
import { isNativeShell, type CloudProviderStatus, type RememberedProvider } from "../net/native";
import { rememberedRows } from "./providers";

/** A browser view's memory of its cloud's last provider catalog read, so the
 * connections section shows the last known rows at once, as the app does
 * natively (`pro_cloud_status` `remembered_providers`). Kept per cloud
 * address: a page's gateway prefix (`/app/{host}` or `/workspace/{id}`)
 * belongs to one account's cloud, so another account signed in to this
 * browser never sees it, and signing out here forgets it all. A hint only:
 * it never confirms a connection or enables anything but Connect. Storage can
 * be missing or refuse (a private window); every access is guarded and the
 * page works without it. */
const PREFIX = "chimaera.pro.catalog:";

function key(): string | null {
  if (isNativeShell()) return null;
  const scope = gatewayPrefix();
  return scope ? `${PREFIX}${scope}` : null;
}

export function recallCatalog(): CloudProviderStatus[] | null {
  const name = key();
  if (name === null) return null;
  try { return rememberedRows(JSON.parse(localStorage.getItem(name) ?? "null")); } catch { return null; }
}

/** Remembers a fresh catalog's rows (only the fields rendering needs). */
export function rememberCatalog(providers: CloudProviderStatus[]): void {
  const name = key();
  if (name === null) return;
  const rows: RememberedProvider[] = providers.slice(0, 16).map(({ id, label, category, state, methods, disconnect_supported }) => ({
    id, label, category, state, methods: methods.slice(0, 8), ...(disconnect_supported === undefined ? {} : { disconnect_supported }),
  }));
  try {
    if (rows.length) localStorage.setItem(name, JSON.stringify(rows));
    else localStorage.removeItem(name);
  } catch { /* storage refused: the page reads live rows as before */ }
}

/** Forgets every cloud's remembered rows in this browser (on sign-out). */
export function forgetCatalogs(): void {
  try {
    for (let index = localStorage.length - 1; index >= 0; index -= 1) {
      const name = localStorage.key(index);
      if (name?.startsWith(PREFIX)) localStorage.removeItem(name);
    }
  } catch { /* nothing stored, or storage refused */ }
}
