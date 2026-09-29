/**
 * The native shell's Reload Window (View menu, ⌘R / F5 — see
 * crates/chimaera-app/src/menu.rs) evaluates a fixed script in the focused
 * window that calls `window.__chimaeraReloadWindow`. main.ts installs it
 * before App mounts, so a window whose App never booted still reloads; once
 * App's asset-navigation gate runs, the reload goes through that gate.
 */

import { requestWindowReload } from "./assetTransition";

declare global {
  interface Window {
    __chimaeraReloadWindow?: () => void;
  }
}

let gateLive = false;
let lastPlainReload = Number.NEGATIVE_INFINITY;

/** App claims reloads once its navigation effect is mounted. */
export function claimWindowReload(): () => void {
  gateLive = true;
  return () => {
    gateLive = false;
  };
}

/** Reload this window: through App's safety gate when it runs; plainly when
 *  App never mounted, which leaves no edits or drafts to lose. A held key
 *  repeats the menu item, and every re-issued reload cancels the load in
 *  flight, so the plain path ignores repeats within a second (the gate is
 *  idempotent on its own). */
export function reloadWindow(): void {
  if (gateLive) {
    requestWindowReload();
    return;
  }
  const now = performance.now();
  if (now - lastPlainReload < 1000) return;
  lastPlainReload = now;
  location.reload();
}

export function installReloadHook(): void {
  window.__chimaeraReloadWindow = reloadWindow;
}
