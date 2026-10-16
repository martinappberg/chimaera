/**
 * The window's side of cross-window tab moves (`crossWindow.ts` is the
 * protocol): the sender ledger, the per-workspace transport subscription,
 * the receiving adopt, out-of-window drops, detach into a new window, the
 * detached window's re-attach, and the "Move to ⟨window⟩" menu.
 *
 * Invariants this module carries:
 * - SENDER-REMOVES-ON-ACK. Sent tabs are armed in the ledger before the
 *   routing call goes out; only an ok-ack inside the late window removes
 *   them, and only from the workspace the move started in. A refused or lost
 *   ack leaves them in place.
 * - The receiver refuses (acks false) anything it cannot adopt faithfully: a
 *   malformed blob, the wrong workspace, or a layout still restoring.
 * - The live `layout` stays owned by App.svelte: the controller reads it
 *   through `getLayout` and hands new ones to App's own `setLayout`.
 * - The adopt payload (`{ ws, layout }`) and the detach blob are read by other
 *   windows and the daemon; their shape is a wire contract.
 */

import {
  ACK_LATE_MS,
  ACK_TIMEOUT_MS,
  broadcastTransport,
  mintTransfer,
  nativeTransport,
  TransferLedger,
  type AdoptDrop,
  type XwinTransport,
  type XwinWindowInfo,
} from "./crossWindow";
import { dropSpotAt, paneRootEl, sameSpot, type DropSpot } from "./dnd";
import {
  adoptTabs,
  allTabs,
  deserializeLayout,
  detachTab,
  findPane,
  paneForTab,
  panes,
  serializeLayout,
  soloLayout,
  tabKey,
  type AdoptSpot,
  type Layout,
  type Tab,
} from "./layout";
import { putViewState } from "./viewState";
import { buildViewBlob } from "./windowPersistence";
import { isNativeShell, openDetachedPopup, openDetachedWindow } from "../net/native";

/** Where an out-of-window release happened (dnd's `out` spot coordinates). */
export interface OutPoint {
  clientX: number;
  clientY: number;
  screenX: number;
  screenY: number;
}

// --- pure guards and shaping (characterized in crossWindowController.test.ts)

/** Whether drags in this window may tear out into a new window. A compute
 *  (job) window's daemon has its own URL vocabulary; the shell refuses the
 *  detach anyway — refusing HERE keeps the ghost honest and stops each
 *  attempt from pre-seeding an orphaned view-state blob. */
export function canDetachOut(ws: string | null, jobWindow: boolean): boolean {
  return ws !== null && !jobWindow;
}

/** Realm-local unsaved state must not leave this window: a dirty file's
 *  CodeMirror buffer, and a chat composer's volatile draft (memory-only
 *  image attachments / oversized text), both evaporate for the moving tab.
 *  Same rule as the rename guard. The reason to flash, or null to allow. */
export function dirtyMoveReason(
  tabs: readonly Tab[],
  dirty: ReadonlySet<string>,
  volatile: ReadonlySet<string>,
): string | null {
  if (tabs.some((t) => t.surface === "file" && dirty.has(t.path))) {
    return "save the file before moving it to another window";
  }
  if (tabs.some((t) => t.surface === "terminal" && volatile.has(t.sessionId))) {
    return "send or clear the chat draft before moving it to another window";
  }
  return null;
}

/** Whether the dirty-move guard would refuse this tab (silently — for arming
 *  decisions; the guard itself flashes the reason at act time). */
export function guardBlocksTab(
  tab: Tab,
  dirty: ReadonlySet<string>,
  volatile: ReadonlySet<string>,
): boolean {
  return (
    (tab.surface === "file" && dirty.has(tab.path)) ||
    (tab.surface === "terminal" && volatile.has(tab.sessionId))
  );
}

/** The adopt payload: enough for the receiver to validate the workspace
 *  and rebuild the tabs (`layout` is a serialized solo blob). */
export function adoptPayload(
  ws: string | null,
  tabs: Tab[],
  active: number,
  fontSize?: number,
): unknown {
  return { ws, layout: serializeLayout(soloLayout(tabs, active, fontSize)) };
}

/** An incoming payload this window can adopt: its tabs and the key of the
 *  tab to focus. Null = refuse (malformed, empty, wrong workspace, or this
 *  window still restoring its own layout, which would overwrite the
 *  adoption). */
export function decodeAdopt(
  payload: unknown,
  ws: string | null,
  ready: boolean,
): { tabs: Tab[]; activeKey: string | undefined } | null {
  const body = payload as { ws?: unknown; layout?: unknown } | null;
  const decoded =
    body !== null && typeof body === "object" ? deserializeLayout(body.layout) : null;
  const incoming = decoded !== null ? allTabs(decoded) : [];
  if (decoded === null || incoming.length === 0 || !ready || body === null || body.ws !== ws) {
    return null;
  }
  const pane0 = panes(decoded.root)[0];
  const activeTab = pane0?.tabs[Math.min(pane0.active, pane0.tabs.length - 1)];
  return { tabs: incoming, activeKey: activeTab !== undefined ? tabKey(activeTab) : undefined };
}

/** The layout with each of `tabs` removed wherever it now sits. */
export function removeSent(layout: Layout, tabs: readonly Tab[]): Layout {
  let next = layout;
  for (const t of tabs) {
    const loc = paneForTab(next.root, t);
    if (loc !== null) next = detachTab(next, loc.paneId, loc.index);
  }
  return next;
}

// --- the controller ---------------------------------------------------------

/** What App lends the controller: reads of its state and the setters App
 *  defines (so every assignment to `layout` and `dropSpot` stays in App). */
export interface CrossWindowHost {
  /** This window's key (read lazily: App declares it after the controller). */
  winKey(): string;
  getLayout(): Layout;
  setLayout(next: Layout): void;
  getWs(): string | null;
  getWsRoot(): string | null;
  /** The layout has finished restoring (App's `layoutReady`). */
  isReady(): boolean;
  isJobWindow(): boolean;
  /** This window's detached-from origin (re-attach target), or null. */
  getOrigin(): string | null;
  windowLabel(): string;
  isDetached(): boolean;
  getDropSpot(): DropSpot | null;
  setDropSpot(spot: DropSpot | null): void;
  dirtyFiles(): ReadonlySet<string>;
  volatileDrafts(): ReadonlySet<string>;
  flash(message: string): void;
  /** An adoption landed in this window's layout (App refocuses). */
  onAdopted(): void;
  /** Test seam: the transport for a workspace scope. Defaults to the shell
   *  transport in the native app, else a BroadcastChannel per workspace. */
  createTransport?(ws: string | null): XwinTransport | null;
  /** Test seam; defaults to `isNativeShell`. */
  isNative?(): boolean;
}

export interface CrossWindow {
  /** Subscribe this window's incoming adopt + ack traffic for the workspace
   *  `ws`; returns the teardown. App calls it from its transport `$effect`
   *  (keyed on the active workspace) so it re-subscribes on every switch. */
  subscribe(ws: string | null): (() => void) | undefined;
  canDetachOut(): boolean;
  /** The dirty-move guard: true = blocked, reason flashed. */
  guardDirtyMove(tabs: Tab[]): boolean;
  guardBlocksTab(tab: Tab): boolean;
  outDrop(tabs: Tab[], at: OutPoint, dragId: number, active?: number, fontSize?: number): Promise<void>;
  detachOut(tabs: Tab[], at: OutPoint, active?: number, fontSize?: number): Promise<void>;
  reattachToOrigin(): Promise<void>;
  /** The pane menu's "Open in new window". */
  detachTabToWindow(paneId: string, index: number): void;
  moveTargets(): XwinWindowInfo[];
  refreshMoveTargets(): void;
  moveTabToWindow(paneId: string, index: number, winId: string): void;
  /** A drag's out-of-window pointer (native only; resolves whether a sibling
   *  is under it). */
  track(clientX: number, clientY: number, dragId: number): Promise<boolean>;
  /** A drag ended: clear any sibling highlight and fence its track calls. */
  cancel(dragId: number): void;
}

export function createCrossWindow(host: CrossWindowHost): CrossWindow {
  const native = host.isNative ?? isNativeShell;
  const makeTransport =
    host.createTransport ??
    ((ws: string | null): XwinTransport | null =>
      isNativeShell()
        ? nativeTransport()
        : ws !== null
          ? broadcastTransport(ws, {
              winId: () => host.winKey(),
              label: () => host.windowLabel(),
              detached: () => host.isDetached(),
            })
          : null);

  /** The active transport: shell-routed in the native app, a per-workspace
   *  BroadcastChannel in the browser. Null on the home screen (browser). */
  let xwin: XwinTransport | null = null;
  /** Cached sibling-window roster for the "Move to ⟨window⟩" menu rows —
   *  menus build synchronously, so they read the last refresh. */
  let roster: XwinWindowInfo[] = [];
  /** Tabs sent away, plus the workspace they left — an ack must only ever
   *  remove tabs from the workspace the move started in (tabKey singletons
   *  like the dashboard exist in EVERY workspace). */
  const pendingMoves = new TransferLedger<{ tabs: Tab[]; ws: string | null }>();

  function refreshRoster(): void {
    void xwin?.roster().then((r) => (roster = r));
  }

  function payload(tabs: Tab[], active: number, fontSize?: number): unknown {
    return adoptPayload(host.getWs(), tabs, active, fontSize);
  }

  /** Sent tabs enter the ledger BEFORE the routing call goes out (the
   *  transfer id is sender-minted), so the target's ack can never beat the
   *  entry it resolves. Soft/hard deadlines per crossWindow.ts. */
  function armTransfer(transfer: number, tabs: Tab[]): void {
    pendingMoves.add(transfer, { tabs, ws: host.getWs() }, Date.now());
    setTimeout(() => {
      for (const _ of pendingMoves.timeouts(Date.now())) {
        host.flash("the move didn't complete — the tab stays here");
      }
    }, ACK_TIMEOUT_MS + 50);
    setTimeout(() => pendingMoves.expire(Date.now()), ACK_LATE_MS + 100);
  }

  /** A routed move that never left after all (refused, errored): forget the
   *  pre-armed entry without touching the layout. */
  function disarmTransfer(transfer: number): void {
    pendingMoves.ack(transfer, false, Date.now());
  }

  function resolveTransfer(transfer: number, ok: boolean): void {
    const known = pendingMoves.has(transfer);
    const entry = pendingMoves.ack(transfer, ok, Date.now());
    if (entry === null) {
      if (known && !ok) host.flash("the other window declined the move");
      return;
    }
    if (entry.ws !== host.getWs()) {
      // The move started in another workspace: its copies live in that
      // workspace's blob, not this layout. Removing "matching" tabs here
      // would delete this workspace's own dashboard/git/file tabs.
      return;
    }
    host.setLayout(removeSent(host.getLayout(), entry.tabs));
  }

  /** The receiving half: validate the payload (workspace + serialized solo
   *  blob), adopt at the drop spot, and ack. Anything that cannot be adopted
   *  FAITHFULLY acks false, and the sender keeps its tabs. */
  function acceptAdopt(drop: AdoptDrop): void {
    host.setDropSpot(null);
    const decoded = decodeAdopt(drop.payload, host.getWs(), host.isReady());
    if (decoded === null) {
      xwin?.ack(drop.transfer, false);
      return;
    }
    const raw = drop.at !== undefined ? dropSpotAt(drop.at.x, drop.at.y) : null;
    const spot: AdoptSpot | null =
      raw !== null && (raw.kind === "zone" || raw.kind === "tab" || raw.kind === "edge")
        ? raw
        : null;
    host.setLayout(adoptTabs(host.getLayout(), decoded.tabs, spot, decoded.activeKey));
    xwin?.ack(drop.transfer, true);
    host.onAdopted();
  }

  function guardDirtyMove(tabs: Tab[]): boolean {
    const reason = dirtyMoveReason(tabs, host.dirtyFiles(), host.volatileDrafts());
    if (reason === null) return false;
    host.flash(reason);
    return true;
  }

  /** Whether a browser-transport target is still alive. BroadcastChannel
   *  posts to a closed window are indistinguishable from delivered ones, so
   *  the menu paths probe the live roster first — the native shell errors
   *  from adopt_tab itself and needs no probe. */
  async function browserTargetAlive(winId: string): Promise<boolean> {
    if (native() || xwin === null) return true;
    const r = await xwin.roster();
    return r.some((w) => w.winId === winId);
  }

  /**
   * Detach tabs into their own window: pre-seed the new window's view-state
   * blob (`dt:1` + this window as origin), open a window on that id, then —
   * only once the open genuinely happened — remove the tabs here. The tabs
   * are views onto daemon-owned state, so the new window reattaches its own
   * sockets; nothing is ever moved but layout.
   */
  async function detachOut(tabs: Tab[], at: OutPoint, active = 0, fontSize?: number): Promise<void> {
    const wsId = host.getWs();
    if (wsId === null || tabs.length === 0 || guardDirtyMove(tabs)) return;
    const solo = soloLayout(tabs, active, fontSize);
    const winId = `w-${crypto.randomUUID()}`;
    const blob = buildViewBlob({
      ws: wsId,
      layout: solo,
      wsRoot: host.getWsRoot(),
      detached: true,
      origin: host.winKey(),
    });
    if (!(await putViewState(`${winId}_${wsId}`, blob))) {
      host.flash("couldn't reach the daemon — the tab stays here");
      return;
    }
    // Size the new window on the source pane when the payload lives in one
    // (a rail-row drag doesn't), clamped to a sane floor.
    const srcPane = paneForTab(host.getLayout().root, tabs[0])?.paneId;
    const rect = srcPane !== undefined ? paneRootEl(srcPane)?.getBoundingClientRect() : undefined;
    const w = Math.max(rect?.width ?? 900, 680);
    const h = Math.max(rect?.height ?? 600, 440);
    if (native()) {
      try {
        // Client coords: the shell lifts them into screen space itself.
        await openDetachedWindow(wsId, winId, { x: at.clientX, y: at.clientY, w, h });
      } catch (e) {
        host.flash(`couldn't open the window — ${String(e)}`);
        return;
      }
    } else if (!openDetachedPopup(winId, wsId, { x: at.screenX, y: at.screenY, w, h })) {
      host.flash("the browser blocked the popup — allow popups to detach tabs");
      return;
    }
    host.setLayout(removeSent(host.getLayout(), tabs));
  }

  return {
    subscribe(ws) {
      // One transport per (window, workspace scope); incoming adoption and
      // acks ride it. Handlers read live state at event time, not at
      // subscription time.
      const t = makeTransport(ws);
      xwin = t;
      if (t === null) return undefined;
      const unIncoming = t.incoming({
        onOver: (at) => {
          // The shell streams `over` per animation frame; only a LOGICAL spot
          // change may touch $state — an unconditional write would force a
          // layout pass and re-render in this (background) window every frame.
          const next = dropSpotAt(at.x, at.y);
          if (!sameSpot(next, host.getDropSpot())) host.setDropSpot(next);
        },
        onLeave: () => host.setDropSpot(null),
        onDrop: (drop) => acceptAdopt(drop),
      });
      const unAcks = t.acks((transfer, ok) => resolveTransfer(transfer, ok));
      refreshRoster();
      return () => {
        unIncoming();
        unAcks();
        t.close();
        // A leave that raced the teardown (workspace switch mid-hover) would
        // otherwise strand this window's drop overlay.
        host.setDropSpot(null);
        if (xwin === t) xwin = null;
      };
    },

    canDetachOut: () => canDetachOut(host.getWs(), host.isJobWindow()),

    guardDirtyMove,

    guardBlocksTab: (tab) => guardBlocksTab(tab, host.dirtyFiles(), host.volatileDrafts()),

    /**
     * An out-of-window RELEASE: over a sibling chimaera window it becomes a
     * cross-window move (shell-routed, remove-on-ack); over the desktop it
     * detaches into a new window. The browser has no sibling geometry, so
     * there it always detaches. A routing failure falls through to detach —
     * a completed gesture must never dissolve into silence.
     */
    async outDrop(tabs, at, dragId, active = 0, fontSize) {
      if (guardDirtyMove(tabs)) return;
      if (native() && xwin !== null) {
        const transfer = mintTransfer();
        // Armed BEFORE the routing call: the shell emits the drop to the
        // target inside drag_drop, so a fast ack could otherwise beat the
        // invoke's own response back to an empty ledger.
        armTransfer(transfer, tabs);
        try {
          const r = await xwin.drop(at.clientX, at.clientY, dragId, transfer, payload(tabs, active, fontSize));
          if (r.routed) return; // the ack (or its timeout) owns the outcome now
          disarmTransfer(transfer);
        } catch {
          disarmTransfer(transfer);
        }
      }
      await detachOut(tabs, at, active, fontSize);
    },

    detachOut,

    /**
     * The solo strip's one-click undo: send everything in this detached window
     * back to the window it came from. On the ok-ack the ledger removes the
     * tabs, the layout empties, and the empty-close effect closes this window
     * — the whole flow is the ordinary move machinery.
     */
    async reattachToOrigin() {
      const origin = host.getOrigin();
      if (xwin === null || origin === null) return;
      const layout = host.getLayout();
      const tabs = allTabs(layout);
      if (tabs.length === 0 || guardDirtyMove(tabs)) return;
      if (!(await browserTargetAlive(origin))) {
        host.flash("the original window isn't open anymore — use a tab's Move menu instead");
        return;
      }
      const current = host.getLayout();
      const p = findPane(current.root, current.focusedPaneId);
      const activeTab = p?.tabs[p.active];
      const activeIndex =
        activeTab !== undefined ? tabs.findIndex((t) => tabKey(t) === tabKey(activeTab)) : 0;
      const transfer = mintTransfer();
      armTransfer(transfer, tabs);
      try {
        await xwin.adoptInto(origin, payload(tabs, Math.max(activeIndex, 0), p?.fontSize), transfer);
      } catch {
        disarmTransfer(transfer);
        host.flash("the original window isn't open anymore — use a tab's Move menu instead");
      }
    },

    detachTabToWindow(paneId, index) {
      const p = findPane(host.getLayout().root, paneId);
      const tab = p?.tabs[index];
      if (p === null || tab === undefined) return;
      // No drop point on the menu path: open near the pane, offset a touch
      // so the new window reads as "came out of here". Client coords sit in
      // the VIEWPORT while window.screenX/Y is the OUTER frame — the browser
      // chrome (tab strip + toolbar) between them must be added, or the
      // popup opens a chrome-height too high. (Native ignores screen coords;
      // the shell lifts client coords itself.)
      const rect = paneRootEl(paneId)?.getBoundingClientRect();
      const cx = (rect?.left ?? 80) + 48;
      const cy = (rect?.top ?? 80) + 48;
      const chromeX = Math.max((window.outerWidth - window.innerWidth) / 2, 0);
      const chromeY = Math.max(window.outerHeight - window.innerHeight, 0);
      void detachOut(
        [tab],
        {
          clientX: cx,
          clientY: cy,
          screenX: window.screenX + chromeX + cx,
          screenY: window.screenY + chromeY + cy,
        },
        0,
        p.fontSize,
      );
    },

    moveTargets: () => roster,

    refreshMoveTargets: refreshRoster,

    moveTabToWindow(paneId, index, winId) {
      const p = findPane(host.getLayout().root, paneId);
      const tab = p?.tabs[index];
      if (p === null || tab === undefined || xwin === null) return;
      if (guardDirtyMove([tab])) return;
      void (async () => {
        if (!(await browserTargetAlive(winId))) {
          host.flash("that window isn't open anymore");
          refreshRoster();
          return;
        }
        const transfer = mintTransfer();
        armTransfer(transfer, [tab]);
        try {
          await xwin?.adoptInto(winId, payload([tab], 0, p.fontSize), transfer);
        } catch {
          disarmTransfer(transfer);
          host.flash("that window isn't open anymore");
        }
      })();
    },

    track: (x, y, dragId) => xwin?.track(x, y, dragId) ?? Promise.resolve(false),

    cancel(dragId) {
      xwin?.cancel(dragId);
    },
  };
}
