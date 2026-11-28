/**
 * An agent's page imports a plugin library (`chimaera:lib/<name>/…`) whose
 * plugin is installed but off in the page's workspace: the daemon names it
 * before the page API, the page API reports it, and the card or pane offers
 * "This page uses <plugin>. Turn it on here". The page's word is only an
 * id: the name shown and the switch come from the app's own plugin list,
 * which App registers here (a page can't name a plugin that isn't one).
 */

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
