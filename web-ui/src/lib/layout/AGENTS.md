# web-ui/src/lib/layout — splits, panes, tabs, window state

Orientation for coding agents. This area owns the in-window layout tree (row/col splits, panes, tab stacks), drag and drop, cross-window tab moves, lazy pane views, per-window view state, and the window reload and close gates. It does not own what a tab shows: each surface lives in its own area (`terminal/`, `previews/`, `browser/`, `chat/`, `workspace/`). It does not hold the live layout value: `App.svelte` does. Parent map: repo-root [AGENTS.md](../../../../AGENTS.md). Rules: [.claude/rules/web-ui.md](../../../../.claude/rules/web-ui.md). Feature page: [workbench](../../../../docs/features/workbench.md).

## File map

| File | What it owns |
|---|---|
| `layout.ts` | The layout model: `Layout`, `PaneNode`, `Tab` kinds, and every op (`openTab`, `splitPane`, `activateTab`, `moveTabDirection`, …). Pure and DOM-free; dev-only self-checks at the end of the module. `MAX_PANES = 4`. Also `trimLive`, the trim of a pane's kept-alive view set (Pane's `LIVE_CAP = 8`), which never evicts a file view with unsaved edits. |
| `SplitNode.svelte` | Renders one split or pane node recursively, with the draggable divider. |
| `Pane.svelte` | One pane: its tab layers, kept-alive views, the per-view `<svelte:boundary>`, and lazy view loading. |
| `PaneTabs.svelte` | The pane's 28px top bar: tabs, pane controls and menu, the zoom badge, and the drag handle. |
| `dnd.ts` | Pointer-event drag and drop (no HTML5 DnD): sources, drop targets (`DropSpot`), the movement threshold, and Escape to cancel. |
| `crossWindow.ts` | Moving tabs between windows: one adopt protocol over two transports (the native shell, or a `BroadcastChannel` in the browser). `crossWindowController.ts` is the window's side: `createCrossWindow` owns the sender ledger, the per-workspace transport subscription (App's `$effect` calls `subscribe(activeWsId)`; teardown clears `dropSpot`), the receiving adopt (refused while the layout restores), out-drops, detach-out (`buildViewBlob`), re-attach and the Move menu, plus the pure guards (`dirtyMoveReason`, `guardBlocksTab`, `decodeAdopt`, `adoptPayload`). `dragStart.ts` (`createDragStart`) is the tab/row/pane-grip drag start and owns the per-window drag id. Neither holds the layout. |
| `lazyViews.ts` | `PaneViewKind`, the dynamic-import loader per view kind, and recovery of a failed chunk or stylesheet without a reload. |
| `viewState.ts` | `windowKey()` and the daemon-owned `/view-state/{key}` GET/PUT (≤64KB, debounced 500ms). |
| `surfaces.ts` | The window's surface manifest sent with the view-state PUT: identities only, capped at `SURFACES_CAP = 40`. |
| `railState.ts` | Sidebar width and FILES section size per window, in localStorage, clamped on read; keeps the newest 16 windows. |
| `windowPersistence.ts` | The window's view-state persistence: `stateKey`/`wsKey`, the pure `buildViewBlob` (the exact PUT body: v, ws, layout, surfaces, then dt/origin), `pickRestoreBlob` + `restoreFromBlob` (boot's own-key → bare-window-key → `ws_` mirror precedence; a detached window takes only its own), `pruneDeadFiles`, and `createViewPersistence`, the controller behind App's persist effect, boot and workspace switch. It never holds the layout: App lends getters and the setters it defines. |
| `railResize.ts` | `createRailResize`: the rail's FILES divider and width-handle drags (pointer capture, rAF throttle, Escape/cancel restores, `railState.ts` clamps) and the double-click width reset, over App's getters/setters. |
| `tabScroll.ts` | Pure tab-strip scroll arithmetic (reveal a clipped tab, edge fades). |
| `assetTransition.ts` | The `assetTransition` store: when the window must load a new interface (build change, moved connection, failed chunk, manual reload) and when navigation is safe. |
| `assetGate.svelte.ts` | `installAssetGate()`: the effects that carry out an `assetTransition` — the `vite:preloadError` report, the navigation (deferred while `dirtyFiles` or `volatileChatDrafts` is non-empty unless forced), the hidden-window retry catch-up, the timer cleanup, and the Reload Window claim. App calls it once, before its `beforeunload` guard effect. |
| `AssetTransitionNotice.svelte` | The notice for an asset transition that waits or retries. |
| `windowReload.ts` | `window.__chimaeraReloadWindow` for the native Reload Window; routes through App's gate once App runs. |
| `windowClose.svelte.ts` | Native window close and quit with unsaved files: replies `shown`, then `proceed` or `cancel` to the shell. |
| `CloseDirtyDialog.svelte` | "Save changes before closing?" for every close gesture over dirty files. |

Tests: `*.test.ts` beside the modules.

## State owners

| State | Owner | Who mutates |
|---|---|---|
| The live `layout` (`$state<Layout>`) | `App.svelte` | App only, by `layout = op(layout)` with the pure ops from `layout.ts`; `windowPersistence.ts` hands new layouts to a setter App defines. |
| `assetTransition` (writable) | `assetTransition.ts` | Its exported functions (`requireAssetNavigation`, `noteChunkFailure`, `requestAssetReload`, `requestWindowReload`, …). |
| View-state blobs | The daemon (`/view-state/{key}`) | `saveViewState` via `windowPersistence.ts`'s controller (App's persist effect, boot, workspace switch); `putViewState` from `crossWindowController.ts`'s detach-out. |
| Rail dimensions | `railState.ts` (localStorage) | Its save functions; the live values are App `$state`, dragged through `railResize.ts`. |

## Invariants / gotchas

- **Layout ops are pure.** An op takes a `Layout` and returns a new one with structural sharing. Keep DOM and stores out of `layout.ts`; `browser/agentOpen.ts` depends on that to stay unit-testable.
- **The pane cap is 4 for auto-splits and chords.** A manual drag-tear is not capped (`MAX_PANES` comment).
- **Every pane view renders inside `<svelte:boundary>`** (`Pane.svelte`), so one view that throws fails alone. See the rule bullet "A view that throws fails alone".
- **Keep-alive hides with `opacity:0` + `inert`.** See the rule bullet "Hide with opacity + inert". `keepsPaneViewAlive` decides which tabs keep their component tree: every non-terminal tab, and a terminal tab shown as chat. A PTY tab parks its xterm DOM in the terminal pool instead.
- **A cross-window move is sender-removes-on-ack.** The sender drops its tabs only after an ok-ack. A lost or refused ack leaves the tabs in place (`crossWindow.ts` header).
- **The browser has no cross-window drag.** Only the native shell knows window geometry (`crossWindow.ts` header).
- **The surface manifest never carries file contents.** It sends paths, session ids, and singleton names; at most one entry is `focused` (`surfaces.ts` header). The `surfaces` key is additive on the wire.
- **Never lose an edit on close.** The native shell never closes on a failed save; only an explicit Don't save discards (`windowClose.svelte.ts` header).

## Checks

```sh
npm --prefix web-ui run check
npm --prefix web-ui run test -- src/lib/layout
```

Drag, split, and tab-switch changes need a live check in the isolated preview (**verify-app**). Tab-switch cost is measured in `scripts/perf/tab-switch/`.

## Neighbours

[net](../net/AGENTS.md) · [terminal](../terminal/AGENTS.md) · [previews](../previews/AGENTS.md) · [browser](../browser/AGENTS.md) · [workspace](../workspace/AGENTS.md) · [shared](../shared/AGENTS.md) · [chat](../chat/AGENTS.md) · [dashboard](../dashboard/AGENTS.md). Feature pages: [native-app](../../../../docs/features/native-app.md), [lifecycle-and-persistence](../../../../docs/features/lifecycle-and-persistence.md).
