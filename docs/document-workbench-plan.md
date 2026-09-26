# Documents, files & references: the plan

Dated 2026-09-25. It covers the markdown viewer, every other file viewer, embeds, file
links from agents, pointing agents at parts of files, how files show up in chat, and
safe editing over a remote link. It was built from four code maps of the tree at
commit `f44a8b7` and a survey of how Obsidian, Typora, iA Writer, VS Code, GitHub,
Notion and others do it. The [appendix](#appendix-what-the-code-does-today) describes
the code as it was **before** this work.

LaTeX and Typst reports are **out of scope here** by the maintainer's call (a
separate effort); see [Out of scope](#out-of-scope).

## Status (2026-09-26)

Built in martinappberg/chimaera#159. The feature pages
([files and previews](features/files-and-previews.md), [agents](features/agents.md),
[chat mode](features/chat-mode.md), [terminals](features/terminals.md)) describe what
shipped; the phases below remain the design record.

- **Shipped:**
  - Phase 0: buffers outlive views, the close prompt, hash-verified idempotent saves,
    three-way merge, byte fidelity, the draft journal and its daemon mirror, the
    hardened write path, and the native app asking before a window close or quit drops
    unsaved edits.
  - Phase 1: one file-reference parser, the `fs/validate` ladder, and open-at-line.
  - Phases 2 and 3: one lezer renderer behind reading and live, a parity corpus, and
    an outline. Live is now the default mode. Hover previews on links (reading on rest,
    live on Mod, Mod+K from the keyboard): a note's section through the same renderer,
    any other file as its embed card's compact body.
  - Phase 4: embed cards in documents and chat, `resolve_targets`, cacheable `/raw`,
    and HTML reports that load their own assets.
  - Phase 5: the locator grammar, pointing from every viewer, and region crops.
  - Phase 6: the MCP guide, `check_document`, the issues chip, and the opt-in
    installs.
  - Phase 7:
    - PDF (fixed blank pages, find, outline, links), and virtualized tables with
      bioinformatics presets.
    - New viewers: notebooks, logs, Marp slides, mermaid, media, Word, PowerPoint,
      diagram boards (JSON Canvas, Excalidraw, draw.io) and Parquet.
    - Publishing a markdown document, built in the browser: one self-contained HTML
      file (pictures inlined up to 50 MB, MathML, SVG diagrams, no script), print to
      PDF, and a zip bundle of the document and the files it names, laid out so its
      links still resolve. The zip is client-side (`jszip`), not a daemon route: a
      "these paths" variant of the folder download would need a new multi-path ticket,
      and the bundle's copy of the document is rewritten in the browser anyway.
- **Cut by the maintainer** (judged by what a user actually opens and uses):
  - image compare, pixel readout and PDF thumbnails;
  - table stats and sorting, and the sequence, hex, archive and JSON-tree views;
  - the reference basket, agent-opened tabs (`show_user`) and backlinks;
  - EPUB/email and the LibreOffice path.
- **Not yet built:**
  - `![](note.md#Heading)` transclusion (cards show an excerpt);
  - the "what changed" gutter;
  - server-side thumbnails (measure first).

## The short version

- **Fix data loss first.** Today an unsaved edit can vanish without a warning: closing
  its tab, opening eight other files in the same pane, or splitting the pane all
  destroy the buffer and forget it was dirty. Keys typed during a slow remote save are
  marked saved when they are not. Phase 0 fixes this before anything else.
- **One markdown engine, three views.** Live and reading look different today because
  two separate renderers draw them. The plan: one renderer draws both. Reading is the
  finished page. Live is the same page with only the paragraph you are typing in shown
  as source. Source stays plain text. Reading is the default until live matches it.
- **Links always open.** One resolver for chat, terminals, documents and tool cards.
  It understands `file.py:12:3`, `#L10-L20`, subfolders, other working directories,
  bare file names, `a/` and `b/` diff prefixes, and paths an agent creates later. It
  opens the file *at* the line.
- **Embed anything.** `![caption](results/umap.png)` already works for images. The same
  standard syntax will embed PDFs (a page), other markdown (a section), code (a line
  range), tables (a slice), HTML reports, video, notebooks and slides. The same embed
  cards show agent output in the chat.
- **Point at anything.** Select lines, a heading, a PDF passage, a box on an image, a
  block of table cells or a video moment, then send it to an agent. One address format
  (`@path#fragment`) built on existing web standards, so every LLM already reads it.
  Agents can point back: "look here" opens the file with the spot highlighted.
- **Agents learn the rules.** A short portable dialect (GitHub-flavored markdown,
  GitHub alerts, math, mermaid, standard links and embeds) renders here, on GitHub and
  in Obsidian. Agents learn it from the chimaera MCP server every session already
  loads, and a `check_document` tool lets them fix broken links before handing over.
- **More formats.** Video, audio, notebooks, JSON trees, logs, slides written in
  markdown, bioinformatics text formats, then Word and PowerPoint, then Parquet.
- **Built for remote.** One round trip to open a document, bytes only on demand, a
  browser cache that survives re-renders, and heavy rendering on the client so the
  daemon stays inside its login-node budget.

## Principles

1. **The file is the truth.** Never re-serialize or normalize a file the user did not
   change. Unified editors built on ProseMirror/Tiptap/BlockNote rewrite markdown on
   save and lose content; for Chimaera it is worse, because a normalized `*` → `_`
   breaks an agent's next exact-string edit and makes noisy diffs.
2. **One parser, one renderer.** Every surface that draws markdown draws it from the
   same syntax tree with the same code, so "live vs reading" differences cannot exist.
3. **Portable by default.** What an agent writes here must read well on GitHub and in
   Obsidian. Chimaera-only syntax is avoided; where a standard exists, use it.
4. **One address for any part of any file.** Links, embeds and references share one
   locator grammar.
5. **Never lose a keystroke.** Buffers outlive views, drafts are journaled, saves are
   verified by content, and conflicts merge instead of clobbering.
6. **Remote-first.** Each cold HTTP request over the tunnel costs about two round trips
   ([remote perf plan](perf-remote-plan.md), finding F2). Batch lookups, lazy-load
   bytes, cache by content version.
7. **The daemon stays small.** It streams bytes, slices, hashes and headers. Parsing
   and drawing happen in the browser. Conversions that need real CPU are opt-in,
   bounded and off the reactor ([daemon rules](../.claude/rules/daemon.md)).

## Phase 0: never lose an edit

The first phase because every later phase adds editing surface.

### What is broken today

| # | Problem | Where |
|---|---|---|
| 1 | An unsaved buffer lives only inside the mounted editor. Unmounting destroys it **and** calls `forgetDirty`, so the tab dot, the beforeunload prompt and the reload gate all stop protecting it. Triggers: closing the tab (no prompt), the pane keep-alive cap (`LIVE_CAP = 8`, dirty tabs not exempt), splitting a pane, zoom toggle, dragging the tab to another pane, switching workspace, renaming a parent folder from the tree. | `CodeView.svelte:319-326`, `Pane.svelte:127-149`, `App.svelte:3296` |
| 2 | Keys typed while a save is in flight are marked clean (`clearDirty()` runs unconditionally after the PUT). A later disk change then auto-reloads over them. | `CodeView.svelte:379-408` |
| 3 | Auto-reload adopts the disk version token *before* checking whether a key landed, so the next Cmd+S overwrites the external change silently. | `CodeView.svelte:422-460` |
| 4 | CRLF and lone-CR line endings are rewritten to LF; a CRLF split across the 256 KB load chunks adds a blank line. | `CodeView.svelte:330-364` |
| 5 | Non-UTF-8 text (Latin-1, CP1252) opens editable and is written back with U+FFFD; a UTF-8 BOM is dropped; the decoder is never flushed. | `CodeView.svelte:156,259` |
| 6 | Small `.gz` text files are editable, and saving writes plain text over the gzip. | `files.ts:603` |
| 7 | Native app quit and window close never check for unsaved edits. | `chimaera-app/src/shell.rs:487-498` |
| 8 | After a remote daemon restart or a tunnel port change, saves go to the dead origin; the only exits are copy by hand or "reload anyway". | `App.svelte:466-501` |
| 9 | The conflict check is metadata only (`stat`, which NFS may cache for 3 to 60 s), has a check-then-rename gap, and the token uses `DefaultHasher`, whose output may change across Rust releases. | `fs.rs:182-201,686-753` |
| 10 | Temp-and-rename changes owner and group (bad on shared project dirs), drops ACLs, breaks hardlinks, skips fsync, and leaves a partial temp file on a full disk. | `fs.rs:686-753` |
| 11 | No timeout, retry or idempotency on save; a lost 204 turns a retry into a 409 against our own write. | `files.ts:119-146` |

### The fix

- **A buffer store that outlives views** (`previews/buffers.svelte.ts`). The
  `EditorState` for a dirty file is owned by the store, keyed by path, not by the
  `CodeView` instance. A remounting view re-attaches to the same state, so undo
  history, cursor and dirty flag survive every layout change. The pane keep-alive
  cap never evicts a dirty tab.
- **Explicit close.** Closing a dirty tab asks Save / Don't save / Cancel. The native
  shell handles `CloseRequested` and quit by asking the UI first.
- **Draft journal.** Dirty text is journaled about 1 s after typing stops: to
  IndexedDB (fast, works with the tunnel down) and mirrored to the daemon under
  `~/.chimaera/drafts/` (size-capped JSONL) whenever connected. The daemon copy
  matters because a new tunnel port is a new browser origin with an empty IndexedDB.
  On reopen: a "recovered unsaved changes" bar, never a silent restore. A journal
  failure is shown, never hidden.
- **Content-hash versions.** The daemon returns `X-Content-Hash` (SHA-256; `sha2` is
  already in the tree, and hashing a 1 MB file costs about a millisecond). `PUT` sends
  `expect_hash`. The daemon opens and hashes the current file right before writing
  (`open` forces fresh NFS attributes, unlike `stat`). If the disk already holds
  exactly our bytes, the PUT returns success: a retry after a lost reply is a no-op.
  The mtime token stays as a cheap first check and moves off `DefaultHasher`.
- **Save generations.** A save records the document version it sent; on success only
  that version is marked clean. Keys typed during the save stay dirty. Saves get a
  timeout, one automatic retry, and a visible "not saved, reconnecting" state.
- **Three-way merge instead of clobbering.** The store keeps the `base` text each
  buffer was loaded from. When the disk changes under a dirty buffer (usually an agent
  editing the same document), merge `base`/`mine`/`disk` line-wise (diff3). A clean
  merge applies in place with a quiet "merged changes from disk, view diff" notice and
  never moves the cursor. Overlaps open the existing `@codemirror/merge` view with
  Keep mine / Take disk. "Overwrite" stays, behind the diff.
- **Byte fidelity.** Detect and keep line endings (`EditorState.lineSeparator`), BOM
  and final newline. Load files up to the 1 MB edit cap in one request (the read cap is
  already 2 MB), which removes the chunk-seam bug. Text that is not valid UTF-8 opens
  read-only with a clear note. Compressed files are always read-only.
- **Write path.** Create the temp file with the target's mode, `fsync` it, rename,
  `fsync` the directory. Keep owner and group where the user may (`fchown` to the
  original ids). If `nlink > 1`, write in place under the hash check instead of
  renaming. Clean up the temp file on every error path.
- **Multi-window.** Windows on the same origin announce open dirty buffers over a
  `BroadcastChannel`, so a second window shows "being edited in another window".
  Cross-origin windows rely on the hash check and the merge above.
- **Agent writes arrive fast.** Claude hooks and chat edit events already call
  `mark_path_dirty`; also push the path on `/ws/events` so an open document refreshes
  (or merges) immediately instead of on the 2 s poll, labelled with who wrote it.
- **Autosave (setting).** Agents only see what is on disk, so an unsaved paragraph is
  invisible to them. Once the merge ships, offer autosave after about 1 s idle, per
  file kind. Default is an [open decision](#open-decisions).

**Verification.** Rust tests for the hash check, idempotent retry, symlink, hardlink,
mode/owner and temp cleanup. Vitest for the merge, the buffer store and encoding
round-trips (CRLF, BOM, final newline). A live chaos script under `scripts/verify/`
that kills the tunnel mid-save, restarts the daemon with a dirty buffer, and has a
fake agent write the same file during typing; it passes only if no byte is lost.

## Phase 1: links that always open

### What is broken today

The same path works in one surface and not the next. The worst cases:

- **In documents, links to other files do nothing.** Reading mode swallows every
  relative link (`MarkdownView.svelte:403-427`); live mode follows only web URLs
  (`mdLive.ts:1245-1268`).
- **Line suffixes never work in chat.** `src/x.rs:42`, `a/b.py:12:3` and `#L10-L20`
  are validated literally and miss, although Claude and Codex are both told to write
  file references that way. The terminal strips `:42` but then drops it, and nothing
  in the UI can open a file at a line (`FileTab` has no line field, `layout.ts:26`).
- **Only one base directory.** Chat resolves against the session's spawn directory
  only; an agent that `cd`'d, or a chat started in a subfolder, misses. Terminal
  scrollback resolves against the shell's *current* directory, not the one in effect
  when the line was printed.
- **Partial paths and bare names.** `figs/plot.png` for `results/figs/plot.png` has no
  fallback. A bare `plot.png` misses when ambiguous, inside an ignored folder
  (`work/`, `target/`), behind a symlink, or with a long extension.
- **Misses are permanent in chat.** A path mentioned before the agent creates it stays
  dead for the life of the message, and a transient daemon error becomes a permanent
  miss. A settle race can drop links entirely.
- **Punctuation, spaces, Unicode.** `(results/plot.png)`, `"x.py"`, `résumé.pdf`,
  `Screenshot (1).png`, `a/` and `b/` diff prefixes, `@src/x.ts`, `file://` URLs and
  paths the TUI hard-wrapped across lines all miss somewhere.
- **Tool cards.** Only the first location opens, a Grep/Glob directory opens as a
  file tab, and relative paths break both the button and the artifact tile.

The full list, with lines, is in the [appendix](#link-resolution-failures).

### The fix

- **One parser for file references**, shared by chat, terminal, documents and tool
  cards (`shared/fileRef.ts`). It returns `{path, line?, col?, endLine?, fragment?}`
  and handles `:12`, `:12:3`, `#L12`, `#L12-L20`, `a/`/`b/` prefixes, a leading `@`,
  `file://`, percent-encoding, wrapping quotes/brackets/backticks on either side
  (including full-width punctuation), `…/` abbreviations, Unicode, and spaces when the
  path is delimited (a link target or a code span).
- **One resolver on the daemon**: `POST /api/v1/fs/resolve`, additive next to
  `fs/validate` (which stays for older clients). Each candidate carries an ordered list
  of base directories: the cwd when the text was written, the session's current cwd,
  the document's folder, the workspace root, and linked worktree roots. The ladder:
  exact → each base → without a diff prefix → unique basename → unique path suffix →
  up to five matches for a picker → miss. Runs under the shared filesystem semaphore
  with a timeout.
- **Remember where the agent was.** Claude hook payloads and Codex items carry a cwd;
  store it on each message and tool call so links resolve against the directory the
  agent was in when it wrote them.
- **Misses expire.** Misses are retried after a short TTL and always on click; a path
  becomes a link as soon as the file exists. Fix the chat settle race.
- **Open at the spot.** Tabs gain a transient `reveal` locator (not persisted): the
  editor scrolls to and flashes the lines, a PDF jumps to the page, an image outlines
  the region. Cmd/Ctrl-click opens in a split everywhere, chat included.
- **Tool cards** list every location; directories open in the Finder.
- **Tests.** A shared fixture, `fileRefs.fixture.json`, of real agent output (Claude,
  Codex and Gemini styles) with the expected parse, run by both Vitest and the Rust
  suite, like `mathBlocks.fixture.json` today.

## Phase 2 and 3: one markdown engine, three views

### Why live and reading differ today

Live is CodeMirror decorating the source line by line (`mdLive.ts`). Reading is
comrak on the server, cleaned by ammonia, typeset on the client
(`fs.rs:1107-1131`). Two renderers means two sets of bugs. Visible today:

| Construct | Live | Reading |
|---|---|---|
| Relative file links | ignored unless web URL | swallowed |
| YAML frontmatter | muted source | becomes a rule plus a **heading** |
| Task checkboxes | real, clickable | **removed** by the sanitizer |
| Callouts `> [!NOTE]` | plain quote, marker visible | plain quote, marker visible |
| Footnotes | source | rendered, but the jump links are dead (ids stripped) |
| Heading anchors / outline | none | none (no ids) |
| Code fences | syntax highlighted | not highlighted |
| Paragraphs | ragged, one line per source line | flowing |
| Nested quotes | flattened | nested |
| Reference links | source | resolved |
| Mermaid | plain fence | plain fence |

The mode is also not remembered (every open starts in live), and switching modes
does not keep your place.

### The design

Obsidian has the same split and users have complained about it for years. Instead:

- **One parser: lezer.** Live already uses it; it is incremental and gives exact
  source positions.
- **One block renderer** (`previews/doc/`): syntax tree → DOM, built with
  `createElement`/`textContent` only, as `mdTable.ts` already does for tables. There
  is no HTML-string injection anywhere. Raw HTML blocks go through DOMPurify with a
  strict allowlist, the policy chat already uses. Every block element carries its
  source range (`data-from`/`data-to`).
- **Reading** is that renderer's output as a normal page: native selection, find,
  print, and the existing `readingWindow` for very long documents. Because every block
  knows its source lines, a selection in reading now references exact lines.
- **Live** is the same editor as today, but every block *not* touching the cursor is
  replaced by the renderer's block (a CodeMirror block widget from a state field, with
  cached measured heights). Only the block you are typing in shows as source, with
  the inline styling live has now. This is Typora's model: live looks exactly like
  reading, except where you type.
- **Source** stays the plain editor.
- **Switching keeps your place**: the top visible block's source offset maps between
  views.
- **Modes are remembered** per file, and a setting picks the default: **reading for
  now** (the maintainer's call); revisit switching the default to live once Phase 3
  parity is proven.
- **comrak stays** in three roles: the reference renderer in a shared parity corpus
  (every construct, both sides, like the math fixture); the fallback reader for files
  too large to parse in the browser (it gains `sourcepos`, frontmatter, alerts and
  heading ids so the fallback matches); and export (Phase 7).

### Constructs to add (both views, one code path)

- **Properties panel** for YAML frontmatter: a compact, typed, collapsible header
  (text, list, date, checkbox, link), like Obsidian Properties, editable in live.
- **Callouts**: the five GitHub alerts plus Obsidian's aliases and `+`/`-` folding.
- **Headings** get GitHub-style anchors; an **outline** panel follows the scroll.
- **Footnotes** with hover preview and working jumps.
- **Syntax highlighting** in reading, via the same lezer highlighter live uses, so
  colors match.
- **Mermaid** diagrams (lazy chunk, strict security level).
- **Task checkboxes** that edit the source in both views.
- **Wikilinks** `[[note]]` and `![[embed]]`: read support for Obsidian vaults; agents
  never write them, and a quick action converts one to a standard link.
- **Search**: `@codemirror/search` in live and source (today Cmd+F only sees the
  rendered viewport).
- **Hover preview**: Mod-hover a link to see the target (a section, a page, an image)
  in a popover, using the embed renderer.
- **Backlinks** ("linked from"), computed on demand by a bounded daemon search, never a
  persistent index (login-node rule).
- **What changed**: a gutter mark on blocks changed since you last looked (against a
  last-seen hash, or git HEAD), so an agent's edits to a long document are easy to
  spot.

### Risks

CodeMirror block widgets can make scrolling jump when estimated heights are wrong,
and selection across widgets and IME input need care. Mitigations: Phase 2 ships the
new reading view first (no editor risk), Phase 3 caches measured heights and keeps
source mode as the escape hatch, and the live preview is driven by hand on WebKit and
Chromium before each step ships.

## Phase 4: embeds, in documents and in chat

### Syntax

Standard markdown first. `![caption](path.ext#fragment)` embeds by file kind, and
GitHub still shows images and meaningful alt text. Size hints use Obsidian's form,
`![caption|400](plot.png)`. `![[file#fragment|400]]` is read for compatibility.

| Target | Renders as |
|---|---|
| image, `#xywh=` | image, optionally cropped to the region |
| `.pdf#page=3` | that page, at column width, with open/expand |
| `.md#Heading` | the section, transcluded (depth 3, cycle-safe) |
| code `#L10-L30` | a highlighted read-only excerpt with line numbers |
| `.csv/.tsv#row=1-20`, `.xlsx#sheet=S&range=A1:F20` | a table slice |
| `.html` | the report in a sandboxed frame, fixed height, expand |
| `.mp4/.webm/.mp3/.wav#t=30,45` | native player, starting at the moment |
| `.ipynb#cell=7`, `.pptx#slide=3`, Marp `#slide=3` | that cell or slide |
| anything else | a file card: icon, size, open, download (remote) |

### How it behaves

- **One card design** everywhere: a thin header (icon, name, fragment, open in pane,
  reference in agent, download on remote) above the file's own viewer in a compact
  mode. Missing files show a clear card with "find" and "create" actions.
- **One round trip per document.** `POST /api/v1/doc/resolve` takes every link and
  embed target in the document and returns, for each: resolved path, kind, size,
  version, image dimensions (from the header bytes), and a ticket. Placeholders get the
  right aspect ratio up front, so nothing jumps as bytes arrive. Bytes load only near
  the viewport.
- **Cache that works.** `/raw` today sends no `ETag` or `Cache-Control`, and a new
  ticket is a new URL, so every re-render re-downloads. Add `ETag` plus
  `If-None-Match` → 304, a private max-age, and tickets that stay stable for a given
  file version within their lifetime.
- **Relative assets for HTML reports.** Today a report's `app.js` or `figs/a.png` 404s
  or loads the app shell (the route is single-segment `/raw/{ticket}`). A
  directory-scoped route, `/raw/{ticket}/{*path}`, confined to the ticket's folder
  with no-follow opens (as downloads already do), fixes it while the frame stays
  sandboxed and never same-origin.
- **Live on disk.** A document registers only its *visible* embeds with the disk
  watcher (inside the 64-path cap), so a regenerated `plot.png` updates in place.
- **Big images over slow links.** Measure first; if needed, a bounded server-side
  thumbnail (`?w=`) with a pixel cap and a small cache in the runtime directory.

### Files in chat

The same cards replace today's thin artifact tiles:

- `![](results/plot.png)` in agent prose renders (it is a broken image today).
- The turn's "made this turn" gallery also catches files written by shell commands
  (a plot saved by a script), HTML reports, documents, spreadsheets and slides, not
  only images, tables and PDFs touched by edit tools. Aborted turns keep theirs.
- Tiles show errors instead of blank boxes, stay fresh when a file is overwritten, and
  a regenerated plot offers "compare with previous".
- File paths in agent prose use the Phase 1 resolver and open at the line.

## Phase 5: point at anything

### One address format

Links, embeds and references share one grammar: `path#fragment`, using existing
standards wherever one exists, so any LLM can read and write it.

| Kind | Fragment | Standard |
|---|---|---|
| text, code, markdown | `#L12-L20` | GitHub |
| markdown heading | `#heading-slug` | GitHub |
| PDF page | `#page=4` | PDF open parameters (RFC 8118) |
| image or PDF region | `#xywh=160,120,320,240`, `#page=4&xywh=…` | W3C Media Fragments |
| video, audio | `#t=12.5,20` | W3C Media Fragments |
| CSV, TSV | `#row=5-9`, `#col=2`, `#cell=5,2-9,4` | RFC 7111 |
| spreadsheet | `#sheet=Summary&range=B2:F9` | A1 notation |
| JSON, YAML | `#/path/to/key` | JSON Pointer (RFC 6901) |
| notebook, slides | `#cell=7`, `#slide=3` | Chimaera convention |

### Selecting

Every viewer publishes its selection to the existing reference bridge
(`shared/reference.ts`) and shows the existing "reference in agent" chip:

- **Markdown reading**: exact source lines plus the enclosing heading (today it sends
  no lines).
- **Images and PDFs**: drag a box with the region tool; PDF text selection sends the
  page and the quote.
- **Tables**: the selected cell range plus the values as a small TSV (capped).
- **Video and audio**: the current moment, or a marked range.
- **HTML reports**: the selected text as a quote (the sandbox allows no more).
- **A reference basket**: collect several spots across files, then send them together.

### Delivering to any agent

- **Text** is typed as today: `@path#fragment "quote"`, never with a newline, so it
  never auto-submits.
- **Pixels** (an image or PDF region): chat mode attaches the crop as an image through
  the existing attachment pipeline. Terminal agents get the crop written to a bounded
  per-session scratch file (runtime directory, count and size capped, removed with the
  session) and its path added to the reference.
- **MCP `read_reference(locator)`** returns the exact content: text, a TSV slice, or a
  PNG crop as MCP image content. It works for any MCP-capable agent.
- **Agents point back.** MCP `show_user(locator, note?)` opens the file in a
  background tab with the spot highlighted and an attention dot. It never steals focus
  and is rate-limited. Whether agents may do this unasked is an
  [open decision](#open-decisions).

## Phase 6: agent-first documents

### The portable dialect

What agents are taught to write. Everything renders on GitHub and in Obsidian:

- CommonMark + GFM: tables, task lists, footnotes, strikethrough, autolinks.
- Alerts: `> [!NOTE]`, `TIP`, `IMPORTANT`, `WARNING`, `CAUTION`.
- Math: `$…$`, `$$…$$`, `` ```math ``. Diagrams: `` ```mermaid ``.
- YAML frontmatter: `title`, `summary`, `status`, `audience` (`public` or
  `internal`), `updated`, `tags`.
- Links: `[text](relative/path.md#heading)` with `%20` for spaces. Never absolute
  local paths in a public document.
- Embeds: `![meaningful alt text](relative/path.ext#fragment)`.
- Avoid: wikilinks, MDX, Markdoc tags, Pandoc/Quarto `:::` divs, MyST directives,
  HackMD containers (they show as literal text on GitHub).

### Teaching agents, across LLMs

- **MCP instructions.** Every Claude and Codex session already loads the chimaera MCP
  server (`mcp.rs`). Add a short documents paragraph to its `instructions`, plus a
  `document_guide` tool that returns the full guide with examples. This is the one
  channel that reaches every agent with no repository change.
- **`check_document(path)`**: the same checker the UI uses. It reports broken links
  and embeds, unsupported syntax, missing alt text, absolute local paths, oversized
  images, and dangling heading anchors, each with a suggested fix. Agents run it
  before handing a document over; the UI shows the same findings as a small "3 issues"
  chip.
- **Opt-in, never automatic:** a button that adds a documents section to a
  repository's `AGENTS.md`, and an installable skill (the `SKILL.md` format Claude
  Code and Codex both read), for agents launched outside Chimaera.

## Phase 7: viewers, formats and publishing

### Fix and upgrade what exists

- **Images**: compare against git HEAD or the previous version (side by side, swipe,
  onion skin, difference). Agents regenerate plots constantly, and "what changed in
  this figure" has no answer today. Also a pixel readout, the region tool, and routing
  BMP, ICO and AVIF to the viewer (they go to the binary card today).
- **PDF**: page indicator and jump-to-page, outline sidebar, find with highlights,
  clickable links (annotation layer), thumbnails, `#page=` deep links, and less
  prefetching on slow links.
- **Tables**: sort, filter and search; column quick stats (type, missing, distinct,
  min/max, a tiny histogram); header-less mode and `#` comment skipping (VCF, BED,
  GFF); row virtualization (today every loaded row stays in the DOM); jump to row;
  expand a cell. On the daemon, a sparse row-offset index so deep pages stop
  re-scanning from byte 0.
- **Code and text**: search, folding, active line, go to line; logs get ANSI colors and
  a follow mode (for `slurm-*.out`).
- **Binary**: the real hex view the docs already promise, "open as text anyway", and
  download.
- **HTML**: the relative-asset fix above, working `target=_blank` links, reload, and
  open in the system browser.

### New formats, in order of value to agents and public documents

1. **Cheap, high value**: video and audio (native players; `/raw` already serves
   ranges), JSON and JSONL tree with JSON Pointer references, notebooks (`.ipynb`: the
   daemon pages cells; outputs as tickets; HTML outputs sandboxed; ANSI text), slides
   written in markdown (Marp: `marp: true` frontmatter, `---` between slides; the
   easiest deck format for any LLM to write correctly), mermaid files, bioinformatics
   text formats (VCF, BED, GFF, SAM as tables; FASTA and FASTQ as a sequence view with
   stats), and zip/tar listings.
2. **Office**: Word (`docx-preview`) and PowerPoint (`@aiden0z/pptx-renderer`: lazy
   slides, tested against python-pptx output), both Apache-2.0 and loaded only when
   needed. When LibreOffice is on the host, an optional high-fidelity "view as PDF"
   (one conversion at a time, niced, time-limited, cached by content hash). EPUB and
   email later.
3. **Data and diagrams**: Parquet and Arrow via `hyparquet` over ranged `/raw` reads
   (only the needed column chunks cross the tunnel; no daemon memory), JSON Canvas,
   draw.io and Excalidraw (read-only, lazy), and TIFF stacks for microscopy.

Every new format: a lazy chunk, a size cap, streaming or ranged reads where possible,
and no parsing on the async reactor.

### Publishing

For public-facing output: export a markdown document as one self-contained HTML file
(embeds inlined, math pre-rendered, print styles), as PDF (print of the reading view),
or as a zip bundle (the document plus every referenced file, with links rewritten) for
GitHub, Obsidian or email. Marp decks export to HTML and PDF.

## Quick wins

Small, independent fixes that can land any time, before their phase:

- Reading mode opens relative file links in a pane (today swallowed).
- comrak: frontmatter delimiter (stops the fake heading), alerts, heading ids.
- Allow the task checkbox through the sanitizer (disabled, read-only).
- A "default markdown mode" setting, defaulting to reading, and remember the mode per
  file.
- Route video, audio, BMP, ICO and AVIF to real viewers.
- PDF page indicator; `@codemirror/search` in the editor.
- Stop line suffixes and `#L` anchors from breaking chat links.

## Remote and performance budget

- Opening a document: one request for content, one for resolve, then bytes lazily.
- `/raw` and file reads revalidate with `ETag` (304) instead of re-downloading.
- No new resident daemon caches over a few MB; conversions are subprocesses with caps.
- Disk watching registers only what is visible, inside the existing 64+64 caps.
- New scenarios in `scripts/perf/`: open a 1 MB document with 30 embeds over a
  high-latency tunnel; scroll it; switch modes; type in live mode with a 5,000-line
  document.

## Order and effort

| Phase | What | Size |
|---|---|---|
| 0 | Never lose an edit | medium |
| 1 | Links that always open, open at line | medium |
| 2 | One renderer: new reading view, parity corpus, new constructs | large |
| 3 | Live = reading with one block revealed | large |
| 4 | Embeds in documents and chat | large |
| 5 | Point at anything, MCP reference tools | medium |
| 6 | Dialect, MCP guide, `check_document` | small |
| 7 | Viewer upgrades, formats, publishing | large, in slices |

Phase 0 and Phase 1 are independent and can run in parallel worktrees. Phase 2 must
precede 3. Phase 4 builds on 1 (resolver) and 2 (renderer). Quick wins can land any
time. Each phase ships behind the usual gates (`just check`, web-UI check, test and
build) and is driven live before merge, per the
[verify-app](../.claude/skills/verify-app/SKILL.md) skill.

## Open decisions

1. **Default markdown mode.** Reading now (decided); flip to live after Phase 3?
2. **Autosave.** Recommend on by default for markdown once the merge ships, because
   agents only see saved text. Or keep it opt-in?
3. **`AGENTS.md` section.** Is a one-click, opt-in button acceptable, or only the MCP
   guide and a skill?
4. **Office fidelity.** Client renderers only, or also use LibreOffice when present?
5. **Agents pointing back.** May an agent open a background tab with `show_user`
   unasked, or only when the user asked "show me"?

## Out of scope

- **LaTeX and Typst reports** (compile on save, PDF beside the source, SyncTeX jumps,
  errors as editor marks): a separate effort by the maintainer's call. The locator
  grammar and embed cards here are designed so a compiled PDF slots in unchanged.
- A real IDE editor (LSP, completion, multi-file refactor) stays a non-goal
  ([DESIGN.md](../DESIGN.md#scope-philosophy-and-non-goals)).

## Appendix: what the code does today

### Markdown today

- `MarkdownView.svelte` hosts live and source (one `CodeView`, extensions swapped in a
  compartment, never remounted) and reading (`GET /api/v1/fs/markdown` → comrak →
  ammonia → `{@html}` → client KaTeX). Mode is component state reset to live on every
  mount (`:119-130`); files over 1 MB go straight to reading.
- comrak options (`fs.rs:1107-1119`): strikethrough, table, autolink, tasklist,
  footnotes, `math_dollars`, `unsafe`; no frontmatter, alerts, heading ids,
  `sourcepos`, or syntax highlighting (`default-features = false`).
- ammonia defaults plus `data-math-style` (`fs.rs:1121-1131`): no `input`, `section`,
  `id`, `class` or `data:` URLs, which is why checkboxes vanish and footnote jumps
  die.
- Live decorations walk only the visible ranges (`mdLive.ts:628-982`); tables and `$$`
  blocks come from a state field (`:1191-1215`).
- The reading selection publishes no line numbers (`MarkdownView.svelte:437-460`).
- Tests: `mdMath.test.ts`, `mdTable.test.ts`, the shared `mathBlocks.fixture.json`
  (36 cases, 7 known divergences), and Rust `markdown_tests` in `fs.rs`. Nothing
  covers live decorations, links, modes, task lists, footnotes or frontmatter.

### Link resolution failures

- Chat: `chat/paths.ts` pre-filters (no whitespace, 200-char cap, ASCII-only bare
  names, trailing punctuation only); `Markdown.svelte` caches hits and misses forever
  per mount (`:69-70`); `ChatView.svelte:1052-1065` resolves against the spawn cwd
  only and turns errors into misses.
- Terminal: `terminal/links.ts` token regex is ASCII and splits on `:`, `(`, `[`,
  quotes and spaces; tries the current cwd then the workspace root; a 15 s cache;
  parses `:42` and drops it on click (`docs/features/terminals.md` said otherwise
  until this change).
- Daemon: `fs::validate` (`fs.rs:1667-1748`) takes one base, joins and canonicalizes;
  a unique-basename fallback uses the quick-open index (`quickopen.rs`), which skips
  ignored folders and symlinks, and returns nothing while cold. No suffix matching.
- Tool cards: only `locations[0]` opens, unresolved (`ToolCallCard.svelte:157-176`);
  Claude Grep/Glob `path` may be a relative directory
  (`chimaera-agent/src/claude.rs:4642-4660`).
- Chat artifacts: collected at turn end from edit-tool and image locations only, at
  most 8, rendered by `ArtifactGallery.svelte` / `InlinePreview.svelte` (image, 5-row
  table, PDF via `<object>`); no error state.

### Viewers today

- Dispatch by extension in `files.ts:597-613`; eight kinds. Video, audio, office,
  Parquet, BMP, TIFF and archives go to an info card. Notebooks open as raw JSON.
- Image: fit, 1:1, cursor-anchored zoom, pan, pixel grid, checkerboard; no compare or
  metadata.
- PDF (pdf.js 6.1.200): lazy pages, raster and text-layer caps, zoom; no find, outline,
  thumbnails, page indicator or clickable links. Chat uses the browser's `<object>`.
- Tables: sticky header, paging, numeric alignment, resize, selection, copy; no sort,
  filter, search, stats or virtualization; deep pages rescan from the start.
- HTML: sandboxed preview/split/edit; relative assets do not load.
- Binary: an info card only, no hex view.
- `/raw`: single byte ranges, no `ETag`/`Last-Modified`/`Cache-Control`.

### Sources

- Obsidian drift between Live Preview and Reading: forum threads on
  [task lists](https://forum.obsidian.md/t/live-preview-and-reading-mode-parse-todo-lists-very-differently/60791),
  [nested blocks](https://forum.obsidian.md/t/live-preview-and-reading-view-render-nested-block-elements-inside-quotes-differently/73772),
  [general](https://forum.obsidian.md/t/live-preview-and-reading-mode-are-very-different/87552).
- Obsidian [embeds](https://github.com/obsidianmd/obsidian-help/blob/master/en/Linking%20notes%20and%20files/Embed%20files.md),
  [callouts](https://github.com/obsidianmd/obsidian-help/blob/master/en/Editing%20and%20formatting/Callouts.md),
  [JSON Canvas](https://github.com/obsidianmd/jsoncanvas/blob/main/spec/1.0.md).
- iA Writer [content blocks](https://github.com/iainc/Markdown-Content-Blocks).
- Round-trip loss in tree editors: [Tiptap #7147](https://github.com/ueberdosis/tiptap/issues/7147),
  [BlockNote lossy export](https://www.blocknotejs.org/docs/features/export/markdown).
- GitHub [alerts](https://github.com/orgs/community/discussions/16925),
  [math](https://docs.github.com/en/get-started/writing-on-github/working-with-advanced-formatting/writing-mathematical-expressions),
  [image view modes](https://github.blog/news-insights/behold-image-view-modes/).
- VS Code [same-length overwrite bug #119002](https://github.com/microsoft/vscode/issues/119002),
  [hot exit](https://code.visualstudio.com/blogs/2016/11/30/hot-exit-in-insiders).
- NFS attribute caching: [nfs(5)](https://www.man7.org/linux/man-pages/man5/nfs.5.html).
- Three-way merge: [node-diff3](https://github.com/bhousel/node-diff3).
- Renderers: [docx-preview](https://www.npmjs.com/package/docx-preview),
  [pptx-renderer](https://github.com/aiden0z/pptx-renderer),
  [hyparquet](https://github.com/hyparam/hyparquet),
  [Marp directives](https://marpit.marp.app/directives).
- What agents produce: Anthropic's [document skills](https://github.com/anthropics/skills).
- Standards: [W3C Media Fragments](https://www.w3.org/TR/media-frags/),
  [RFC 7111](https://www.rfc-editor.org/rfc/rfc7111),
  [RFC 8118](https://www.rfc-editor.org/rfc/rfc8118),
  [RFC 6901](https://www.rfc-editor.org/rfc/rfc6901).
