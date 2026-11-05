/**
 * The window's side of pane views that the always-loaded shell needs: where
 * they are possible, and the one `native-view` event router. Split from
 * `paneView.ts` so the app shell (App, the tab bar, the agent browser's
 * runtime) doesn't load the whole placement action into its entry bundle.
 */

import { isNativeShell, listenNativeView } from "../net/native";
import { decodeNativeViewEvent, type NativeViewEvent } from "./paneViewBridge";

// One `native-view` listener per window, routing by view id. It stays for
// the window's life: one idle callback, and no subscribe/unsubscribe race
// when the last view closes as the next one opens.
const routes = new Map<string, (ev: NativeViewEvent) => void>();
let listening = false;

/** Receive the `native-view` events of view `id` (the last route set for an
 *  id wins). Returns the unroute, which removes only its own route. */
export function routeNativeView(id: string, fn: (ev: NativeViewEvent) => void): () => void {
  routes.set(id, fn);
  if (!listening) {
    listening = true;
    void listenNativeView((raw) => {
      const ev = decodeNativeViewEvent(raw);
      if (ev !== null) routes.get(ev.id)?.(ev);
    }).catch(() => (listening = false));
  }
  return () => {
    if (routes.get(id) === fn) routes.delete(id);
  };
}

/**
 * Whether pane views are worth trying here: the macOS app (the shell builds
 * them on WKWebView). The Linux and Windows apps go straight to the iframe
 * instead of paying a failing call per page.
 */
export function paneViewsPossible(): boolean {
  return isNativeShell() && /Mac/.test(navigator.userAgent);
}

/** The window asks the page view in pane `paneId` to zoom (⌘+/⌘−, the
 *  reset chord): +1 in, -1 out, 0 back to 100 %. `paneView` listens; a pane
 *  without a live view ignores it. */
export const PANE_ZOOM_EVENT = "chimaera:pane-zoom";

export interface PaneZoom {
  paneId: string;
  step: 1 | -1 | 0;
}

export function zoomPaneView(paneId: string, step: 1 | -1 | 0): void {
  window.dispatchEvent(new CustomEvent<PaneZoom>(PANE_ZOOM_EVENT, { detail: { paneId, step } }));
}
