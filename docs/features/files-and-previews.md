# Files & previews

Browsing and managing the workspace's files. The file tree and the Finder browse, open,
and — via their right-click context menus — create, rename, delete, and download files
and folders; the preview service streams file bytes and renders them as code, markdown,
tables, PDFs, images, sandboxed HTML, or a hex/binary summary — plus a light single-file
editor. Everything streams (never whole-file loads) to hold the daemon's ~150 MB RSS
budget on shared login nodes.

**Where it lives (shared):** UI `web-ui/src/lib/previews/` (`files.ts` loaders,
`fileStore.svelte.ts` the content store, `CodeView`, `MarkdownView`, `TableView`, `PdfView`,
`ImageView`, `HtmlView`, `BinaryView`, `FinderView`, `cm.ts`) +
`web-ui/src/lib/workspace/FileTree.svelte` + glyphs in `web-ui/src/lib/shared/`
(`FileIcon`, `FolderIcon`, `icons.ts`). Daemon: **all preview endpoints are in
`crates/chimaera-server/src/fs.rs`** (there is no separate previews module). The file diff
viewer (`DiffView.svelte`) is shared with git — see [git.md](git.md).

## The file tree

- **What & when.** The rail's FILES section: a lazily-loaded directory tree of the workspace
  root. Browse the project and open files into panes.
- **How it's used.** Click a directory to expand/collapse; click a file to open it in the focused
  pane. Start typing (or click the magnifier) to filter the loaded tree; a directory link clicked
  in a terminal/chat reveals + flashes its row. File rows drag out (drop into a pane/split, or
  onto an agent to reference).
- **Where it lives.** `FileTree.svelte`; `fsList()` in `files.ts`. Route
  `GET /api/v1/fs/list?path=&hidden=` (server `fs.rs`).
- **Key behaviors.** Rendered as a flat list of rows (indent = `depth * 13px`), not recursive
  components. Respects `files.showHidden`. Re-lists the root + every expanded dir when the
  workspace's git **epoch** bumps (a new untracked file gets a row to carry its status badge)
  or the client **fs epoch** bumps (any create/rename/delete from any surface — see "File
  management" below) — **coalesced**: a working agent bumps both epochs on every file it writes,
  so the re-list is debounced (~250 ms) into one pass, sparing a remote link a storm of `fs/list`
  calls. Changed files show a right-aligned letter badge (M/A/D/R/C/T/U/!) and a recolored name; a
  collapsed dir containing changes shows a rollup dot.

## File management (create / rename / copy / paste / delete / download)

- **What & when.** Right-click anywhere files show — tree rows, the tree background, Finder
  entries, Finder column backgrounds, file-backed pane tabs — for New File…/New Folder…,
  Copy/Cut/Paste, Rename…, Download (remote only), Copy Path, and Delete…. The FILES section
  header also carries new-file/new-folder buttons targeting the workspace root.
- **How it's used.** Creates are **inline**, VS Code-style: an editable row appears in place;
  the typed name may nest (`a/b/c.txt` creates the intermediate folders). A created file opens
  immediately (pinned). Rename swaps the row (or tab label) for an input with the stem
  preselected; renaming a *terminal/chat tab* pins the session name instead (the "master name"
  pattern — see [workbench.md](workbench.md)). **Copy/Cut/Paste** works from the menu and from
  ⌘/Ctrl+C/X/V while a tree row or Finder is focused (scoped so terminals keep their own
  copy): paste runs a server-side copy/move (bytes never round-trip the browser), copies get a
  macOS "name copy" sibling on collision, a cut row dims until it lands (Escape clears it), and
  a cut into the same folder is a no-op. Files can also be **dragged from the OS desktop** onto
  a Finder column or a FILES-tree folder to upload into it (see
  [drag-drop-and-uploads.md](drag-drop-and-uploads.md)). Delete always confirms in a modal
  (permanent — no server-side trash). Download streams a single file as-is (forced via the
  anchor `download` attribute so it never navigates the native webview), a folder as
  `<name>.zip`; it is **hidden on local workspaces** (the file already lives on this machine)
  and shown only on remote ones, where the window's origin *is* the ssh tunnel.
- **Symlinks.** A symlinked file/dir renders with an italic name and a small alias-arrow badge,
  its `→ target` on hover; navigation still resolves the target (a symlinked dir opens it). A
  **broken (dangling) symlink** is now visible — err-tinted, refuses to open — so it can be
  renamed or deleted (both act on the link itself, never its target).
- **Where it lives.** UI: `shared/contextMenu.svelte.ts` + `ContextMenuHost.svelte` (the one
  right-click menu), `shared/ConfirmDialog.svelte`, `shared/fsNames.ts` (name validation +
  stem preselect), `workspace/fsEvents.ts` (the mutation bus), `workspace/fileClipboard.svelte.ts`
  (the in-app file clipboard + paste). Daemon: `fs.rs` handlers
  `create`/`rename`/`copy`/`move`/`delete` + `crates/chimaera-server/src/download.rs`.
- **Routes.** `POST /api/v1/fs/create {path, kind}` (makes parents; 409 if the target exists),
  `POST /api/v1/fs/rename {from, to}` (409 on existing target; symlink-safe; case-only renames
  allowed; cross-device moves refused), `POST /api/v1/fs/copy {from, to, on_conflict?}`
  (recursive; symlinks recreated as links, never followed; `unique` picks a free "name copy"
  sibling; refuses copying a dir into its own subtree; 250k-entry ceiling), `POST /api/v1/fs/move
  {from, to}` (rename, falling back to a guarded copy+delete across filesystems; refuses `$HOME`
  and dir-into-itself), `POST /api/v1/fs/delete {path}` (recursive; refuses `/` and `$HOME`),
  all bearer-authed. `fs/list` entries now carry `symlink`/`target`/`broken` (additive, absent
  on older daemons). Downloads ride the ticket pattern: `POST /api/v1/fs/ticket` accepts
  directories too, and the unauthenticated `GET /download/{ticket}` streams a file (with
  `Content-Disposition: attachment`, RFC 5987 unicode names) or a zip built on the fly
  (`async_zip` through a 64 KiB duplex — bounded memory, no disk spool; symlinks never
  followed; 250k-entry ceiling aborts loudly). `/raw/{ticket}` stays file-only.
- **Key behaviors.** Every mutation bumps the client `fsEpoch` (tree + Finder re-list from any
  surface's change) and nudges `git::mark_path_dirty`. App subscribes to `lastFsMutation`:
  a rename/move **rewrites open tabs** (file/diff/finder, prefix-aware for folder renames —
  `rewriteTabPaths` in `layout/layout.ts`); a delete closes tabs under the path and retargets
  Finders to the parent (`pruneDeletedPath`). A slow (remote) listing shows a delayed spinner —
  a per-node "listing…" row in the tree, an incoming-column spinner in the Finder. A file tab's
  Rename is disabled while the file has unsaved edits. Escape cancels any inline input; blur
  commits a non-empty valid name.

## Raw reads & lightweight editing

- **What & when.** Ranged byte reads back the code viewer, image/PDF/HTML previews, and the small
  editor.
- **How it's used.** `GET /api/v1/fs/file?path=&offset=&limit=` returns a slice with
  `X-File-Size`/`X-Truncated`/`X-Mtime` headers; `.gz`/`.bgz` files are decompressed transparently
  (offsets address decompressed bytes). `PUT /api/v1/fs/file?path=&expect_mtime=` writes atomically.
- **Where it lives.** `fs.rs` (`file`/`read_file_response`/`read_gz_slice`, `put_file`/
  `write_file_atomic`); `CodeView.svelte` + `cm.ts` (CodeMirror).
- **Key behaviors.** Read chunk cap 2 MB (default 256 KB); PUT body cap 1 MB (editing is for small
  text files — 413 over) and is mtime-guarded (409 "file changed on disk"). Writes go through a
  hidden tmp sibling + rename, keep the original mode, and call `git::mark_path_dirty` so the git
  panel refreshes without polling. Gzip decompress is capped at 64 MB/request (defuses gzip bombs).

## Rendered previews

- **Markdown.** `GET /api/v1/fs/markdown?path=` renders GFM → **ammonia-sanitized** HTML
  (source cap 4 MB). `MarkdownView.svelte`.
- **Tables (CSV/TSV, incl. gzip).** `GET /api/v1/fs/table?path=&offset_rows=&limit_rows=&delim=auto`
  returns one page (header row + string cells; rows cap 1000/page; delimiter auto-sniffed; `.gz`/`.bgz`
  transparent). `TableView.svelte`. Bioinformatics reality — big delimited files are the norm.
- **Spreadsheets (xlsx/xls/xlsm/ods).** `GET /api/v1/fs/xlsx?path=&sheet=&offset_rows=&limit_rows=`
  parses the workbook server-side (**calamine**) into the same paged `TablePage` (first row = header),
  plus the workbook's `sheets` list. `XlsxView.svelte` renders a sheet picker over the shared
  `TableView` grid — so selection/resize/paging come for free. calamine loads a whole sheet into
  memory, so the SOURCE file is size-capped (`MAX_XLSX_BYTES`, 8 MB) before parsing, off the reactor
  (`spawn_blocking`); over-cap files get an honest "too large" message. No live-on-disk refresh (no
  store entry) and no editing (a spreadsheet isn't a text file). **Gotcha:** XlsxView must NOT hand
  its own `$state` page object to `TableView` — the shared deeply-reactive proxy cross-links the two
  components' reactive graphs into a freeze; `TableView` fetches its own plain page via a *stable*
  `fetchPage`.
- **PDF / image / HTML.** Fetched via a short-lived **ticket**: `POST /api/v1/fs/ticket {path}` →
  `GET /raw/{ticket}` (no bearer header — iframes/`<img>`/pdf.js can't send one; ticket TTL 600s,
  range-aware). HTML is sandboxed (`CSP: sandbox allow-scripts`, no-referrer); SVG is sandboxed too.
  `PdfView`/`ImageView`/`HtmlView`.
- **Binary / Finder.** Non-text files get a hex/summary view (`BinaryView`); `FinderView` is a
  directory browser surface.

## Preview caching & live-update

- **What & when.** Every preview reads its content through a shared, LRU-capped content store
  (`previews/fileStore.svelte.ts`) keyed by path — the file-side analogue of the terminal pool.
  A view `retain`s its entry on mount, `release`s on unmount.
- **Where it lives.** `previews/fileStore.svelte.ts` (`FileEntry`, `retain`/`release`/`noteWrite`);
  every `*View.svelte` reads from it. It subscribes to `workspace/fsEvents.ts` (`fsEpoch`/
  `lastFsMutation`) + `workspace/git.ts` (`gitStatus.epoch`).
- **Key behaviors.** Switching pane-tabs (or panes) to a recently-viewed file is instant and
  re-hits **no** route — the cached first 256 KB chunk / rendered markdown / first table page /
  `/raw` ticket render immediately (the LRU caps browser memory; large files still stream
  per-view). On a coalesced fs/git bump the store re-probes the **on-screen** paths' mtime (a
  1-byte read carries `X-Mtime`) and, when it moved, refreshes their payloads **in place** (never
  nulling — a null chunk would unmount a live `CodeView`) — so an agent or external edit to a file
  you have open updates the view live. An **unsaved** `CodeView` buffer is never clobbered: a disk
  change while dirty raises the "changed on disk" conflict bar instead of reloading.

## File & folder glyphs

- **What & when.** One visual language for files/folders across the tree, git rows, tabs, and
  quick-open, so a file looks the same everywhere.
- **Where it lives.** `web-ui/src/lib/shared/FileIcon.svelte`, `FolderIcon.svelte`; resolution
  `iconFor` in `files.ts` / `icons.ts`.
- **Key behaviors.** `FileIcon` picks a vendored Tabler glyph by exact filename first (Dockerfile,
  lockfiles, `.gitignore`) then extension (a gzip wrapper resolves by inner extension, e.g.
  `foo.tsv.gz` → table glyph), tinted per category (`--ficon-*`). Note the **`bio`** category — a
  bioinformatics-aware tint tier, consistent with the audience. `FolderIcon` has an open variant used
  while a tree dir is expanded. All colors are theme tokens.

## Key constraints

- Every listing/read runs under `spawn_blocking` — a slow Lustre `read_dir` must never wedge a tokio
  worker. Directory listings cap at `MAX_DIR_ENTRIES = 4000` with an honest `truncated` flag.
- Previews **stream**; a preview of a huge Parquet/HTML/CSV must never balloon memory. This is a
  review criterion, not a nice-to-have (see [rules/daemon.md](../../.claude/rules/daemon.md)).

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Why previews (and lightweight editing) are shaped this way
_Captured 2026-07-09 — drafted from DESIGN.md + code, confirmed live with the maintainer._

- **Problem it solves.** Previews are the durable **moat** — the part Anthropic won't build —
  because the deliverable of an agent session (especially in bioinformatics) is usually *files*
  (plots, MultiQC reports, tables, PDFs), not the conversation.
- **Core value, will extend.** The preview layer is core value and **will be extended** (more
  formats over time). Lightweight single-file editing is deliberately in scope; the firm **non-goal**
  is a real editor — no LSP, completions, multi-file refactor, or debugger (serious editing lives in
  real editors; agents write most code).
- **Do not change:** the no-IDE-editor boundary, and streaming (never whole-file loads). The set of
  preview formats is expected to grow.

### File management (context menus, create/rename/delete, downloads) — why it exists
_Captured 2026-07-10 (from the maintainer)._

- **Problem it solves:** downloads are the heart of it — *"when on a remote it is nice to get the
  files to your local desktop."* The shaping constraint: *"you don't have local files on your
  remote"* — a remote workflow strands your outputs on the cluster, and the download menu brings
  them home. The rest (create/rename/delete, the context menus, the master-name rename) rounds out
  the file surfaces around that.
- **How settled it is:** the maintainer intends to keep the current behavior but explicitly did not
  want hard promises (*"I intend to keep this but could change"*). Grade: everything here is an
  **addition**, not a core bet — improve freely if a better shape appears.
- **Do not change (or: open to change):** open to change (*"can change"*). Nothing in this
  capability is frozen; only the remote→local retrieval *why* is settled.
- **Folded in 2026-07-11 (#46):** copy/cut/paste of files & folders (server-side — bytes never
  round-trip the browser), symlink marking (+ visible broken symlinks), and OS-desktop drops **into a
  folder** are the same capability rounding out the file surfaces — same *why*, same **addition**
  grade (open to change). The download-hidden-on-local / shown-on-remote rule follows directly from
  the remote→local retrieval *why* above.
