# Files & previews

Browsing and managing the workspace's files. The file tree and the Finder browse, open,
and — via their right-click context menus — create, rename, delete, and download files
and folders; the preview service streams file bytes and renders them as code, markdown,
tables, PDFs, images, video and audio, sandboxed HTML, Jupyter notebooks, program logs,
Marp slide decks, mermaid diagrams, or a binary info card — plus a light single-file
editor. Everything streams (never whole-file loads) to hold the daemon's ~150 MB RSS
budget on shared login nodes.

**Where it lives (shared):** UI `web-ui/src/lib/previews/` (`files.ts` loaders,
`fileStore.svelte.ts` the content store, `CodeView`, `MarkdownView` + `mdDoc.ts` /
`docLinks.ts`, `TableView`, `PdfView`, `ImageView`, `MediaView`, `HtmlView`, `BinaryView`,
`NotebookView` + `notebook.ts`, `LogView` + `logText.ts`, `SlidesView` + `marp.ts`,
`MermaidView`, `RawTextView`, `ansi.ts`, `FinderView`, `cm.ts`) +
`web-ui/src/lib/workspace/FileTree.svelte` + glyphs in `web-ui/src/lib/shared/`
(`FileIcon`, `FolderIcon`, `icons.ts`). Daemon: **the preview endpoints are in
`crates/chimaera-server/src/fs.rs`**, except the notebook pager (`notebook.rs`). The file diff
viewer (`DiffView.svelte`) is shared with git — see [git.md](git.md).

## The file tree

- **What & when.** The rail's FILES section: a lazily-loaded directory tree of the workspace
  root. Browse the project and open files into panes.
- **How it's used.** Click a directory to expand/collapse; click a file to open it in the focused
  pane. Start typing (or click the magnifier) to filter the loaded tree; the **collapse folders**
  button beside it folds every open dir and returns to the top. A directory link clicked
  in a terminal/chat reveals + flashes its row. File rows drag out (drop into a pane/split, or
  onto an agent to reference).
- **Finding your place in a deep tree.** **Indent guides** — one hairline per depth under each
  level's chevron; hovering a row draws its parent folder's guide strong, so "which folder am I
  in" is answered without scrolling up. **Sticky ancestors** — while scrolled inside a folder,
  the ancestor dir rows of the first visible row pin to the top of the tree as a shelf (nearest
  three levels); click one to scroll to that folder's real row, click its chevron to collapse it.
  **Collapse anchoring** — collapsing a folder whose row has scrolled out of view (from its
  sticky copy, or the keyboard) brings that row back under its ancestors instead of leaving the
  viewport on unrelated content; a visible row stays put, and expanding never moves the row.
- **Where it lives.** `FileTree.svelte`; `fsList()` in `files.ts`. Route
  `GET /api/v1/fs/list?path=&hidden=` (server `fs.rs`).
- **Key behaviors.** Rendered as a flat list of rows (indent = `depth * 13px`, driven by a
  per-row `--depth` custom property that also draws the guides as a background gradient — no
  guide DOM), not recursive components. The tree is **its own scroller** (the rail body around
  it does not scroll): the sticky shelf is a zero-height `position: sticky` anchor at the
  scroller's top, recomputed once per frame on scroll or row change by probing
  `elementsFromPoint` (the inline create/error/listing rows make row arithmetic unreliable),
  and the scroller opts out of the browser's own scroll anchoring: the component anchors **every**
  row change itself (a pre-DOM snapshot of the first row under the shelf, restored once the new
  rows are in — WebKit has no anchoring of its own, so a relist above the viewport would
  otherwise shift the rows), and a collapse re-anchors on the collapsed row. **OS-desktop drops** (see
  [drag-drop-and-uploads.md](drag-drop-and-uploads.md)) name their destination: the targeted
  dir row gets an accent ring + wash and its expanded descendants a light wash (hovering a file
  targets its parent), the root target keeps the whole-tree frame, and a sticky
  `upload into <folder>/` label pins to the top of the tree for the length of the drag. App
  passes the exact folder via `dropDir` (null when no file drag is over the tree).
  Respects `files.showHidden`. Re-lists **only the dirs whose direct listing could
  have changed** — not the whole tree — when the workspace's git **epoch** bumps (parents of
  paths that entered/left the git dirty set: a symmetric diff, so both a new untracked file and a
  removal are caught), the client **fs epoch** bumps (the exact parent of that
  create/rename/delete — both parents for a rename; see "File management" below), or the daemon's
  mounted-path monitor reports that a visible directory changed on disk (covers ignored/non-Git
  paths and Finder locations outside the workspace). **Targeted +
  debounced (~250 ms) + coalesced** into one pass, so a working agent re-lists just the folder it
  touched, not every expanded dir — sparing a remote link a storm of `fs/list` calls. A
  modified-in-place file changes no listing (its badge updates reactively via `gitIndex`); a
  change under a collapsed dir needs no relist (its rollup dot is reactive too). Changed files show
  a right-aligned letter badge (M/A/D/R/C/T/U/!) and a recolored name; a collapsed dir containing
  changes shows a rollup dot.
- **Large-directory safety.** `fs/list` returns at most 1000 entries and carries an honest
  `truncated` flag. The tree and Finder surface that partial-listing state instead of silently
  looking complete, and offscreen Finder rows use browser layout containment. Filesystem preview
  requests share an eight-operation blocking-work ceiling, so a burst against a slow NFS/Lustre
  mount queues without consuming the daemon's async workers or an unbounded blocking-thread pool.

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
  (`async_zip` through a 64 KiB duplex — bounded memory, no disk spool). The ticket target is
  opened once without following symlinks; folder traversal stays anchored to that descriptor and
  opens every component relative to it with symlink following disabled. A 250k-entry ceiling plus
  separate 8 MiB ceilings for one directory's retained names and the DFS stack's full relative
  paths abort loudly before a wide/deep tree can amplify traversal memory. `/raw/{ticket}` stays
  file-only and streams byte ranges from disk instead of materializing the whole file.
- **Key behaviors.** Every mutation bumps the client `fsEpoch` (tree + Finder re-list from any
  surface's change) and nudges `git::mark_path_dirty`. App subscribes to `lastFsMutation`:
  a rename/move **rewrites open tabs** (file/diff/finder, prefix-aware for folder renames —
  `rewriteTabPaths` in `layout/layout.ts`); a delete closes tabs under the path and retargets
  Finders to the parent (`pruneDeletedPath`). A slow (remote) listing shows a delayed spinner —
  a per-node "listing…" row in the tree, an incoming-column spinner in the Finder. A file tab's
  Rename is disabled while the file has unsaved edits. Escape cancels any inline input; blur
  commits a non-empty valid name. Finder descents reveal the new column with the smallest possible
  horizontal movement; refreshes preserve the user's horizontal position and re-list only affected
  visible columns, coalescing mutation and disk-watch bursts.

## Raw reads & lightweight editing

- **What & when.** Ranged byte reads back the code viewer, image/PDF/HTML previews, and the small
  editor (code, markdown live/source, HTML edit/split — all one `CodeView`).
- **How it's used.** `GET /api/v1/fs/file?path=&offset=&limit=` returns a slice with
  `X-File-Size`/`X-Truncated`/`X-Mtime` headers, plus `X-Content-Hash` (SHA-256) when the body is
  the whole raw file; `.gz`/`.bgz` files are decompressed transparently (offsets address
  decompressed bytes). `PUT /api/v1/fs/file?path=&expect_hash=` (or `expect_mtime=` against a
  daemon that never sent a hash) writes atomically. Cmd/Ctrl+S saves; Mod-f searches (every
  editor, read-only ones too); the **Autosave** setting (`editor.autosave` off | after a delay,
  `editor.autosaveDelay`) saves after idle typing and on blur / tab switch.
- **Where it lives.** `fs.rs` (`file`/`read_file_response`/`read_gz_slice`, `put_file`/
  `write_file_atomic`); UI `previews/buffers.svelte.ts` (the buffer store), `CodeView.svelte` (a
  view onto a buffer) + `cm.ts`, `textCodec.ts` (byte fidelity), `merge.ts` (three-way merge over
  `node-diff3`), `drafts.ts` (the journal), `CompareView.svelte`, `layout/CloseDirtyDialog.svelte`.
- **Buffers outlive views.** The buffer store owns each open file's `EditorState` (text, undo
  history, cursor) and what it knows about the disk (base text, content hash, mtime token, line
  endings/BOM). A `CodeView` attaches to it; unmounting detaches, and a buffer with unsaved edits
  lives on — closing a pane, the keep-alive cap, split, zoom, a tab drag, a workspace switch and a
  rename of the file or a parent (the buffer re-keys) all come back to the same text and undo
  stack. `shared/editing.ts`'s `dirtyFiles` mirrors the store, so the tab dot, beforeunload and
  the reload gate protect an unmounted dirty buffer too. A clean buffer is forgotten with its last
  view.
- **Closing asks.** Every close of a tab whose file is dirty — ×, middle-click, Close / Close
  Others / Close All, Cmd/Ctrl+W and the close-view chord — opens **Save / Don't save / Cancel**
  (one dialog for several files). A save that fails keeps its tab open with the reason. Save
  waits at most 15 s (a dead link): past that it reports "not saved" and hands control back
  while the save carries on in the background. Cancel and Escape stay live while saving and
  only keep the tabs open.
- **Saves are verified.** A save sends the base's content hash; success (whose reply carries the
  new hash) marks the buffer clean only if nothing was typed since it was sent (save
  generations). A save gets a 20 s timeout and one automatic retry once the events link is back
  (the precondition makes it idempotent: the daemon answers success without writing when the disk
  already holds the bytes), with a visible "not saved, retrying" / "offline" status. A save is
  refused — never a silent overwrite — while a conflict is open.
- **Disk changes merge instead of clobbering.** When the file moves on disk (an agent editing it),
  the buffer reads it whole and hashes it: a clean buffer reloads in place (cursor kept), a dirty
  one three-way merges base / mine / disk line-wise. A clean merge applies as minimal edits with
  a quiet "Merged changes from disk" notice (view diff, undo); overlapping or adjacent edits raise
  the conflict bar — **compare** (a side-by-side diff), **keep mine** (the disk becomes the base,
  so the next save deliberately wins), **take disk** (undoable). The disk token is adopted only
  together with the content it names, and a read that raced a save is discarded, so our own write
  never reads as a conflict. A 409 on save takes the same path.
- **Byte fidelity.** Files up to the 1 MB edit cap load in one request. Uniform CRLF or lone-CR
  line endings, a UTF-8 BOM and the final newline round-trip exactly (the editor holds LF text;
  the save re-joins with the file's own break). Mixed line endings, text that is not valid UTF-8,
  compressed files and anything past the cap open **view-only** with a note saying why.
- **The draft journal.** About a second after typing stops (and on hide/pagehide) dirty text is
  written to IndexedDB and mirrored to the daemon (`PUT /api/v1/fs/drafts`, ≤ 1 MiB, under
  `~/.chimaera/drafts`; a listing reads only each draft's small metadata sidecar, and the client
  reuses one for 3 s across opens) — a new tunnel port is a new origin with an empty IndexedDB.
  Opening a file whose draft differs from the disk shows "Recovered unsaved changes from …" with Restore /
  Discard, never a silent restore; a draft typed against an older disk version restores through
  the merge. When both copies exist with different text the newer wins by the writers' own
  clocks (each PUT carries the client's `updated_ms`, answered back as `client_updated_ms`), not
  the daemon's arrival stamp, which runs on another machine's clock; a mirror copy from an older
  client falls back to that stamp. A save of exactly that text, or a discard, clears both copies; mirror writes for a
  path land in issue order, so a late journal write never re-creates a cleared draft. A
  hide/pagehide flush sends drafts as keepalive requests only within a shared 56 KiB budget of
  encoded body bytes (the browser's keepalive quota); the rest go as normal requests, with the
  IndexedDB copy regardless. A journal that failed
  everywhere shows "draft not backed up" in the status bar. Older daemons without the routes fall
  back to IndexedDB only.
- **Other windows.** Same-origin windows announce dirty paths over a `BroadcastChannel`; a window
  showing a file another one holds unsaved shows "Unsaved edits in another window".
- **Key behaviors.** Read chunk cap 2 MB (default 256 KB); PUT body cap 1 MB (editing is for small
  text files — 413 over). Writes go through a hidden tmp sibling + rename, keep the original
  mode, and call `git::mark_path_dirty` so the git panel refreshes without polling. Gzip
  decompress is capped at 64 MB/request (defuses gzip bombs). Open-at-line requests
  (`shared/reveal.ts`) place the cursor, center the range and flash it.

## Rendered previews

- **Markdown.** `MarkdownView.svelte`, with Obsidian-style modes **live | reading | source**.
  A file opens in the mode it was last shown in — remembered per file in this browser's
  `localStorage` (the 300 most recently opened; `mdDoc.ts` `createModeMemory`, every access
  guarded, so a private window just forgets) — else in the **Markdown Default Mode** setting
  (`editor.markdownDefaultMode`: reading, live or source; **reading** by default). Opening in
  reading costs one request, the render: the source is fetched, and the editor mounted, on the
  first live/source click. Files over the 1 MB edit cap and binary-content files always open
  in reading and stay there (an editor click on one says why in the mode bar).
  - **live** is an *editable reading view* — the shared CodeMirror editor
    (`CodeView`) carrying the `mdLive.ts` extension set (`@codemirror/lang-markdown`, GFM
    base): headings sized, emphasis/links/inline code styled, syntax marks hidden on every
    line the selection doesn't touch, images/task-checkboxes/rules rendered as widgets
    (clicking a checkbox edits the source), blockquotes drawn as the shared quote card, and
    a small always-visible copy affordance on each fence line and quote card. **Equations**
    — `$…$` and `$$…$$`, Obsidian's dollar dialect, plus GitHub's ```` ```math ```` fence
    (the same block under another name; `` $`…`$ `` and Codex's `\(`/`\[` are not recognized
    in files) — typeset as KaTeX widgets; click one, or move the cursor onto its lines, and
    the LaTeX shows as mono source. A line opening with `$$` starts a **block** whose lines
    are raw until the first later line containing `$$` (so a continuation line like
    `+ \left(1-w\right)` can't become a bullet list), but only when that closer is in sight
    before the next blank line: prose that merely starts with `$$` stays prose, and a slip
    costs at most a paragraph. The server mirrors the block rule by promoting such blocks to
    comrak's math fence before parsing (`fs.rs` `promote_math_blocks`) and hands every
    equation over as the same `span`; being a line pass, it reads a block inside a list item
    indented four columns or more as code, where the editor still renders it. `mdMath.ts` is
    the parser extension; its delimiter rules mirror comrak's `math_dollars` so live and
    reading agree on what is math (`$5 and $10` stays currency, while `$HOME/$USER` in prose
    becomes math — as on GitHub), and one case list, `previews/mathBlocks.fixture.json`, pins
    the block grammar for both sides: the Vitest and Rust suites each run every case, with
    the known divergences named in it.
    **Tables** render as the reading view's grid (the shared "Markdown tables" recipe, so the
    views agree cell for cell — alignment, inline formatting, links, images, `$` math through
    the same KaTeX policy) while the cursor is outside them. Click a cell, or move the cursor
    into the table, and the whole table shows as editable mono text with its pipes aligned
    (rows stay on one line; a wide one scrolls the editor sideways), the cursor in the
    clicked cell — a row shorter than the header gets its missing pipes written first, so
    typing lands in the clicked column — and it re-renders the moment the cursor leaves
    (whole-table reveal, not in-place cell editing). Like a `$$` block, a table is replaced
    whole from the `blocks` state field (`mdLive.ts`), built as data from the syntax tree
    (`mdTable.ts`) and turned into elements without any HTML injection; a wide one is a tab
    stop while it overflows, like chat's. Raw HTML and frontmatter stay as mono source, and so
    does any construct the decorator can't render faithfully (reference links, multi-line
    image syntax). **Mod+click follows links** (a plain click places the cursor) through the
    same routing as reading, below — Mod+Shift+click opens a file link beside, and a
    same-document `#heading` or `#L12` reveals its line in the editor (a heading is mapped to
    its line through the daemon's render, so the slug is comrak's). Right-click gives the same
    URL menu as reading.
  - **reading** is the complete non-editable render: `GET /api/v1/fs/markdown?path=` →
    `{html, frontmatter}` — server-side comrak GFM (+ `math_dollars`) → **ammonia-sanitized**
    HTML (source cap 4 MB), fetched on first entry and refreshed in place on saves/agent
    writes. `data-math-style` on `span` marks each LaTeX literal, which the client typesets
    under the one KaTeX policy every surface shares (`shared/math.ts`, loaded on demand at the
    first equation, memoized, time-sliced). The render also carries what the reading view
    builds on (older daemons omit it, and each piece degrades to the old behavior):
    - **Properties.** A leading `---` YAML block comes back as raw `frontmatter` text instead
      of rendering as a rule and a heading, and shows as a compact, collapsible key/value
      panel above the document (collapsed state remembered in this browser). A tiny tolerant
      reader (`mdDoc.ts` `parseFrontmatter`, no YAML library) handles `key: value`,
      `key: [a, b]`, `- item` lists, `|`/`>` blocks and true/false (a check box); a nested
      value shows as its source, and a block it can't read at all shows whole as source.
      Every value is text, never markup. Live mode keeps frontmatter as muted source.
    - **Alerts.** GitHub's `> [!NOTE]` / `[!TIP]` / `[!IMPORTANT]` / `[!WARNING]` /
      `[!CAUTION]` arrive with comrak's `markdown-alert-*` classes and render as a tinted card
      with a colored rule and a title row led by the type's glyph — semantic theme tokens
      (`--syn-func`, `--syn-string`, `--rate`, `--warn`, `--err`), so every curated theme
      restyles them.
    - **Task boxes.** `- [x]` items arrive as `span.md-task[data-task=done|todo]` (the
      sanitizer drops `<input>`), drawn as live's check box in place of the bullet; a done
      item's own text is muted and struck through, as in live.
    - **Anchors.** Heading and footnote ids carry GitHub's `user-content-` prefix, so a
      document can't clobber the app's own ids; a `#my-heading` or `#fn-1` link finds
      `user-content-my-heading` inside *this* document (never a global lookup — every open
      document shares the page) and scrolls there without touching `location.hash`.
    - **Line mapping.** Every block carries `data-sourcepos` (lines of the original file,
      frontmatter included — the properties panel stands in for its lines). A selection's
      reference chip now names the lines its two ends sit in, and a **reveal** (a `#L12`
      link, `shared/reveal.ts`) scrolls to the tightest block holding the line and flashes
      it — only while reading shows; in live/source the editor takes the reveal
      (`acceptReveal` on `CodeView`).
    **Tables scroll, never squeeze** — one recipe shared with the chat transcript and the
    live mode's table widget (`web-ui/src/app.css`, "Markdown tables"; [chat mode](chat-mode.md))
    minus their host wrapper: the `<table>` itself is the horizontal scroller, so a table whose
    columns can't fit the reading column scrolls in place (prose cells still wrap at spaces).
    GFM `:--:` / `--:` alignment is honoured through the `align` attribute comrak emits and the
    sanitizer keeps; numerals are tabular. Unlike chat, headers wrap like any cell, and a
    hand-written `<table>` in the file gets the same scroller (the sanitizer still reshapes it:
    no `tfoot`, no `width` / `style`). A table, fence or display equation wider than the column
    is a tab stop while it overflows (`shared/scrollRegion.ts`, re-checked on pane resize, text
    size, and late layout); the reading pane itself is a region named after the file, so Tab
    reaches it and arrow keys scroll — WebKit never makes a scroller focusable on its own. The
    selection's reference chip follows any of them scrolled sideways, not only the pane's own
    scroll.
  - **source** is the same editor as plain raw markdown (an extension swap in CodeView's
    `extra` compartment — never a remount, so the buffer, undo history, and dirty state
    survive every toggle; the file is only written on Cmd/Ctrl+S).

  **Links in documents open** (`previews/docLinks.ts`, both modes). A document-relative path
  (`other.md`, `../data/run.csv`, `figs/a%20b.png`, a local `file://` URL) resolves against
  the document's folder, a root-relative `/docs/x.md` also against the workspace root, and
  the daemon confirms it (`POST /fs/validate`, `strict`: only the exact join, so a broken
  `b/spec.md` never opens `spec.md`) before anything opens; the file (or folder)
  then opens through the shared opener (`shared/openPath.ts`, registered by the app) —
  Cmd/Ctrl+click or a middle-click opens it beside. `#L12` / `#L12-L20` opens the file at
  those lines (a reveal); `other.md#heading` opens the document and then scrolls to the
  heading once it has rendered (a small pending-anchor map, keyed by path). A link that
  resolves to nothing shows a brief inline "not found" hint where it was clicked; web URLs
  keep their routing (a live local app in a browser pane, anything else in the real
  browser); mailto:/tel: stay the browser's; any other scheme is dropped.

  Rendered documents carry the workbench's reading chrome: fenced code blocks and blockquotes
  get the same hover copy button as the chat transcript (`shared/copyDecor.ts`, one decorator
  for both surfaces), and **document-relative images** (a `figs/plot.png`-style src) resolve
  against the file's directory through short-lived `/raw/` tickets (`rawTicketUrl`, memoized
  so re-renders keep the `src` stable) — absolute URLs pass through untouched in reading
  (the server sanitizer already constrains them), while live's image widgets render only
  http(s)/`data:image` URLs and leave other schemes as source. The live mode mirrors all of
  it (quote cards, ticketed image widgets, fence + quote copy) inside the editor; raw HTML
  in a document is **never rendered** there — it stays visible source.

  The toolbar carries a quiet **"N issues"** chip (`previews/DocIssues.svelte`) when the
  daemon's portable-dialect check (`GET /api/v1/fs/check_document`) finds errors or warnings:
  broken links and embeds, missing anchors, absolute paths, missing alt text, syntax GitHub
  shows as literal text. Clicking an issue reveals its line; it re-checks after disk changes,
  only while visible. Details:
  [agents.md](agents.md#documents-the-portable-dialect-check_document-and-the-issues-chip).
- **Tables (CSV/TSV and bioinformatics text, incl. gzip).**
  `GET /api/v1/fs/table?path=&offset_rows=&limit_rows=&delim=auto` returns one page (header row +
  string cells; rows cap 1000/page; delimiter from the name, else sniffed from the first line that is
  not a comment; `.gz`/`.bgz` transparent). Bioinformatics reality — big delimited files are the
  norm. Additive options read the genomics formats: `comment=` (comma-separated line prefixes,
  skipped wherever they appear), `header=false` (every line is data; the columns are `names=` then
  `colN`), and `quote=false` (a SAM quality or VCF text may open a field with `"`). `files.ts`
  (`tablePreset`, `tableQuery`) routes them to the table view and sets the options: **VCF** (`##`
  lines skipped; the `#CHROM` line is the header, shown as `CHROM`; `.vcf.gz` too), **BED /
  bedGraph / narrowPeak / broadPeak** (`#`, `track` and `browser` lines skipped; the standard BED
  column names), **GFF / GFF3 / GTF** (`#` skipped; the nine standard names) and **SAM** (`@`
  header lines skipped; the eleven mandatory names, then `colN` for optional tags). Every answer
  carries `total_rows` once a scan has reached the end, else `est_rows` (a byte-rate estimate from
  the rows walked plus two sampled windows of the rest).
  **Deep pages seek.** A plain file keeps a sparse row-offset index in the daemon
  (`fs/row_index.rs`): a byte offset per 1,000 data rows, keyed by path + parse options and
  dropped when the file's version token changes; at most 16,384 offsets per file (past that the
  stride doubles) and 32 files (LRU) — ~4 MB at worst. A request seeks to the nearest checkpoint
  and walks at most 64 MB; one that runs out before its offset answers `scan_limited` with
  `scanned_to`, and asking again resumes where it stopped. Gzip cannot seek, so `.gz` still
  decodes from the start under its 64 MB decompression cap.
  **The grid** (`TableView.svelte`; its arithmetic in `tableGrid.ts`) keeps only the rows in view
  plus overscan in the DOM, between two spacer rows. The loaded window pages in both directions as
  you scroll, capped at 20,000 rows (paging past that drops the far end). Columns start at widths
  counted from the first page in the mono font's advance — a fixed layout, so nothing shifts while
  scrolling — widen as longer cells page in, and can be dragged or double-click auto-fit. The
  footer says what is loaded (`row 1,499,998 · 600 loaded rows of ~2M`, or `of 5,000` once all
  of it is), and its row field jumps: a loaded row scrolls into view; any other row fetches a
  window around it (asking again while the daemon reports `scan_limited`, with `indexing… row N`
  in the footer) and flashes. Measured live: row 1,500,000 of a 2M-row, 100 MB TSV in ~0.8 s cold
  (two budgeted scans) and ~50 ms once indexed. Double-click a cell to read all of it in a popover
  (column, row, length, copy; Escape or a click outside closes it). Drag or shift-click selects
  cells, the gutter selects rows, and ⌘/Ctrl+C copies them as TSV.
- **Spreadsheets (xlsx/xls/xlsm/ods).** `GET /api/v1/fs/xlsx?path=&sheet=&offset_rows=&limit_rows=`
  parses the workbook server-side (**calamine**) into the same paged `TablePage` (first row = header),
  plus the workbook's `sheets` list. `XlsxView.svelte` renders a sheet picker over the shared
  `TableView` grid — so selection, resizing, paging, virtualization, jump to row and cell expand
  come for free. calamine loads a whole sheet into
  memory, so the SOURCE file is size-capped (`MAX_XLSX_BYTES`, 8 MB) before parsing, and ZIP-backed
  workbooks are preflighted at 64 MiB expanded / 4096 entries before calamine runs off the reactor
  (`spawn_blocking`); over-cap files get an honest "too large" message. A disk change remounts the
  workbook preview on the fresh mtime; there is no editing (a spreadsheet isn't a text file).
  **Gotcha:** XlsxView must NOT hand
  its own `$state` page object to `TableView` — the shared deeply-reactive proxy cross-links the two
  components' reactive graphs into a freeze; `TableView` fetches its own plain page via a *stable*
  `fetchPage`.
- **PDF / image / HTML.** Fetched via a short-lived **ticket**: `POST /api/v1/fs/ticket {path}` →
  `GET /raw/{ticket}` (no bearer header — iframes/`<img>`/pdf.js can't send one; ticket TTL 600s,
  range-aware). HTML is sandboxed (`CSP: sandbox allow-scripts`, no-referrer); SVG is sandboxed too.
  `PdfView`/`ImageView`/`HtmlView`. `ImageView` takes png, jpg, gif, webp, svg, bmp, ico and
  avif (formats every supported webview decodes). `HtmlView` carries a **preview | split | edit** toggle
  (`SplitEditPreview.svelte` owns the split geometry; a failed source fetch is retried on the
  next split/edit click);
  its split live-preview is a `sandbox="allow-scripts"` `srcdoc` iframe fed the (debounced) editor
  buffer — same origin-less isolation. **Relative assets load in neither mode today:** `/raw` is
  the single-segment `/raw/{ticket}`, so a page's `app.js` or `figs/a.png` 404s or falls through
  to the app shell; only self-contained HTML (e.g. MultiQC) renders fully. The planned fix is a
  directory-scoped `/raw/{ticket}/{*path}` ([plan](../document-workbench-plan.md#phase-4-embeds-in-documents-and-in-chat)).
  **PDF** runs on pdf.js's **legacy** build: the modern build calls `Map.prototype.getOrInsertComputed`
  on every render and range read, which WebKit (the macOS app) and Chromium before 145 lack, so its
  pages stayed blank. pdf.js's standard fonts, CMaps, wasm decoders and ICC profiles ship with our
  own build under `assets/pdfjs-<version>/` (the `pdfjsAssets` plugin in `vite.config.ts` copies
  them from `pdfjs-dist`; never a CDN), so a PDF that names Helvetica without embedding it paints
  its text. Bytes load lazily — `disableAutoFetch` + `disableStream` over the ranged `/raw` ticket,
  so a remote tunnel carries only the 64 KB chunks the visible pages need. Every page gets a
  placeholder sized like page 1 at once (the scrollbar, jumps and reveals work immediately) and a
  background walk corrects odd-sized pages in batches. The toolbar shows **p / N** for the page
  being read (the one under a line a third of the way down the viewport, computed from the slot
  sizes, no DOM reads) and takes a page number: type it and press Enter to jump (Escape reverts).
  - **Outline** — a toolbar button (only when the document has bookmarks) opens them as a sidebar,
    first level expanded and deeper levels on demand; a click resolves the destination (named or
    explicit; `XYZ`, `FitH` and `FitR` land on their point) and scrolls there. Open or closed is
    remembered per tab.
  - **Links** — pdf.js's `AnnotationLayer`, given only the page's link annotations (no forms, no
    scripting) and a small link service: internal destinations navigate in place; http(s) URLs go
    through `activateUrl` (a live local app opens in a pane, anything else in the real browser);
    any other scheme is inert.
  - **Find** — ⌘/Ctrl+F anywhere in the view (or the magnifier) opens a find bar. It walks every
    page's text starting at the page being read, one page per task so the UI stays responsive,
    and counts as it goes (`2 / 3`); Enter and Shift+Enter step, scrolling to matches on pages not
    rendered yet too. Matching is case-insensitive, lets words run together or break across lines,
    and knows the ﬁ/ﬂ/ﬀ ligatures (`pdfFind.ts`); matches are highlighted on the text layer, the
    current one stronger. Pages over the 5,000-item text ceiling are not searched, and the bar
    says how many; the count stops at 5,000 matches.
  - **Reveals** — a reveal carrying `page` (`#page=N`) jumps there, and a `region` (`#xywh=`, in
    PDF points from the page's top-left at 100%) is outlined on that page and scrolled into view;
    a reveal wins over the remembered scroll position.

  The text layer uses pdf.js's own sizing (font size and scale from `--total-scale-factor` on the
  page), so selection and highlights sit on the glyphs and follow a zoom at once. Rasters cap at
  12M pixels and inactive canvases use an 8-page LRU. Selectable text is a bounded
  enhancement: a page over 5,000 text items stays canvas-only, and the viewer keeps at most 12,000
  text items across retained pages; the toolbar says `selection limited` when either ceiling is hit.
  This protects highly-compressed vector plots whose small PDF stream expands into tens of thousands
  of browser nodes. Concurrent preview mounts also share and await one in-flight raw-file ticket, so
  the first mount cannot observe an unfinished URL mint and require a second open. Closing or
  evicting a PDF cancels its active pdf.js raster tasks, text reads, text layers, delayed rerenders,
  and restoration frame before destroying the document worker, so invisible work cannot keep a pane
  or webview busy.
  **Image region reveals** — a reveal carrying a `region` (`#xywh=`, in image pixels) is clipped
  to the image, outlined with an accent frame (a veil over the rest fades out), and framed so it
  fills about half the view (never below fit, never past 400%) (`imageRegion.ts`); the toolbar
  shows its coordinates with frame-again and clear buttons.
- **Video / audio.** mp4, webm, m4v, ogv and mov play in the native `<video>` player; mp3, wav,
  m4a, flac, ogg, oga, opus and aac in `<audio>` (`MediaView.svelte`). Bytes come from the same
  ticketed `/raw/` URL — the daemon serves single byte ranges, so seeking fetches only what it
  needs and nothing is buffered daemon-side. A player keeps issuing range requests for as long
  as it plays, past the ticket's 10 minutes: a load that fails on an aged ticket re-mints it
  and resumes at the same time (as does a re-mint after the file changes on disk). A failure on
  a fresh ticket is the format — a container is no promise of a codec (HEVC in a `.mov`,
  Vorbis in WebKit) — and gets a card saying so, with try again and, on a remote host, a
  download so it can play locally. A parked pane pauses its video (it doesn't resume by
  itself); audio keeps playing.
- **Notebooks.** `.ipynb` opens read-only in `NotebookView.svelte`: code cells highlighted
  (the editor's `--syn-*` palette, parsers from `@codemirror/language-data` via `highlight.ts`)
  beside their `[n]` execution counts; markdown cells through `marked` + DOMPurify with chat's
  profile (no `<style>`, web links in a new tab without an opener), math (`$…$`, `$$…$$`,
  `\(…\)`, `\[…\]`, `\begin{…}` blocks) typeset through the shared KaTeX policy only when a
  cell has any, `attachment:` images inlined and relative ones through `/raw` tickets, links
  followed like the markdown view's. Outputs: stdout/stderr and tracebacks in ANSI color
  (the theme's terminal palette, carriage-return progress bars collapsed to their last state);
  png/jpeg/gif as data URLs; SVG as an `<img>` (never live markup); `text/html` in an iframe
  sandboxed with **no** permissions (`sandbox=""`, `srcdoc`, the theme's colors written in),
  sized from a sanitized offscreen layout, capped at 440px with *show all* and a drag handle;
  `text/markdown` and `text/latex` rendered. The rail beside a cell's outputs folds them. The
  daemon pages cells — `GET /fs/notebook?path=&offset=&limit=` → `{cells, offset, total,
  language, nbformat}` — walking the file with serde's streaming visitor so only the page's
  cells are ever held: source ≤ 64 MB, ≤ 100 cells a page and ≤ 8 MB of payload (a page can
  come back short; the next starts at `offset + cells.length`), each output reduced to its
  richest drawable mime plus `text/plain`, a payload over 8 MB replaced by its size
  (`omitted`), text over 200 KB cut (`truncated`); one parse at a time. The first open is one
  page (up to 24 cells), then more page in while the end is within reach; a disk change
  re-reads the loaded cells in place; `#cell=N` (a reveal) loads up to the cell, scrolls to it and flashes it. nbformat 3
  is refused with how to upgrade it.
- **Logs.** `.log`, `.out`, `.err`, `.stdout`, `.stderr` (so `slurm-*.out` and `.nextflow.log`)
  open in `LogView.svelte`, read-only and tail-first: one 64 KB read (a small log arrives
  whole) then the last 256 KB, opened at the bottom. *Load earlier* pages back 256 KB at a
  time without moving the lines being read; the view keeps a 4 MB window sliding over a file
  of any size (pages fall off the far end), with *top*, *bottom* and *load later*. ANSI colors
  map to the theme's terminal palette (256-color and truecolor fold to its 16), other escapes
  are dropped, progress bars collapse, and error / warning lines are tinted, counted in the bar
  and jumped between (`logText.ts`: Python exceptions, Slurm cancellations, OOM kills; "0
  errors" stays quiet). **Follow** (on at open) polls the tail every 2 s — only while the pane
  and the window are visible — keeps the view pinned, shows a line still being written, and
  stops when the reader scrolls up (scrolling back to the bottom resumes it). With follow off,
  a disk change still appends the new lines without moving the view and shows a *new output*
  pill. Wrap is a per-browser preference; a tail that turns out binary falls back to the
  info card. A gzipped log stays in the text view (no known size to read a tail from).
- **Slides (Marp).** A markdown file whose frontmatter says `marp: true` opens in
  `SlidesView.svelte`, with a **slides | markdown** switch (the markdown side is the normal
  markdown view). FileView decides once per tab from whichever payload the markdown view
  fetches first (the reading render's frontmatter or the source chunk), so detection costs no
  request and an edit never swaps the view out from under the editor. `@marp-team/marp-core`
  renders in the browser (`html: false`, no script, math as KaTeX MathML, emoji as text — no
  CDN fetches) with relative images rewritten to `/raw` tickets first; the slides draw in
  script-less sandboxed iframes, the whole deck at native size in one frame that is moved and
  scaled to show a slide (instant paging, exact layout at any pane size, no WebKit
  foreignObject scaling bug). Arrows / PageUp / PageDown / Space / Home / End page the deck, a
  thumbnail strip jumps, **present** goes full screen (or covers the window where a webview
  refuses), **print** opens the browser's dialog with one slide per page (save as PDF there),
  `#slide=N` reveals a slide, and a disk change re-renders in place keeping the slide. Decks
  over 2 MB of source are refused. The bundle stubs MathJax and the uncommon highlight.js
  grammars (see `vite.config.ts`), so the chunk is ~730 KB (226 KB gzipped), loaded only by
  a deck.
- **Mermaid files.** `.mmd` / `.mermaid` open in `MermaidView.svelte` through the shared,
  strict renderer (`shared/mermaid.ts`): redrawn when the theme flips or the file changes (the
  last good drawing stays up, dimmed while redrawing, with the parse error in the bar), fit to
  the pane without enlarging a small diagram, zoom steps and 1:1, export as SVG or as a 2× PNG
  (a diagram with HTML labels can't rasterize in every engine; the bar says so). A
  **diagram | source** switch shows the file in the editor.
- **Release-safe lazy views.** File and other heavyweight workbench views load from immutable hashed
  chunks. The entry document is never cached and is stamped with the source build that served it,
  so a later health response cannot mistake a replacement daemon for that document's build. Vite's
  global preload signal catches nested PDF/editor/spreadsheet chunks as well as top-level pane
  surfaces. A pane keeps an in-place retry for ordinary tunnel loss; a shared notice offers reload
  when the current asset graph is unavailable. Reload waits behind unsaved file edits and chat
  drafts that exist only in memory, with an explicit reload-anyway escape hatch.
- **Binary / Finder.** Non-text files get an info card (`BinaryView`: name, size, modified time
  from the parent listing; no hex view yet) with **open as text** — a per-tab override
  (`RawTextView`): the bytes decoded read-only (the editor refuses binary content), control
  bytes drawn as their Unicode control pictures so a NUL stays visible, 256 KB at a time up
  to 4 MB, *file info* to go back — and, on a remote host (the `host=` window rule the
  downloads share), a **download** button; `FinderView` is a directory browser surface.

## Preview keep-alive & live-update

- **What & when.** A pane keeps recently-viewed rendered views alive (hidden, not destroyed) across
  a tab switch, bounded by a per-pane LRU (cap 8) that never evicts a file with unsaved edits
  (its buffer would survive in the store anyway; the view keeps scroll and search state). This includes structured chat, which retains a
  bottom-anchored DOM window (64 blocks initially, 192 maximum) rather than a whole long transcript.
  PTY components remount instead: `termPool` re-parents their xterm element into a hidden stash while
  preserving its socket and scrollback. A shared, LRU-capped content store
  (`previews/fileStore.svelte.ts`, keyed by path) additionally caches the *bytes* so re-opening a
  view the live-set evicted re-renders warm rather than re-fetching.
- **Where it lives.** `layout/Pane.svelte` (one persistent layer per tab; the live-set decides which
  hold a mounted view; parked layers are `opacity:0` + `inert`, dormant — `visibility:hidden` — after
  30 s, and each layer starts and ends with a selectable 1px image, the "selection stop");
  `previews/fileStore.svelte.ts` (`FileEntry`,
  `retain`/`release`/`noteWrite`); every `*View.svelte`. The store subscribes to
  `workspace/fsEvents.ts` (`fsEpoch`/`lastFsMutation`) + `workspace/git.ts` (`gitStatus`) +
  `workspace/diskWatch.ts`; the daemon half is `chimaera-server/src/fs_watch.rs` on `/ws/events`.
- **Large reading documents.** `previews/readingWindow.ts` keeps offscreen prose paragraphs
  in memory behind measured, same-tag placeholders once rendered text exceeds
  100,000 characters. Only blocks within 1,000 px of the reading viewport stay connected, so a
  tab's inherited `inert`/dormant flags no longer restyle every inline word in a long document.
  Heights are measured from the real layout; resizing, font changes, and disk refreshes rebuild
  the window. Selection, Cmd/Ctrl+F/G, and printing restore the full text. Links, code fences,
  other keyboard targets, tables, lists, images, math, and Svelte's HTML range boundaries
  remain connected; editors keep their own viewport handling.
- **Key behaviors.** Switching pane-tabs (or panes) to a recently-viewed file reuses its view with
  **scroll position, image decode, finder columns, and editor state preserved** — cached reading
  blocks are reattached as needed, and no route is re-hit (a view only mounts while active, so nothing is measured at
  a degenerate size). Inactive PTY tabs park in the pool's hidden stash instead of leaving invisible
  WebGL renderers attached. Inactive chat tabs freeze their bounded transcript snapshot while the
  pooled reducer/socket continues; historical artifact previews load only near the viewport, and
  initial journal hydration mounts from the newest end once instead of painting oldest-to-newest.
- **Parking is opacity + inert, one layer per tab, fenced by selection stops.** Parked layers
  never hide with `visibility:hidden`, `display:none`, `pointer-events`, `user-select` or a
  `z-index` on the shown one — every one of those inherits (WebKit re-resolves and re-shapes the
  whole parked subtree per switch) or traps a view's `position:fixed` overlay; a switch never
  inserts a sibling into the pane (positional selectors would re-resolve every parked document);
  a layer that parks releases DOM focus and selection (a caret left in an inert subtree is
  re-canonicalized through the whole parked document on every rendering commit); the selectable
  1px image at each end of a layer is where WebKit's editor-state caret walk ends instead of
  crossing every parked node (its empty alt keeps it out of copied text; parked text is inert,
  hence unselectable and never copied); and a layer parked for 30 s goes dormant
  (`visibility:hidden`, from a timer), which returns the backing stores WebKit keeps for a
  transparent scroller — the reveal of a dormant layer pays that subtree's restyle once, a quick
  switch-back never does. `inert` inherits too: a switch restyles the two switched layers' subtrees,
  never a bystander's. The terminal pool keeps xterm's `<style>` sheets out of
  the re-parented element for the same reason (see [terminals](terminals.md)). The measurements
  and the harness: [field notes](../history/field-notes.md#the-tab-switch-stall-reproduced-and-fixed-without-screen-control-2026-09-02-safari-harness),
  `scripts/perf/tab-switch/`.
  `chatPool` keeps the reducer/socket and view cursor warm if a view is eventually evicted or moved.
  Live-on-disk update is **mounted-path-scoped**, not Git-gated:
  each events
  socket registers only mounted preview files and visibly-listed tree/Finder directories. The daemon
  stats those exact paths every ~2 s and performs a capped directory-name/type hash every
  ~12 s as an NFS/Lustre metadata-cache backstop. Exact path invalidations cover repeated writes to
  an already-dirty file, ignored/non-repo files, and Finder paths outside the workspace; recognized
  agent writes and in-app mutations remain the immediate event-driven fast path. A moved mtime
  refreshes payloads **in place** (never nulling — a null chunk would unmount a live `CodeView`),
  while PDF/spreadsheet/binary surfaces remount on the new token. An editor buffer is never
  clobbered: it retains its path (so a dirty buffer stays watched with no view mounted) and
  reconciles a moved token by reading the whole file — reload when clean, merge or conflict when
  dirty (see *Raw reads & lightweight editing* above). Chat artifact cards memoize their `/raw` ticket (`rawTicketUrl`) so a cached output
  image doesn't re-fetch and re-decode (the flash) on re-render.

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

- Every listing/read runs under `spawn_blocking` behind one eight-permit semaphore — a slow Lustre
  `read_dir` must never wedge a Tokio worker or cause unbounded blocking-pool growth. Directory
  listings cap at `MAX_DIR_ENTRIES = 1000` with an honest `truncated` flag.
- Disk monitoring is per events client and hard-capped at 64 mounted files + 64 visible directories
  (64 KiB of retained path text); a closed window retains nothing. It never recursively walks a
  workspace, and its slow directory hash caps at the same 1000 entries as `fs/list`. New-directory
  baselines are limited to four per two-second poll, so registration churn cannot turn those caps
  into a continuous shared-filesystem scan.
- Previews **stream**; a preview of a huge Parquet/HTML/CSV must never balloon memory. This is a
  review criterion, not a nice-to-have (see [rules/daemon.md](../../.claude/rules/daemon.md)).
- Capability tickets expire after 10 minutes and the in-memory store is capped at 4096; expiry-first
  eviction keeps unauthenticated preview URLs bounded even under repeated minting.

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

### Why markdown previews carry reading chrome (copy + inline images)
_Captured 2026-08-27 — confirmed with the maintainer as the feature shipped._

- **Problem it solves.** Markdown is a primary working surface ("we interact a lot with
  markdown files"), and quoted prose/figures must move cleanly into external documents
  (Word). Confirmed verbatim by the maintainer.
- **Nothing pinned.** No aspect of the quote-card look or the copy affordances is
  deliberately fixed — later passes may restyle or rework them freely (an Obsidian-like
  editable reading view is already on the wish list).

### Why markdown gets Obsidian-style modes (live | reading | source)
_Intent pending — drafted from the maintainer's request, 2026-08-27; questionnaire not yet run._

- **Problem it solves (from the request).** The maintainer asked for markdown to behave
  like Obsidian: "by default a nice reading view that is editable, but also a complete
  reading mode (that replaces split) and then an edit that shows the source." Markdown is a
  primary working surface, and the old default (a read-only render with editing a toggle
  away) put friction in front of the common case — read a document, touch it up in place.
- **Pending.** The mode names (`live`/`reading`/`source`), the default-to-live choice, and
  which constructs the live view renders vs leaves as source have not been confirmed with
  the maintainer — capture via **capture-feature-intent** when available.

### Why live mode renders tables as the reading grid
_Intent pending — drafted from the maintainer's request, 2026-09-07; questionnaire not yet run._

- **Problem it solves (from the request).** "The live view for tables in the .md look quite
  weird" — live mode showed a table as monospace pipe rows, and since the editor soft-wraps,
  a wide table folded its rows and the columns fell apart. Offered a small fix (keep rows on
  one line) or a real table widget, the maintainer picked the widget ("bigger"): a table
  reads the way the reading view and chat draw it, and turns back into source the moment
  it is edited.
- **Pending.** The whole-table reveal (rather than Obsidian 1.5-style in-place cell editing),
  writing a short row's missing pipes on click, and rendering images inside cells have not
  been confirmed with the maintainer — capture via **capture-feature-intent** when available.

### Finding your place in a deep tree — why it exists
_Intent pending — drafted from the maintainer's request, 2026-09-06; questionnaire not yet run._

- **Problem it solves (from the request).** "If you have a lot of directories it can be hard
  when they retract etc. — needs just a slight UI polish." Indent guides, sticky ancestor rows,
  collapse anchoring, and collapse-all answer "which folder am I in" and stop a collapse from
  throwing the viewport onto unrelated content.
- **Pending.** The three-level sticky cap, the hover-lit parent guide, and the collapse-all
  placement beside the filter have not been confirmed with the maintainer — capture via
  **capture-feature-intent** when available.
