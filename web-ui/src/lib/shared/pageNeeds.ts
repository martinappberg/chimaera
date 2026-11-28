/**
 * What a host that shows an agent's page inside its navigation guard (an
 * app view, a guarded frame) asks the daemon for, with its bearer token,
 * before it loads the page (`POST /fs/page-key`): the page's key to read
 * the files beside it (`chimaera-key` in its URL), and the plugins lending
 * a library it imports that are off where it is. Those the host offers as
 * "This page uses <plugin>. Turn it on here", by the name and switch the
 * app's own plugin list gives (App registers the lookup): the daemon names
 * them, never the page.
 */
import { api } from "../net/api";

export interface PageAccess {
  key: string | null;
  needs: string[];
}

/** The `/raw` ticket of a page URL (`/raw/{ticket}[/…]`), if it is one. */
export function rawTicket(url: string): string | null {
  return /^\/raw\/([^/?#]+)/.exec(url)?.[1] ?? null;
}

/** The page's key and needs; no key (it reads nothing) when the daemon
 *  can't say (an older daemon, a refused ticket). */
export async function fetchPageAccess(url: string): Promise<PageAccess> {
  const ticket = rawTicket(url);
  if (ticket === null) return { key: null, needs: [] };
  try {
    const res = await api("/fs/page-key", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ ticket }),
    });
    if (!res.ok) return { key: null, needs: [] };
    const b = (await res.json()) as { key?: unknown; needs?: unknown };
    const needs = Array.isArray(b.needs)
      ? [...new Set(b.needs.flatMap((n) => (typeof n === "object" && n !== null && typeof (n as { plugin?: unknown }).plugin === "string" ? [(n as { plugin: string }).plugin] : [])))]
      : [];
    return { key: typeof b.key === "string" ? b.key : null, needs };
  } catch {
    return { key: null, needs: [] };
  }
}

export interface PluginOffer {
  /** The plugin's own name, from the catalog (never the page's). */
  name: string;
  /** Switch it on in the active workspace. */
  turnOn(): Promise<void>;
}

type Lookup = (pluginId: string) => PluginOffer | null;

let lookup: Lookup | null = null;

export function registerPluginOffers(l: Lookup): () => void {
  lookup = l;
  return () => {
    if (lookup === l) lookup = null;
  };
}

/** The offer for a plugin a page needs: null when it is unknown here,
 *  already on, or nothing registered. */
export function pluginOffer(pluginId: string): PluginOffer | null {
  return lookup?.(pluginId) ?? null;
}
