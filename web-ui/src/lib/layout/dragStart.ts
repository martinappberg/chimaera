/**
 * The window's drag starts: a tab or rail row/tree entry (`beginDrag`) and a
 * whole pane by its grip (`beginPaneDrag`), wired onto `dnd.ts`'s pointer
 * drag. Owns the per-window drag id: the shell fences a drag's late rAF track
 * calls once its drop/cancel has landed, so every drag that can tear out gets
 * a fresh id here and hands it to the cross-window controller.
 *
 * The live `layout`, `dropSpot` and `bandPanes` stay owned by App.svelte:
 * this module reads them through getters and hands new values to App's own
 * setters.
 */

import { isMac } from "../shared/keys";
import { basename } from "../previews/files";
import { transferBlock, transferInto, type FileSource } from "../workspace/fileTransfer";
import type { CrossWindow } from "./crossWindowController";
import { folderTargetAt, startDrag, type DropSpot } from "./dnd";
import {
  dropTab,
  dropTabAtRootEdge,
  findPane,
  movePane,
  movePaneToIndex,
  movePaneToRootEdge,
  moveTabToIndex,
  paneForTab,
  panes,
  type Layout,
  type Tab,
} from "./layout";

/** What App lends the drag starts. */
export interface DragStartHost {
  getLayout(): Layout;
  setLayout(next: Layout): void;
  setDropSpot(spot: DropSpot | null): void;
  /** Panes whose bottom band is armed for the CURRENT drag. */
  setBandPanes(panes: ReadonlySet<string>): void;
  xw: CrossWindow;
  /** The native shell routes out-of-window drags; the browser cannot. */
  isNative(): boolean;
  tabLabel(tab: Tab): string;
  sessionAlive(sessionId: string): boolean;
  /** The live session a pane shows (names the ref/link band), or null. */
  paneSessionId(paneId: string): string | null;
  linkTargetsFor(tab: Tab): ReadonlyMap<string, string> | undefined;
  linkSessionsFor(tab: Tab): ReadonlySet<string> | undefined;
  referenceFileDrop(paneId: string, path: string, kind: "file" | "dir"): void;
  referenceCommitDrop(paneId: string, tab: Tab): void;
  linkByDrop(terminalId: string, agentPaneId: string): void;
  linkBySession(terminalId: string, agentId: string): void;
  focusTerminal(sessionId: string): void;
  /** Focus the session the layout's focused pane shows, if any. */
  focusFocusedSession(): void;
  focusPane(paneId: string): void;
}

export interface DragStart {
  beginDrag(
    e: PointerEvent,
    tab: Tab,
    onClick: () => void,
    linkIntent?: boolean,
    source?: FileSource | null,
  ): void;
  beginPaneDrag(e: PointerEvent, paneId: string): void;
}

export function createDragStart(host: DragStartHost): DragStart {
  /** Monotonic per-window drag id: the shell fences a drag's late rAF track
   *  calls once its drop/cancel has landed (see done_drags). */
  let dragSeq = 0;

  /**
   * Words for the drag ghost's hint on the spots dnd.ts can't name itself:
   * WHICH session a reference band or link target is ("@ reference in
   * claude-1", "link to codex-2") — the same names the tabs and rail show.
   * Null for the geometric spots (dnd's generic reading covers them).
   */
  function describeSpot(spot: DropSpot): string | null {
    if (spot.kind === "fileOp") {
      const folder = spot.dir === "/" ? "/" : `${basename(spot.dir)}/`;
      const modifier = spot.operation === "move" ? ` · ${isMac ? "Option" : "Alt"} to copy` : "";
      return spot.blocked ?? `${spot.operation} into ${folder}${modifier}`;
    }
    if (spot.kind === "ref" || spot.kind === "link") {
      const sid = host.paneSessionId(spot.paneId);
      if (sid === null) return null;
      const name = host.tabLabel({ surface: "terminal", sessionId: sid });
      return spot.kind === "ref" ? `@ reference in ${name}` : `link to ${name}`;
    }
    if (spot.kind === "linkpane" || spot.kind === "linktab" || spot.kind === "linkrow") {
      return `link to ${host.tabLabel({ surface: "terminal", sessionId: spot.sessionId })}`;
    }
    return null;
  }

  /**
   * Shared drag start for rail rows and pane tabs (any surface). `linkIntent`
   * (set when the drag starts from a pane's link icon) restricts a shell
   * terminal to link-only drops — anywhere but an agent is a no-op, never a
   * tab move.
   */
  function beginDrag(
    e: PointerEvent,
    tab: Tab,
    onClick: () => void,
    linkIntent = false,
    source: FileSource | null = tab.surface === "file" ? { path: tab.path, kind: "file" } : null,
  ): void {
    const { xw } = host;
    const label = host.tabLabel(tab);
    const dragId = ++dragSeq;
    // Arm the bottom bands for this drag: reference targets for path drags
    // (file previews and Finder/dir payloads), link targets for shell-
    // terminal drags. Drives the partitioned zone previews (the band region
    // is reserved, never flashed over).
    const refPath =
      tab.surface === "file" || tab.surface === "finder"
        ? tab.path
        : tab.surface === "gitx" && tab.view === "commit"
          ? tab.sha
          : undefined;
    const armed = new Set<string>();
    const linkTargets = host.linkTargetsFor(tab);
    const linkSessions = host.linkSessionsFor(tab);
    if (linkTargets !== undefined) for (const id of linkTargets.keys()) armed.add(id);
    if (refPath !== undefined) {
      for (const p of panes(host.getLayout().root)) {
        const a = p.tabs[p.active];
        if (a !== undefined && a.surface === "terminal" && host.sessionAlive(a.sessionId)) {
          armed.add(p.id);
        }
      }
    }
    host.setBandPanes(armed);
    startDrag(
      e,
      { tab, label, refPath },
      {
        onSpot: (s) => host.setDropSpot(s),
        onDrop: (spot) => {
          if (spot.kind === "fileOp") {
            if (source !== null && spot.blocked === null) void transferInto(source, spot.dir, spot.operation);
            return;
          }
          if (spot.kind === "ref") {
            // Drag-to-reference: type into the session, never open a tab.
            if (tab.surface === "file") host.referenceFileDrop(spot.paneId, tab.path, "file");
            else if (tab.surface === "finder") host.referenceFileDrop(spot.paneId, tab.path, "dir");
            else if (tab.surface === "gitx") host.referenceCommitDrop(spot.paneId, tab);
            return;
          }
          if (spot.kind === "link") {
            // Plain tab drag onto an agent pane's band.
            if (tab.surface === "terminal") host.linkByDrop(tab.sessionId, spot.paneId);
            return;
          }
          if (spot.kind === "linkpane" || spot.kind === "linktab" || spot.kind === "linkrow") {
            // Link-intent drop on an agent (its view, tab, or rail row): link,
            // then surface the agent.
            if (tab.surface === "terminal") host.linkBySession(tab.sessionId, spot.sessionId);
            return;
          }
          // A link-intent drag (from the link icon) only ever links — a drop
          // anywhere but an agent is a no-op, never a surprise tab move.
          if (linkIntent) return;
          const layout = host.getLayout();
          if (spot.kind === "out") {
            // Released outside the window: move into the sibling window
            // under the pointer, or detach into a new one.
            const src = paneForTab(layout.root, tab);
            const fs = src !== undefined && src !== null ? findPane(layout.root, src.paneId)?.fontSize : undefined;
            void xw.outDrop([tab], spot, dragId, 0, fs);
            return;
          }
          // `upload` never reaches this pointer-drag callback (it is only ever
          // set on dropSpot by App's OS-drop handlers), so the zone arm is
          // explicit and the impossible case is a no-op.
          host.setLayout(
            spot.kind === "tab"
              ? moveTabToIndex(layout, tab, spot.paneId, spot.index)
              : spot.kind === "edge"
                ? dropTabAtRootEdge(layout, tab, spot.edge)
                : spot.kind === "zone"
                  ? dropTab(layout, tab, spot.paneId, spot.zone)
                  : layout,
          );
          if (tab.surface === "terminal") {
            host.focusTerminal(tab.sessionId);
          } else if (document.activeElement instanceof HTMLElement) {
            // A file surface landed: pull DOM focus off any terminal so
            // plain keys stop reaching a PTY that is no longer visible.
            document.activeElement.blur();
          }
        },
        onClick,
        onEnd: () => {
          host.setDropSpot(null);
          host.setBandPanes(new Set());
          // Clear any sibling-window highlight the drag lit up. After a
          // routed drop the shell already forgot the drag — a no-op then.
          // The id also fences this drag's still-in-flight track calls.
          xw.cancel(dragId);
        },
        // The "@ reference" band exists over panes showing a LIVE session
        // (dnd only consults this for file drags).
        acceptsRef: (paneId) => {
          const p = findPane(host.getLayout().root, paneId);
          if (p === null) return false;
          const a = p.tabs[p.active];
          return a !== undefined && a.surface === "terminal" && host.sessionAlive(a.sessionId);
        },
      },
      {
        linkTargets,
        linkSessions,
        linkIntent,
        fileTargetAt: source === null || linkIntent ? undefined : (x, y, copy) => {
          const folder = folderTargetAt(x, y);
          if (folder === null) return null;
          const operation = copy ? "copy" : "move";
          return { ...folder, kind: "fileOp", operation, blocked: transferBlock(source, folder.dir, operation) };
        },
        describe: describeSpot,
        // Realm-local unsaved state never arms the out spot: the ghost must
        // not advertise "open as new window" for a move the drop refuses.
        allowOut: !linkIntent && xw.canDetachOut() && !xw.guardBlocksTab(tab),
        trackOut: host.isNative() ? (x, y) => xw.track(x, y, dragId) : undefined,
        trackEnd: host.isNative() ? () => xw.cancel(dragId) : undefined,
      },
    );
  }

  /** The pane grip's whole-pane drag: move the ENTIRE pane (all tabs) to
   *  another split position, reusing the tab-drag drop zones. A plain click
   *  focuses the pane. No refPath/link arming — only tab/edge/zone spots fire,
   *  and any spot targeting the dragged pane itself is suppressed. */
  function beginPaneDrag(e: PointerEvent, paneId: string): void {
    const { xw } = host;
    const pane = findPane(host.getLayout().root, paneId);
    // The old "last pane never moves" gate predates tear-out: a single-pane
    // window's grip now has somewhere to go — OUT. In-window spots on the
    // dragged pane itself are self-drops and no-op below.
    if (pane === null || (panes(host.getLayout().root).length < 2 && !xw.canDetachOut())) return;
    const dragId = ++dragSeq;
    const active = pane.tabs[pane.active];
    const base = active !== undefined ? host.tabLabel(active) : "pane";
    const extra = pane.tabs.length > 1 ? ` +${pane.tabs.length - 1}` : "";
    startDrag(
      e,
      { label: `${base}${extra}` },
      {
        onSpot: (s) => {
          // Never advertise a drop onto the pane being dragged (self-move).
          host.setDropSpot(s !== null && "paneId" in s && s.paneId === paneId ? null : s);
        },
        onDrop: (spot) => {
          if ("paneId" in spot && spot.paneId === paneId) return;
          const layout = host.getLayout();
          if (spot.kind === "out") {
            // The whole pane (all its tabs, active index, font) tears out.
            const p = findPane(layout.root, paneId);
            if (p !== null) void xw.outDrop(p.tabs, spot, dragId, p.active, p.fontSize);
            return;
          }
          host.setLayout(
            spot.kind === "tab"
              ? movePaneToIndex(layout, paneId, spot.paneId, spot.index)
              : spot.kind === "edge"
                ? movePaneToRootEdge(layout, paneId, spot.edge)
                : spot.kind === "zone"
                  ? movePane(layout, paneId, spot.paneId, spot.zone)
                  : layout,
          );
          host.focusFocusedSession();
        },
        onClick: () => host.focusPane(paneId),
        onEnd: () => {
          host.setDropSpot(null);
          host.setBandPanes(new Set());
          xw.cancel(dragId);
        },
      },
      {
        // The pane's own zones are refused on drop (onSpot hides their
        // preview too), so the ghost must not promise "split right" there.
        describe: (s) => ("paneId" in s && s.paneId === paneId ? "" : describeSpot(s)),
        allowOut: xw.canDetachOut() && !pane.tabs.some((t) => xw.guardBlocksTab(t)),
        trackOut: host.isNative() ? (x, y) => xw.track(x, y, dragId) : undefined,
        trackEnd: host.isNative() ? () => xw.cancel(dragId) : undefined,
      },
    );
  }

  return { beginDrag, beginPaneDrag };
}
