# Workbench — panes, tabs, workspaces & navigation

The workbench shell: how a window is laid out and navigated. A **workspace** is a
registered project directory; everything else (files, git, terminals, agents) keys off
it. Inside a window the stage is a recursive tree of **panes**, each holding a stack of
**tabs** (terminal / file / diff / Finder / source-control / session-changes / settings /
chat). This page covers the layout engine and the surfaces that get you into a workspace.

**Where it all lives (shared):** the layout engine is `web-ui/src/lib/layout/`
(`layout.ts` pure tree ops, `SplitNode.svelte`, `Pane.svelte`, `PaneTabs.svelte`,
`dnd.ts`, `viewState.ts`, `railState.ts`); it's driven from `web-ui/src/App.svelte` (the
`ctrl` object + `onKeydown`). Default chords are in `web-ui/src/lib/shared/keys.ts`.
Daemon side: `crates/chimaera-server/src/{workspaces.rs,view_state.rs,quickopen.rs,fs.rs}`.

## Workspaces & the home screen

`HomeScreen.svelte` loads only while Home is visible, through the existing cached view loader and asset retry path. Home navigation stays eager; a failed load retains Retry, Settings and Open a folder. This reduces the workspace entry bundle without removing Home functionality or reducing total UI code.

- **What & when.** The landing surface when no workspace is open: your registered folders,
  most-recent-first, with a live-session rollup. Open one to start work.
- **How it's used.** Click a row to open it in this window; Cmd/Ctrl-click (or the hover
  "new window" button) opens it in a new window. `Mod+O` opens the folder picker.
  Per-row hover reveals `stop` (end its running sessions), `new window`, and `×` (remove
  from the list — the folder on disk is untouched).
- **Where it lives.** `web-ui/src/lib/workspace/HomeScreen.svelte` + `sessions.ts`
  (`listWorkspaces`/`deleteWorkspace`/`touchWorkspace`). Routes: `GET/POST /api/v1/workspaces`,
  `DELETE /api/v1/workspaces/{id}`, `POST /api/v1/workspaces/{id}/open` (stamps recency).
- **Key behaviors.** Registration is idempotent per canonical root; `canonicalize`+`is_dir`
  run under `spawn_blocking` (a dead NFS mount must not stall the reactor). A registered
  folder carries its workspace id (`.git/chimaera-workspace`, the private git directory of a
  linked worktree, or a `.chimaera-workspace` file when there is no git directory), so reopening it after a reinstall or a data reset, or
  on another computer, is the same workspace; a moved folder keeps its workspace and a local
  duplicate gets its own (see [pro.md](pro.md#what-the-daemon-does)). Opening a
  workspace writes the marker if it is missing; a folder that cannot be written registers
  without one. Recency sorts by
  `last_opened_at`. The rollup dot is muted (dormant) / accent (live) / amber (needs
  attention). `stop` stays always-visible for a running workspace and asks an inline confirm.

## Folder picker & create-folder

- **What & when.** Browse the daemon's filesystem to open (or create) a folder as a workspace.
- **How it's used.** `Mod+O`. Opens at `$HOME`; type to filter the current directory or type
  an absolute/`~` path for shell-style tab-completion. When the typed path doesn't exist the
  top row flips to "create folder" (create + open as a workspace in one step). In browse mode
  a tail **"new folder…"** row swaps to an inline input that creates in the *browsed*
  directory (`a/b` nests) and navigates into it — "open this folder" is the next Enter.
  Enter opens here; Cmd/Ctrl+Enter opens in a new window.
- **Where it lives.** `web-ui/src/lib/workspace/FolderPicker.svelte`; `fsHome`/`fsDirs`/`fsMkdir`
  in `sessions.ts`. Routes: `GET /api/v1/fs/home`, `GET /api/v1/fs/dirs`, `POST /api/v1/fs/mkdir`,
  `POST /api/v1/workspaces`.
- **Key behaviors.** Listings are dirs-only and capped server-side (`MAX_DIR_ENTRIES = 1000` —
  login-node scratch dirs are huge) with an honest `truncated` flag. `new window` targets *this
  window's own daemon* (remote-aware), so a remote workspace doesn't bounce to the launcher.

## Quick-open palette

- **What & when.** A fuzzy palette to jump to any file in the workspace or any live session.
- **How it's used.** `Mod+P` toggles it. Type to filter; Enter opens the highlighted row in the
  focused pane, Cmd/Ctrl+Enter in a fresh split, Esc closes. Matching sessions pin to the top.
- **Where it lives.** `web-ui/src/lib/workspace/QuickOpen.svelte`; `fsQuickOpen` in
  `web-ui/src/lib/previews/files.ts`. Route: `GET /api/v1/fs/quickopen?workspace_id=&q=&limit=&dirs=`
  (server `quickopen.rs`).
- **Key behaviors.** Cached results render instantly while the server call debounces 120ms
  (a `seq` guard drops out-of-order responses). The walk skips VCS/build/venv/pipeline dirs
  (`.git`, `node_modules`, `target`, `dist`, `__pycache__`, `.venv`, `.snakemake`, `work` —
  overridable via `quickOpen.ignoreDirs`), never follows symlinks, guards at 100k files / 32
  levels / 3 s wall. Cmd+P is files-only (`dirs=false`); the chat composer's `@`-mention opts
  into dirs. **The index serves stale and refreshes behind:** only a workspace's very first
  query waits on the walk (single-flighted — concurrent cold callers join it); afterwards every
  query — the palette, `@`-mentions, and the terminal/chat link validator's bare-basename
  fallback, which fires on every repaint — answers from the last index at once, and an index
  past its freshness window kicks one background re-walk. Freshness scales with the walk's own
  cost (10× its duration, floored at 5 s, capped at 120 s), so a 3 s NFS crawl is reused for
  30 s while a 40 ms local walk refreshes every 5 s. Walks run on the blocking pool, never on
  the reactor; an index unused for 10 min is dropped on the next query for any workspace
  (eviction is lazy — a fully idle daemon keeps its last index until someone asks again), and
  a deleted workspace's index goes at once. Measured live on a 20k-entry NFS workspace
  before this: every link-validation burst fanned out into five concurrent 3 s walks on the
  blocking pool (389 guard trips in one day), and the palette's own walk ran inline on a
  reactor worker.

## Find in the current pane

- **What & when.** Find text in the active terminal, conversation, code editor,
  rendered markdown, or PDF. **Quick Open (`Mod+P`) remains file/path and session
  lookup**; Find does not scan project files.
- **How it's used.** `Mod+F` (⌘F on macOS, Ctrl+Shift+F elsewhere), the pane-bar
  magnifier, or “Find in current pane” in Quick Open. Plain Ctrl+F also works
  outside PTY panes; a terminal keeps bare Ctrl+F/Ctrl+G for its running program.
  Enter / Shift+Enter navigate; ⌘G / ⇧⌘G and F3 / Shift+F3 also navigate an open
  search. Escape in the find bar closes it and returns focus to its view.
  `keys.find` is rebindable. Modals own their keyboard, and parked tabs never
  receive a find command. Keyboard focus takes precedence over the last clicked
  pane when moving between views with Tab.
- **Where it lives.** `web-ui/src/lib/shared/find.ts` registers each view's search
  handler; `App.svelte` routes shortcuts to the focused pane, `Pane.svelte` /
  `PaneTabs.svelte` expose the mouse path. `FindBar.svelte` supplies the terminal,
  conversation and rendered-document controls. CodeMirror and PDF retain their
  existing search engines and controls. No daemon route or wire change.
- **Scope.** Terminals search their retained xterm scrollback; conversations
  search retained message text, including messages outside the rendered window
  (not tool output). Markdown searches expanded document text, materializing
  windowed paragraphs while find is open. Each search shows its own scope and
  any result limit. Shared Find controls keep the query and match navigation on one row,
  with scope and result count below it, so narrow panes never wrap buttons into the tab row.
  Unsupported surfaces retain their ordinary key handling.

## Splitting, tabs & drag-and-drop

- **What & when.** Divide any pane row/column (recursively) to see surfaces side by side; each
  pane is a tab stack.
  In the sidebar, an agent working in a linked worktree shows its branch beside its name on the same
  line, with the full branch and path on hover. Agents in the main checkout have no branch chip.
- **How it's used.** Split via **Pane actions** (the tab bar’s ellipsis menu), `Mod+D` (right) / `Mod2+D` (down), or drag
  a tab to a pane edge. Drag the divider to reratio (double-click snaps 50/50; Escape restores).
  Open a surface → it appends a tab to the focused pane (VS Code "no duplicates": if already
  open anywhere, that tab is focused). Middle-click or `×` closes a tab (**detaches the view —
  never kills the session**). `Ctrl+Tab` / `Ctrl+Shift+Tab` cycle forward/backward in the
  focused pane in the native app; browsers use `Mod+Alt+]` / `Mod+Alt+[` because they own
  Ctrl+Tab for browser tabs. Both wrap at either end. Closing its active tab returns to the most recently
  used surviving tab, including the agent or workspace view underneath a document. Tab focuses the selected tab;
  Left/Right and Home/End activate and reveal tabs without moving focus into the document. Drag a tab to reorder within a bar,
  move to another pane, tear off into a split, or slam a **window edge** to split the whole window.
  A **pane grip** (six dots) sits with the actions at the right of the tab strip; drag
  it to move the **whole pane** (all its tabs) to another split — center merges, edges tear a
  split, a window edge re-roots — or drag it past the window edge to tear the whole pane out
  into its own window. It hides only while the pane is zoomed (a single-pane window's grip
  still has somewhere to go: out).
- **Many tabs: fit sizing + a scrolling strip.** Tabs never shrink to make room (VS Code
  "fit" sizing): each takes its natural width (glyph + name + close), capped at 180px with a
  64px floor, and a tab's width depends on itself alone — the active weight is pre-reserved,
  so activating, opening, or closing a tab moves none of its neighbours. When the strip
  overflows it **scrolls horizontally** (hidden scrollbar; a vertical mouse wheel over the
  strip scrolls it sideways, trackpad horizontal scroll is native), the clipped side(s) fade
  into the pane ground, and a **"N more" control** (chevron + count of tabs out of view) at
  the strip's end opens a menu of **every** tab in order, the active one marked — pick one to
  activate it. The active tab is scrolled into view whenever it changes (click, `Ctrl+Tab`,
  an open, a layout restore) and stays in view across a resize when it was in view before it — a
  strip the user scrolled away from is not snapped back by an unrelated layout change.
  The reveal accounts for the dropdown taking space; in narrow panes the edge fades yield
  to the selected tab so its right-hand close button stays reachable. Close buttons keep
  their hover/active visibility, with an unread dot in the same slot on inactive agent tabs. Adjacent
  tabs are separated by a hairline and the
  active tab carries a thin accent underline; a tab drag hovering near either edge of an
  overflowed strip auto-scrolls it. The tab row is 28px high and starts flush with the
  pane edge, with no reserved drag-handle gutter. Find and zoom stay directly
  reachable, while split, text size/reset, **Move to New Window** (the active tab),
  and close-view actions share **Pane actions**. Window moves use the same unsaved-edit
  and chat-draft guards as the tab context menu, and are absent on empty panes.
  In narrow panes, changed-file summaries are also available in that menu.
- **Preview (italic) tabs.** File opens are **preview** tabs, VS Code-style: the name renders
  italic, opening another file **replaces** the one preview slot per pane (so single-clicking
  through files doesn't pile up tabs), and it **pins** (non-italic, permanent) on a
  double-click of the tab or the tree row, on any edit (a dirty file auto-pins so an unsaved
  edit can't be replaced away), or on a tab move/reorder. Tree single-click = preview, tree
  double-click / a created file = pinned; chat/quick-open/terminal-link/Finder opens = preview.
  The preview flag persists in the layout blob (`pv:1`, additive).
- **Document navigation.** Files open above the existing view in the current pane.
  Following nested Markdown links reuses the preview slot; small Back/Forward buttons in a
  compact bottom-left floating control appear only while hovering the document (or focusing
  the controls by keyboard), and only when Back or Forward has a destination. This shared
  pane control works for every file type, clears bottom status bars by one toolbar height,
  and never changes tab-bar spacing. `Mod+[` / `Mod+]` navigate
  that journey (text editors retain their bracket shortcuts). Kept or edited documents stay
  open, and an already-open document is focused rather than duplicated. Each document view keeps at
  most 50 paths followed through its links; opening unrelated files and switching tabs do
  not add to that history. Closing and reopening the document, or reopening/reloading the
  window, starts fresh. Moving the tab carries its history. Renames carry the paths and
  deletions remove them. Link and embed resolution retains the gesture's source pane even
  if focus changes while it resolves. This trail and tab return order stay outside the saved layout.
- **Where it lives.** `web-ui/src/lib/layout/layout.ts` (`splitPane`, `openFile`/`pinTab`/
  `pinPaths`, `detachTab`, `tabKey`, `moveTabToIndex`/`dropTab`/`dropTabAtRootEdge`,
  `movePane`/`movePaneToRootEdge`/`movePaneToIndex`), `dnd.ts` (custom pointer DnD),
  `SplitNode.svelte`/`Pane.svelte`/`PaneTabs.svelte`, `tabScroll.ts` (reveal and fade geometry).
- **Key behaviors.** Ratio clamps to `[0.05, 0.95]` and a 120px minimum during drag; divider
  drags are rAF-throttled and **gate terminal refits** (`pool.setDragging`) to avoid reflow
  jank. The layout tree is pure/immutable with structural sharing. DnD is custom pointer-based
  (HTML5 DnD can't hit 60fps); the source captures the pointer so terminals never see the moves.
  Two special drop bands over a pane's lower ~22%: an **"@ reference in ⟨session⟩"** band (a file
  *or folder* drag types its path into a live session — see
  [drag-drop-and-uploads.md](drag-drop-and-uploads.md), which also covers OS-desktop file drops
  and screenshot paste) and a **"link to ⟨agent⟩"** band (a terminal drag leashes it — see
  [linked-terminals.md](linked-terminals.md)). **Every preview says what it does**: zone previews
  carry a centred label (`split left/right/up/down`, `add to this pane`), the window-edge preview
  says `split window ⟨side⟩`, and the drag ghost's hint names the hovered spot in the same words
  (`add to this pane` over a tab strip too — the caret says where — plus `@ reference in
  ⟨session⟩`, `link to ⟨agent⟩`) — quiet for tile moves, accent for reference/link/out. One
  vocabulary, one source: `zoneWord`/`sideWord` in `dnd.ts` feed both the ghost and the pane
  previews and match the pane-bar split actions; `DragOptions.describe` lets App supply session
  names and `dnd.ts` falls back to generic text, written only on a spot change. A tab-strip drop
  anchors only to the tabs in view — the scrolled-away run under the strip's controls is never
  a target — and lands after the last visible tab past them.

## Detach & cross-window moves

- **What & when.** Tear a tab (or a whole pane) out of the window into its own standalone
  window — a real OS window in the native shell, a popup in the browser — and move it back,
  or into any sibling window, later. For putting an agent, a terminal, or a preview on its
  own screen.
- **How it's used.** Drag a tab (or the pane grip) **past the window edge**: the drag ghost
  grows an "open as new window" hint; release to detach at the drop point. In the native
  shell, dragging **over another chimaera window** on the same host+workspace flips the hint
  to "move into window" — the target lights up its normal drop-spot previews and the drop
  lands there (Escape cancels and clears the highlight). Without a drag: every tab's context
  menu carries **Move to New Window** and **Move to ⟨window⟩** rows; **Pane actions** also
  offers **Move to New Window** for its active tab. A detached window's
  strip has a one-click **re-attach** that sends everything back to its origin window (on
  success the emptied solo window closes itself). Browser windows have the menu paths but no
  cross-window drag (nothing there can know sibling geometry).
- **Where it lives.** Detach/adopt layout ops `soloLayout`/`adoptTabs`/`allTabs` in
  `web-ui/src/lib/layout/layout.ts`; the `out` drop spot + `dropSpotAt` in `layout/dnd.ts`;
  the adopt protocol + `TransferLedger` + both transports in `layout/crossWindow.ts`;
  `detachOut`/`outDrop`/`acceptAdopt`/`reattachToOrigin` in `App.svelte`; menu rows in
  `layout/PaneTabs.svelte`. Native: IPC `open_detached_window`, `drag_track`/`drag_drop`/
  `drag_cancel`, `adopt_tab`/`adopt_ack`, `list_scope_windows` (`crates/chimaera-app/src/
  shell/commands.rs`), coordinate math + window hit-test in `shell/drag.rs`, `xdrag`/
  `xdrag-ack` window-targeted events. Browser transport: `BroadcastChannel("chimaera.xwin.
  {wsId}")`. Daemon: **no new surface** — a detached window is an ordinary window whose
  view-state blob was pre-seeded (`PUT /api/v1/view-state/{win}_{ws}` before opening).
- **Key behaviors.** A detached window is **just its pane** — `dt:1` in its blob, always in
  focus mode with **no rail and no way to reveal one** (the sidebar affordances are absent,
  the focus-mode chord is gated, and boot heals a blob persisted rail-open; re-attach is the
  way back to the full workbench). Its slim strip shows only its own things: workspace label
  (inert), re-attach, host, and an attention badge scoped to the sessions THIS window shows —
  never the workspace-wide roster. In the macOS titlebar overlay, the entire strip's
  non-interactive area drags the window, including padding and gaps; buttons stay clickable.
  It titles itself after its tab, never writes the `ws_`
  layout mirror (a partial layout must not poison the workspace window's restore fallback),
  self-closes when its last tab goes, is excluded from "open this workspace" raises
  (`WindowScope.detached`, set-only), and restores on app relaunch like any window. Moves
  are **remove-only-on-ack**: the sender drops its copy only when the receiver confirms
  (2s soft-timeout toast, late ok still converges, `tabKey` dedupe merges instead of
  duplicating — the surface is daemon-owned, so worst case is briefly two views, never a
  loss). Dirty file tabs refuse to move (the unsaved CodeMirror buffer is window-local);
  detaching from a compute (job) window is refused for now. Coordinate spaces are
  per-platform (logical on macOS, physical elsewhere; unit-tested in `shell/drag.rs`).

## Tab context menu & the "master name" rename

- **What & when.** Right-click a pane tab for surface-appropriate actions; renaming is the
  same *thing-level* rename everywhere — a name change applies to the underlying session or
  file, never to a per-tab alias.
- **How it's used.** Terminal/chat tabs: **Rename…** (inline input in the tab) pins the
  session's display name — the same pin as the rail's double-click/F2 rename and chat's
  `/rename`, so the tab, rail row, and quick-open all agree. File tabs: **Rename…** renames
  the file *on disk* (its unsaved buffer follows the rename), plus Reveal in File Tree,
  Download, Copy Path. Every other surface gets Close. Rail session rows also carry a
  right-click Rename…. Every tab type also offers **Move to pane N**, with its shortcut,
  alongside the existing cross-window moves. Right-click empty tab-bar space for pane actions.
  Elsewhere, surfaces offer their own actions, selected text offers Copy, and blank view
  space stays quiet instead of showing the web view's Reload menu. Native text-editor menus
  retain their edit/spelling actions; embedded web apps own the menus inside their frames.
- **Where it lives.** `PaneTabs.svelte` (`tabMenu`, the inline rename input);
  `shared/contextMenu.svelte.ts` + `ContextMenuHost.svelte` (the app-wide menu singleton);
  session rename via `PATCH /api/v1/sessions/{id}` (unchanged), file rename via
  `POST /api/v1/fs/rename` (see [files-and-previews.md](files-and-previews.md)).
- **Key behaviors.** The inline input is armored against the tab's capture-phase drag,
  middle-click close, and double-click zoom; Escape cancels, blur commits a non-empty valid
  name. A file rename flows through the fs-mutation bus, so the tab (and any diff/Finder tab
  under a renamed folder) rewrites in place.

## Zoom, focus mode & keyboard window management

- **What & when.** Focus one pane or hide the rail for a distraction-free / max-width view;
  move focus and tabs by keyboard.
- **How it's used.** Zoom a pane: bar button, double-click a tab, or `Mod2+Enter` (a "restore"
  badge appears). Focus mode (hide the left rail): `Mod+B` keeps sessions reachable through
  a slim strip. `Mod+Arrow` moves pane focus spatially; `Mod2+Arrow` carries the active tab into
  the neighbor — or, when there is no pane in that direction, **auto-splits a new one** on that
  side (capped at `MAX_PANES` and a minimum pane size); `Mod+1–9` focuses that numbered pane;
  `⌘±`/`⌘0` bump one pane's terminal/markdown font.
- **Empty panes & numbered moves.** `Mod+N` creates an empty pane to the right, up to
  `MAX_PANES` (4); the same action is available from Pane actions or Quick Open.
  The first split puts panes side by side (`1 | 2`). Subsequent new panes split the
  focused pane top/bottom (`1/3 | 2` when pane 1 was focused). Explicit `Mod+D`
  always splits right; `Mod2+D` always splits down.
  Its blank state offers file/session lookup and teaches dragging or holding the modifier
  to discover `Mod+1–9` (focus) and numbered tab moves (`⌃⌘1–9` on macOS;
  the second modifier layer elsewhere). The extra Control avoids macOS screenshot shortcuts.
  Pane numbers appear only while holding
  the modifier. Existing panes keep their shortcut numbers through splits, moves, and
  reloads; new panes take the lowest free number. Agents, terminals, documents, and workspace views use the same tab move operation;
  moving or closing a session view leaves the underlying session running. Empty source panes
  collapse after their last tab moves away. Sessions are reachable from the sidebar or Quick Open.
  Holding the configured modifier for 380 ms fades in a faint pane number and a 3% accent
  tint, with no blur or opaque backdrop. The base layer shows focus shortcuts and its move
  layer shows move shortcuts, plus new-pane / next-tab hints in the focused
  pane. Holding the move modifier keeps the destinations visible. Committing a key,
  releasing the modifier, hiding, or blurring the window clears the hints. No layout shifts.
  Browsers may reserve `Cmd+N`; the native app receives it, and the mouse/Quick Open paths
  are available in either host. `Cmd+Tab` remains macOS application switching.
- **Where it lives.** `layout.ts` (`toggleZoom`, `focusMode`, `moveFocus`, `moveTabDirection`,
  `moveTabToPane`, `navigateFileHistory`, `newPaneSplitDirection`, `setPaneFont` with `FONT_MIN 9`/`FONT_MAX 28`),
  `App.svelte` chords, `keys.ts`, `shared/chordHints.svelte.ts`.
- **Key behaviors.** Zoom always tracks the focused pane (focusing elsewhere clears it, so you
  can't get "stuck" zoomed). Focus mode is part of the persisted layout. Arrow chords defer to a
  text caret in editable surfaces but **not** in xterm's helper textarea (app chords must work
  over a focused terminal). Per-pane font override is persisted per pane.

## Layout & rail persistence

- **What & when.** The entire pane tree (splits, ratios, tabs, active tab, focus, zoom, focus
  mode, per-pane fonts) is saved on the daemon and restored on reload — separately for each
  workspace within each window. The rail width + FILES section are remembered too.
- **How it's used.** Automatic. Reload the page — or **reopen a window on the same workspace**
  (even a brand-new one, e.g. after a deliberate close) — and the exact layout returns.
- **Where it lives.** `web-ui/src/lib/layout/viewState.ts` (`windowKey` via `sessionStorage`,
  `serializeLayout`/`deserializeLayout`), `railState.ts` (localStorage, stamped + bounded to the
  16 most recently saved windows since the key is per tab); the per-window and
  workspace-only keys (`stateKey`/`wsKey`) + boot fallback are in `web-ui/src/App.svelte`. Route:
  `GET/PUT /api/v1/view-state/{key}` (server `view_state.rs`, opaque blobs, key
  `[A-Za-z0-9_-]{1,64}`, ≤64KB; the store keeps the 128 most recently written keys — every
  tab ever opened mints its own — and writes the file on the blocking pool, never the reactor).
- **Key behaviors.** The layout is keyed per (window id, workspace) — the window id lives in
  `sessionStorage` — **and mirrored under a workspace-only key** (`ws_<wsId>`). A reopened window
  mints a fresh id (the native shell discards a closed window's identity by macOS convention), so
  boot tries the window key, then the legacy key, then falls back to the workspace key — restoring
  that workspace's last-active layout instead of the empty default (last-active window wins the
  mirror). Writes debounce 500ms and flush on `pagehide` with `keepalive` so a close never loses
  state. Restore has a 3s timeout so a hung daemon never leaves a blank stage; it prunes dead
  sessions and 404 files, and a record-shaped tab of an unknown kind (a newer build's tab, then a
  rollback) is skipped rather than nulling the whole pane. Session ids survive a daemon restart
  (see [lifecycle-and-persistence.md](lifecycle-and-persistence.md)), which is what lets persisted
  tabs rebind with no client migration.

Agent rows keep one compact, vertically centered line, including while renaming or confirming
close. Linked-worktree branches share that line with separate truncation; provider activity is
available in the name's tooltip, so changing a status/title never shifts the rows below.
Their ordering uses the first known creation time throughout view switches and respawns;
attention-state changes do not reorder the sidebar or its numbered shortcuts.
The session list, expanded Recents and file tree share a slim, theme-aware scrollbar
with a transparent track; its drag area is wider than the visible thumb.

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Why the workbench is shaped this way
_Captured 2026-07-09 — drafted from docs/design/README.md + code, confirmed live with the maintainer._

- **Problem it solves.** Workspace-first, chat-many — the deliberate inversion of the Claude
  desktop app's chat-first / workspace-weak model. The folder *is* the window (file tree, previews,
  git, and N sessions all scoped to it), not an attribute of a chat.
- **Core vs addition.** The workspace-first model is a **core bet** — don't undo it. Split panes,
  focus mode, and the DnD/keyboard details are **additions**: deliberate (panes exist to put an
  agent and its outputs side by side, superseding the earlier "no tiling WM" non-goal) but
  improvable, not sacred.
- **Do not change:** the workspace-first inversion. Everything else in the workbench can change if
  it's a clear improvement.

### VS Code preview tabs — why they exist
_Captured 2026-07-11 (from the maintainer)._

- **Problem it solves.** Browsing files shouldn't litter a pane with permanent tabs. A file open is a
  transient **preview** tab (one preview slot per pane, replaced by the next preview open); editing,
  double-clicking a tab or tree row, or moving a tab **pins** it — muscle-memory parity with VS Code.
- **Grade — addition, improvable.** Deliberate today (the preview flag persists in the layout blob),
  but not a core bet — rework it freely if a better shape appears.

### Detach & cross-window moves — why they exist
_Intent: pending — to be captured via **capture-feature-intent** when the feat ships._

### Workspace-keyed window restore — why it's shaped this way
_Captured 2026-07-12 (from the maintainer)._

- **Problem it solves.** Native feel — a reopened window should come back to where you left it,
  not the empty default.
- **Why workspace-keyed.** *"Workspace keyed is good because we work primarily in workspaces"* —
  restoring by workspace (a reopened window resumes that workspace's last-active layout) fits how
  the app is actually used, and rides the workspace-first core bet above rather than tying restore
  to a throwaway window identity.
- **How settled it is.** *"More just a UX"* — settled as the right default for the workspace-first
  workflow, but an **addition**, not a frozen contract.
- **Do not change (or: open to change):** *"keep a smooth UX for the user"* — the keying mechanics
  are open to improve; only the workspace-first framing (above) is the core bet.

### Fit-width tab strips — why they exist
_Intent pending — drafted from the maintainer's request, 2026-09-06; questionnaire not yet run._

- **Problem it solves (from the request).** "When you have a lot of tabs open in a pane, they
  can sometimes switch sizes and it is really hard to see which tab is which, what they are
  named or how they jump around." Tabs used to shrink toward zero and re-flow on every
  open/close/activate; now they keep their natural width, the strip scrolls, and an overflow
  list names every tab.
- **Pending.** The fit sizing (vs a shrink-then-scroll hybrid), the 180px cap, the overflow
  count, and the quiet active underline have not been confirmed with the maintainer — capture
  via **capture-feature-intent** when available.

### Find in the current pane — why it exists
_Captured 2026-10-01 (from the maintainer)._

- **Problem it solves.** A user asked, “Finns de sök funktion cmd f på chimera?” The
  maintainer asked us to “really think it through and make it good,” and confirmed
  that finding text and finding files are both important.
- **Promise vs addition.** “Keep the shortcut distinction; everything else can improve.”
  Keep Find in the current pane distinct from the existing file/session lookup
  shortcut. The remaining behavior and presentation are improvable additions.
- **Open for improvement.** The maintainer requested a separate UI/UX pass on the
  crowded pane headers, Markdown toolbar, and Find controls at narrow split widths.


### Pane shortcuts and document journeys — why they exist
_Captured 2026-10-04 from the maintainer's requests and live-preview feedback in this session._

- **Problem it solves.** The maintainer likes the i3-like hotkeys, but found it
  confusing that “agents and terminals are treated as differently than views and
  panes.” They requested standard tab behavior: keep the previous view underneath
  an opened document, return to it on close, and cycle tabs within the pane.
- **Deliberate navigation choices.** A new pane first creates `1 | 2`; later panes
  split below the focused pane (`1/3 | 2` when pane 1 is focused). Pane numbers
  should appear “only when you hold the cmd key” with a “very subtle” backdrop.
  Shortcut hints “should not distract the UX at all.”
- **Document history scope.** History is “only in the specific view you are in”
  while following links, and should not survive reopening the window. Back/Forward
  should float just above the status line, be smaller, appear on document hover,
  and disappear entirely when there is nowhere to navigate.
- **Grade — addition.** These refine the existing split-pane and keyboard additions
  above. The maintainer approved the resulting native preview: “Great !Looks good.”
  No additional frozen contract or future exclusions were stated in this session.
