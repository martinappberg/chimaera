/**
 * Lightweight facade for the xterm pool. App needs terminal coordination on
 * every route, but the xterm/WebGL runtime belongs only to mounted terminal
 * panes. Synchronous reads/fallbacks stay synchronous; mutations queue behind
 * the one cached dynamic import without making the home screen download it.
 */

import type { LinkContext, PathKind } from "./links";
import type { UrlTarget } from "./urlLinks";
import { baseFontSize, estimateSize } from "./terminalMetrics";

export { baseFontSize, estimateSize };

export interface PoolHandlers {
  onTitle(id: string, title: string): void;
  onExited(id: string, status: number | null): void;
  onSocketError(id: string, message: string): void;
  onSelection(id: string, text: string): void;
  onPaste(id: string, text: string): void;
  linkContext(id: string): LinkContext;
  onOpenPath(id: string, path: string, kind: PathKind, newSplit: boolean): void;
  /** A proxyable URL link was activated: open it in a browser pane. */
  onOpenUrl(id: string, target: UrlTarget, newSplit: boolean): void;
  /** Right-click on a URL link: the shared Chimaera/Browser/Copy menu. */
  onUrlMenu(event: MouseEvent, url: string): void;
}

type Runtime = typeof import("./termPoolRuntime");

let runtime: Runtime | null = null;
let request: Promise<Runtime> | null = null;
let handlers: PoolHandlers | null = null;
let initialized = false;
let pendingFocusId: string | null = null;
let dragging = false;
const assignments = new Map<HTMLElement, string>();

function loadRuntime(): Promise<Runtime> {
  if (request === null) {
    request = import("./termPoolRuntime").then((loaded) => {
      runtime = loaded;
      if (handlers !== null) {
        loaded.initPool(handlers);
        initialized = true;
        loaded.setDragging(dragging);
      }
      return loaded;
    });
  }
  return request;
}

/** Start fetching xterm alongside the terminal component's own chunk. */
export async function preloadTerminalRuntime(): Promise<void> {
  await loadRuntime();
}

export function initPool(nextHandlers: PoolHandlers): void {
  handlers = nextHandlers;
  if (runtime !== null && !initialized) {
    runtime.initPool(nextHandlers);
    initialized = true;
  }
}

export function disposePool(): void {
  handlers = null;
  pendingFocusId = null;
  assignments.clear();
  if (runtime !== null && initialized) runtime.disposePool();
  initialized = false;
}

export function show(id: string, host: HTMLElement, fontSize?: number): void {
  assignments.set(host, id);
  void loadRuntime().then((loaded) => {
    if (!initialized || handlers === null || assignments.get(host) !== id) return;
    loaded.show(id, host, fontSize);
    if (pendingFocusId === id) {
      pendingFocusId = null;
      loaded.focusTerminal(id);
    }
  });
}

export function release(id: string, host: HTMLElement): void {
  if (assignments.get(host) === id) assignments.delete(host);
  if (request !== null) void request.then((loaded) => loaded.release(id, host));
}

export function focusTerminal(id: string): void {
  if (runtime !== null && initialized) runtime.focusTerminal(id);
  else pendingFocusId = id;
}

export function sendText(id: string, text: string): boolean {
  return runtime !== null && initialized ? runtime.sendText(id, text) : false;
}

export function setDragging(value: boolean): void {
  dragging = value;
  if (request !== null) void request.then((loaded) => loaded.setDragging(value));
}

export function syncSessions(liveIds: readonly string[]): void {
  if (request !== null) void request.then((loaded) => loaded.syncSessions(liveIds));
}

export function disposeSession(id: string): void {
  if (request !== null) void request.then((loaded) => loaded.disposeSession(id));
}

export function getSize(id: string): { cols: number; rows: number } | null {
  return runtime !== null && initialized ? runtime.getSize(id) : null;
}
