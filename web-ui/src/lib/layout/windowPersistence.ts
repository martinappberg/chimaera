/**
 * Window layout persistence: what this window writes to the daemon-owned
 * `/view-state/{key}` store, what it restores at boot, and the order a
 * workspace switch saves, flushes and resets in. The blob shape is a wire
 * contract — the daemon reads its additive `surfaces` key and other windows
 * restore from the `ws_` mirror — so `buildViewBlob` must keep its key order
 * and fields exactly (`viewState.ts` carries the ≤64KB / 500ms contract).
 *
 * The live `layout` stays owned by App.svelte: the controller never holds a
 * layout, it reads one through `getLayout` and hands new ones to App's own
 * `setLayout`.
 */

import { isDetachedWindow, setDetachedWindow } from "../net/api";
import {
  allFilePaths,
  defaultLayout,
  deserializeLayout,
  pruneFiles,
  serializeLayout,
  type Layout,
} from "./layout";
import { surfacesOf } from "./surfaces";
import { flushViewState, loadViewState, saveViewState } from "./viewState";

/**
 * View-state key: (window id, workspace id) composed client-side, so
 * switching workspaces away and back restores that workspace's layout.
 * Server key charset is [A-Za-z0-9_-]{1,64}; uuid + "_" + "w-xxxxxxxx" fits.
 */
export function stateKey(winKey: string, wsId: string | null): string {
  return wsId === null ? winKey : `${winKey}_${wsId}`;
}

/**
 * Workspace-only view-state key (no window id). The layout is mirrored here so
 * a window that reopens a workspace with a FRESH id — after a deliberate close
 * (the native shell discards the window's identity by macOS convention), or a
 * plain browser reopen — still restores that workspace's last-active layout
 * instead of falling back to the empty default. Consulted only when the
 * window-keyed blob is absent; last-active window on a workspace wins.
 */
export function wsKey(wsId: string): string {
  return `ws_${wsId}`;
}

export interface ViewBlobInput {
  ws: string | null;
  layout: Layout;
  /** Workspace root, for workspace-relative surface paths. */
  wsRoot: string | null;
  detached: boolean;
  /** The window this one detached from; written only when `detached`. */
  origin: string | null;
}

/**
 * The per-window view-state blob. Key order is part of the contract (the PUT
 * body is `JSON.stringify` of this): v, ws, layout, surfaces, then dt/origin
 * for a detached window. `surfaces` is the additive, normalized "what this
 * window shows" list (design §8): the daemon reads only that key and treats
 * `layout` as opaque.
 */
export function buildViewBlob(input: ViewBlobInput): Record<string, unknown> {
  const blob: Record<string, unknown> = {
    v: 1,
    ws: input.ws,
    layout: serializeLayout(input.layout),
    surfaces: surfacesOf(input.layout, input.wsRoot),
  };
  if (input.detached) {
    blob.dt = 1;
    if (input.origin !== null) blob.origin = input.origin;
  }
  return blob;
}

/**
 * The outgoing blob of a detached window converting to a full workbench: no
 * `dt` (switching back later must not resurrect solo mode from stale state)
 * and no surfaces (the window is leaving the workspace and shows none of it).
 */
export function leavingViewBlob(ws: string | null, layout: Layout): Record<string, unknown> {
  return { v: 1, ws, layout: serializeLayout(layout), surfaces: [] };
}

/** A stored blob this window may restore for workspace `ws`. */
export function blobMatches(raw: unknown, ws: string | null): boolean {
  return (
    typeof raw === "object" &&
    raw !== null &&
    (raw as { v?: unknown }).v === 1 &&
    (raw as { ws?: unknown }).ws === ws
  );
}

/** A slow/hung daemon must never leave the stage blank forever: after this
 *  the default layout renders and a late restore is simply dropped. */
export const RESTORE_TIMEOUT_MS = 3000;

export interface PickRestoreInput {
  winKey: string;
  ws: string | null;
  /** Read after the first load, as boot always has. */
  isDetached: () => boolean;
  load: (key: string) => Promise<unknown>;
  timeoutMs?: number;
}

/**
 * Which stored blob boot restores from, in precedence order: this window's
 * (window, workspace) key; then the bare window key (blobs written before
 * per-workspace keys); then the workspace mirror. One shared timeout covers
 * all three loads. A detached window never takes the fallbacks: the legacy and
 * mirror blobs describe the FULL workspace, and restoring one would silently
 * turn a torn-off pane into a clone of the main window. Its own blob or
 * nothing. The result may still not match; `restoreFromBlob` decides.
 */
export async function pickRestoreBlob(input: PickRestoreInput): Promise<unknown> {
  const { winKey, ws, load } = input;
  const ms = input.timeoutMs ?? RESTORE_TIMEOUT_MS;
  const timeout = new Promise<null>((resolve) => setTimeout(() => resolve(null), ms));
  let raw = await Promise.race([load(stateKey(winKey, ws)), timeout]);
  if (!input.isDetached()) {
    if (!blobMatches(raw, ws) && ws !== null) {
      // pre-composite-key blob migration
      raw = await Promise.race([load(winKey), timeout]);
    }
    if (!blobMatches(raw, ws) && ws !== null) {
      // No layout for THIS window yet (a reopened/fresh window): fall back
      // to this workspace's last-active layout so a reopen restores, not
      // resets.
      raw = await Promise.race([load(wsKey(ws)), timeout]);
    }
  }
  return raw;
}

export interface RestoredView {
  /** The layout to show: the restored tree, or `current` when the stored one
   *  is unreadable, healed to focus mode for a detached window. */
  layout: Layout;
  detached: boolean;
  origin: string | null;
  /** The blob alone said detached: write the hint so a plain reload agrees. */
  refreshHint: boolean;
}

/**
 * What a picked blob restores, or null when it doesn't match `ws` (the
 * caller keeps its layout and detachedness). Detachedness: the hash hint is
 * authoritative; the blob's dt:1 covers pre-hint blobs. A solo window IS
 * focus mode — no rail, ever — so a blob persisted before that rule is healed.
 */
export function restoreFromBlob(
  raw: unknown,
  ws: string | null,
  current: Layout,
  hintDetached: boolean,
): RestoredView | null {
  if (!blobMatches(raw, ws)) return null;
  const restored = deserializeLayout((raw as { layout?: unknown }).layout);
  let layout = restored ?? current;
  const r = raw as { dt?: unknown; origin?: unknown };
  const detached = hintDetached || r.dt === 1;
  const origin =
    detached && typeof r.origin === "string" && /^[A-Za-z0-9_-]{1,64}$/.test(r.origin)
      ? r.origin
      : null;
  if (detached && !layout.focusMode) layout = { ...layout, focusMode: true };
  return { layout, detached, origin, refreshHint: detached && !hintDetached };
}

/**
 * Drop restored file tabs whose files are definitively gone (400/404); an
 * unreachable or not-yet-upgraded daemon never wipes tabs. The prune applies
 * to the layout as it is when the probes settle, not as it was at the start.
 */
export function pruneDeadFiles(
  getLayout: () => Layout,
  setLayout: (next: Layout) => void,
  probe: (path: string) => Promise<"ok" | "dead" | "unknown">,
): Promise<void> {
  const paths = allFilePaths(getLayout());
  if (paths.length === 0) return Promise.resolve();
  return Promise.all(paths.map(async (p) => [p, await probe(p)] as const)).then((results) => {
    const dead = new Set(results.filter(([, r]) => r === "dead").map(([p]) => p));
    if (dead.size > 0) setLayout(pruneFiles(getLayout(), dead));
  });
}

/** What App lends the controller: reads of its state and the setters App
 *  defines (so every assignment to `layout` stays in App.svelte). */
export interface ViewPersistenceHost {
  winKey: string;
  getWs(): string | null;
  getWsRoot(): string | null;
  getLayout(): Layout;
  setLayout(next: Layout): void;
  isReady(): boolean;
  setReady(ready: boolean): void;
  isDetached(): boolean;
  getOrigin(): string | null;
  setDetached(detached: boolean, origin: string | null): void;
  /** Runs right after `layoutReady` flips true at the end of a boot. */
  onBooted(): void;
}

export interface ViewPersistence {
  /** The persist effect's body: build the blob (tracking every input), then,
   *  once ready, save it under the window key and the workspace mirror. */
  persist(): void;
  /** Restore this (window, workspace)'s layout, then flip ready. */
  boot(): Promise<void>;
  /** A workspace switch's outgoing half: strip a detached window's dt (it is
   *  converting to a full workbench) and flush the pending write under the
   *  outgoing key. Call BEFORE the active workspace changes. */
  leaveWorkspace(): void;
  /** A workspace switch's reset: not ready, THEN the default layout, so the
   *  default is never persisted under the incoming key. Call after the active
   *  workspace changes and before `boot()`. */
  resetForBoot(): void;
  flush(): Promise<void>;
}

export function createViewPersistence(host: ViewPersistenceHost): ViewPersistence {
  /** Guards overlapping boots (initial load racing a workspace switch). */
  let bootSeq = 0;

  return {
    persist(): void {
      const ws = host.getWs();
      const detached = host.isDetached();
      const blob = buildViewBlob({
        ws,
        layout: host.getLayout(),
        wsRoot: host.getWsRoot(),
        detached,
        // Read only when detached, so the effect tracks exactly what it did.
        origin: detached ? host.getOrigin() : null,
      });
      if (!host.isReady()) return;
      saveViewState(stateKey(host.winKey, ws), blob);
      // Mirror under the workspace-only key so a reopened window (fresh id)
      // restores this workspace instead of the empty default (see wsKey).
      // A detached window's deliberately partial layout never writes the
      // mirror — it would poison the real workspace window's restore fallback.
      if (ws !== null && !host.isDetached()) saveViewState(wsKey(ws), blob);
    },

    async boot(): Promise<void> {
      const seq = ++bootSeq;
      const wsAtBoot = host.getWs();
      const raw = await pickRestoreBlob({
        winKey: host.winKey,
        ws: wsAtBoot,
        isDetached: isDetachedWindow,
        load: loadViewState,
      });
      if (seq !== bootSeq) return; // a later switch superseded this boot
      const restored = restoreFromBlob(raw, wsAtBoot, host.getLayout(), isDetachedWindow());
      if (restored !== null) {
        if (restored.layout !== host.getLayout()) host.setLayout(restored.layout);
        host.setDetached(restored.detached, restored.origin);
        if (restored.refreshHint) setDetachedWindow(true);
      }
      // The layout is in place before ready flips: the persist effect is
      // gated on ready, and the default must never be written over a restore.
      host.setReady(true);
      host.onBooted();
    },

    leaveWorkspace(): void {
      if (host.isDetached()) {
        host.setDetached(false, null);
        setDetachedWindow(false);
        saveViewState(stateKey(host.winKey, host.getWs()), leavingViewBlob(host.getWs(), host.getLayout()));
      }
      void flushViewState();
    },

    resetForBoot(): void {
      host.setReady(false);
      host.setLayout(defaultLayout());
    },

    flush: flushViewState,
  };
}
