# Files & previews

Browsing the workspace and viewing its files. The file tree lists and opens (it doesn't
create/rename/delete); the preview service streams file bytes and renders them as code,
markdown, tables, PDFs, images, sandboxed HTML, or a hex/binary summary — plus a light
single-file editor. Everything streams (never whole-file loads) to hold the daemon's
~150 MB RSS budget on shared login nodes.

**Where it lives (shared):** UI `web-ui/src/lib/previews/` (`files.ts` loaders, `CodeView`,
`MarkdownView`, `TableView`, `PdfView`, `ImageView`, `HtmlView`, `BinaryView`, `FinderView`,
`cm.ts`) + `web-ui/src/lib/workspace/FileTree.svelte` + glyphs in `web-ui/src/lib/shared/`
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
  workspace's git **epoch** bumps (a new untracked file gets a row to carry its status badge).
  Changed files show a right-aligned letter badge (M/A/D/R/C/T/U/!) and a recolored name; a
  collapsed dir containing changes shows a rollup dot. **The tree is read-only** — create/rename/
  delete aren't here (create-folder lives in the folder picker, see [workbench.md](workbench.md)).

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
- **PDF / image / HTML.** Fetched via a short-lived **ticket**: `POST /api/v1/fs/ticket {path}` →
  `GET /raw/{ticket}` (no bearer header — iframes/`<img>`/pdf.js can't send one; ticket TTL 600s,
  range-aware). HTML is sandboxed (`CSP: sandbox allow-scripts`, no-referrer); SVG is sandboxed too.
  `PdfView`/`ImageView`/`HtmlView`.
- **Binary / Finder.** Non-text files get a hex/summary view (`BinaryView`); `FinderView` is a
  directory browser surface.

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
