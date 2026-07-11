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
  run under `spawn_blocking` (a dead NFS mount must not stall the reactor). Recency sorts by
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
- **Key behaviors.** Listings are dirs-only and capped server-side (`MAX_DIR_ENTRIES = 4000` —
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
  overridable via `quickOpen.ignoreDirs`), never follows symlinks, guards at 100k files. Cmd+P
  is files-only (`dirs=false`); the chat composer's `@`-mention opts into dirs.

## Splitting, tabs & drag-and-drop

- **What & when.** Divide any pane row/column (recursively) to see surfaces side by side; each
  pane is a tab stack.
- **How it's used.** Split via the pane-bar buttons, `Mod+D` (right) / `Mod2+D` (down), or drag
  a tab to a pane edge. Drag the divider to reratio (double-click snaps 50/50; Escape restores).
  Open a surface → it appends a tab to the focused pane (VS Code "no duplicates": if already
  open anywhere, that tab is focused). Middle-click or `×` closes a tab (**detaches the view —
  never kills the session**). `Mod+Alt+[`/`]` cycle tabs. Drag a tab to reorder within a bar,
  move to another pane, tear off into a split, or slam a **window edge** to split the whole window.
- **Where it lives.** `web-ui/src/lib/layout/layout.ts` (`splitPane`, `openTab`, `detachTab`,
  `tabKey`, `moveTabToIndex`/`dropTab`/`dropTabAtRootEdge`), `dnd.ts` (custom pointer DnD),
  `SplitNode.svelte`/`Pane.svelte`/`PaneTabs.svelte`.
- **Key behaviors.** Ratio clamps to `[0.05, 0.95]` and a 120px minimum during drag; divider
  drags are rAF-throttled and **gate terminal refits** (`pool.setDragging`) to avoid reflow
  jank. The layout tree is pure/immutable with structural sharing. DnD is custom pointer-based
  (HTML5 DnD can't hit 60fps); the source captures the pointer so terminals never see the moves.
  Two special drop bands over a pane's lower ~22%: an **"@ reference"** band (a file *or folder*
  drag types its path into a live session — see [drag-drop-and-uploads.md](drag-drop-and-uploads.md),
  which also covers OS-desktop file drops and screenshot paste) and a **"link to agent"** band (a
  terminal drag leashes it — see [linked-terminals.md](linked-terminals.md)).

## Tab context menu & the "master name" rename

- **What & when.** Right-click a pane tab for surface-appropriate actions; renaming is the
  same *thing-level* rename everywhere — a name change applies to the underlying session or
  file, never to a per-tab alias.
- **How it's used.** Terminal/chat tabs: **Rename…** (inline input in the tab) pins the
  session's display name — the same pin as the rail's double-click/F2 rename and chat's
  `/rename`, so the tab, rail row, and quick-open all agree. File tabs: **Rename…** renames
  the file *on disk* (disabled while the file has unsaved edits), plus Reveal in File Tree,
  Download, Copy Path. Every other surface gets Close. Rail session rows also carry a
  right-click Rename….
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
  badge appears). Focus mode (hide the left rail): `Mod+B` (a slim session strip keeps `Mod+1–9`
  reachable). `Mod+Arrow` moves pane focus spatially; `Mod2+Arrow` carries the active tab into
  the neighbor; `Mod+1–9` opens the Nth rail session; `⌘±`/`⌘0` bump one pane's terminal/markdown
  font.
- **Where it lives.** `layout.ts` (`toggleZoom`, `focusMode`, `moveFocus`, `moveTabDirection`,
  `setPaneFont` with `FONT_MIN 9`/`FONT_MAX 28`), `App.svelte` chords, `keys.ts`.
- **Key behaviors.** Zoom always tracks the focused pane (focusing elsewhere clears it, so you
  can't get "stuck" zoomed). Focus mode is part of the persisted layout. Arrow chords defer to a
  text caret in editable surfaces but **not** in xterm's helper textarea (app chords must work
  over a focused terminal). Per-pane font override is persisted per pane.

## Layout & rail persistence

- **What & when.** The entire pane tree (splits, ratios, tabs, active tab, focus, zoom, focus
  mode, per-pane fonts) is saved on the daemon and restored on reload — separately for each
  workspace within each window. The rail width + FILES section are remembered too.
- **How it's used.** Automatic. Reload the page (or reopen a native window) and the exact layout
  returns; a brand-new browser tab starts clean.
- **Where it lives.** `web-ui/src/lib/layout/viewState.ts` (`windowKey` via `sessionStorage`,
  `serializeLayout`/`deserializeLayout`), `railState.ts` (localStorage). Route:
  `GET/PUT /api/v1/view-state/{key}` (server `view_state.rs`, opaque blobs, key
  `[A-Za-z0-9_-]{1,64}`, ≤64KB).
- **Key behaviors.** The window id lives in `sessionStorage` (a reload restores this window; a
  fresh tab is clean). Writes debounce 500ms and flush on `pagehide` with `keepalive` so a close
  never loses state. Restore has a 3s timeout so a hung daemon never leaves a blank stage; it
  prunes dead sessions and 404 files, and a record-shaped tab of an unknown kind (a newer build's
  tab, then a rollback) is skipped rather than nulling the whole pane. Session ids survive a
  daemon restart (see [lifecycle-and-persistence.md](lifecycle-and-persistence.md)), which is what
  lets persisted tabs rebind with no client migration.

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Why the workbench is shaped this way
_Captured 2026-07-09 — drafted from DESIGN.md + code, confirmed live with the maintainer._

- **Problem it solves.** Workspace-first, chat-many — the deliberate inversion of the Claude
  desktop app's chat-first / workspace-weak model. The folder *is* the window (file tree, previews,
  git, and N sessions all scoped to it), not an attribute of a chat.
- **Core vs addition.** The workspace-first model is a **core bet** — don't undo it. Split panes,
  focus mode, and the DnD/keyboard details are **additions**: deliberate (panes exist to put an
  agent and its outputs side by side, superseding the earlier "no tiling WM" non-goal) but
  improvable, not sacred.
- **Do not change:** the workspace-first inversion. Everything else in the workbench can change if
  it's a clear improvement.
