# web-ui/src/lib/previews — file views and the editor

Orientation for coding agents. This area owns every file surface: the extension dispatcher, the editor and its buffers, the markdown reading and live views, tables, notebooks, PDF, Office, media, boards, and the Finder. It also owns the file service client (`files.ts`) and the file-content cache. It does not own the files tree or git (`workspace/`), embed cards (`shared/embed/`), or the math policy (`shared/math.ts`). Parent map: repo-root [AGENTS.md](../../../../AGENTS.md). Rules: [.claude/rules/web-ui.md](../../../../.claude/rules/web-ui.md). Feature page: [files-and-previews](../../../../docs/features/files-and-previews.md).

## File map

**Entry points.** `FileView.svelte` dispatches a file tab to its view by extension. It sniffs the first 256KB and sends binary content to `BinaryView`. `FinderView.svelte` (Miller columns; `FinderPreview.svelte` is its side preview) and `DiffView.svelte` (git side-by-side, `@codemirror/merge`) are separate pane surfaces.

| Family | Files |
|---|---|
| Data and cache | `files.ts` (client of `/fs/*`: list, file, markdown, table, ticket; extension → view kind), `fileStore.svelte.ts` (one `FileEntry` per open path, LRU `CACHE_CAP = 32`, first 256KB chunk), `rawBytes.ts` (whole-file `/raw` reads with a per-format cap), `paneVisible.ts` (`watchPaneVisible`: is this pane layer on screen) |
| View lifecycle | `viewLifecycle.svelte.ts`: `useFileEntry` (the store claim with FileView's stale-entry guard), `viewSignal` (one `AbortController` per load generation, aborted on renew and unmount), `onDiskVersion` (the mtime reload loop; `whileHidden: "defer"` applies a parked change in the reveal's own update), `paneVisibility` (the `visible` prop, else the layer's `inert`, with `$pageVisible`), `whenRevealed`, `ownedObjectUrls` (over `blobUrls.ts`'s `OwnedUrls`). Plain cores (`ViewSignal`, `FileClaim`, `DiskVersionLoop`, `RevealQueue`) carry the node tests. |
| Editor | `CodeView.svelte` (CodeMirror view onto a buffer), `buffers.svelte.ts` (`Buffer` per path: `EditorState`, base text, hash, save, merge, `presence`), `drafts.ts` (unsaved-edit journal: IndexedDB + `/fs/drafts`), `merge.ts` (line-wise three-way merge), `textCodec.ts` (byte-faithful line endings and BOM), `CompareView.svelte`, `SplitEditPreview.svelte`, `cm.ts` (shared CodeMirror theme), `languages.ts`, `highlight.ts` (static highlight for read-only code) |
| Markdown | `MarkdownView.svelte` (reading / live / source modes), `mdLive.ts` + `mdBlocks.ts` (live mode), `mdMath.ts`, `mdTable.ts`, `mdDoc.ts` (pure helpers, mode memory), `docLinks.ts` (follow a link out of a document), `mathLoad.ts` (on-demand `shared/math`), `readingWindow.ts` (keeps only near-viewport blocks in the DOM), `DocIssues.svelte`, `PublishButton.svelte`; `doc/` holds the shared model: `parser.ts`, `model.ts`, `render.ts`, `reader.ts`, `live.ts`, `slice.ts`, `callouts.ts`, `embeds.ts`, `entities.ts`, hover (`hover.ts`, `hoverController.svelte.ts`, `HoverPreview.svelte`), publish (`publish.ts`, `publishRun.ts`, `publishCss.ts`) |
| Slides | `SlidesView.svelte` + `marp.ts` (Marp decks), `PptxView.svelte` + `pptxDeck.ts`; `stubs/hljs-language.cjs` and `stubs/mathjax.cjs` replace Marp's highlight.js grammars and MathJax at build time (`vite.config.ts`) |
| Tables | `TableView.svelte` + `tableGrid.ts` (paged, virtualized grid), `XlsxView.svelte`, `ParquetView.svelte` + `parquet.ts` + `parquetGrid.ts` + `codecs.d.ts` |
| Documents | `PdfView.svelte` + `pdfFind.ts`, `DocxView.svelte` + `blobUrls.ts`, `officeSafety.ts` (Office sanitizing), `HtmlView.svelte` (sandboxed iframe + edit), `NotebookView.svelte` + `NotebookHtml.svelte` + `notebook.ts` |
| Media and diagrams | `ImageView.svelte` + `imageRegion.ts`, `MediaView.svelte`, `MermaidView.svelte`, `BoardView.svelte` with `boards/` (`format.ts`, `canvas.ts` + `CanvasBoard.svelte`, `excalidraw.ts`, `drawio.ts` + `drawioSvg.ts` + `xml.ts`) |
| Text and other | `LogView.svelte` + `logText.ts` + `ansi.ts`, `RawTextView.svelte` (open binary as text), `BinaryView.svelte`, `Spinner.svelte` (delayed loading spinner) |

Tests: `*.test.ts` beside the modules; `mathBlocks.fixture.json` and `doc/parity.fixture.json` are test fixtures.

## State owners

| State | Owner | Who mutates |
|---|---|---|
| Editor text, undo, dirty, conflict, save state | `buffers.svelte.ts` (`Buffer`) | The buffer's methods. Views attach and detach; they do not own the text. |
| Dirty-path set (`dirtyFiles`) | `shared/editing.ts` | Mirrored by the buffer store. |
| Last-known-on-disk content | `fileStore.svelte.ts` (`FileEntry`) | `retain` / `release` / `noteWrite` and the store's own disk-change handling. |
| `presence` (which other same-origin windows hold unsaved edits to a path) | `buffers.svelte.ts` | `Presence` methods. |
| Pending document anchors (`anchorRequests`) | `docLinks.ts` | `docLinks.ts`. |

## Invariants / gotchas

- **Never lose an edit.** A buffer outlives its view while it holds unsaved edits: close, keep-alive eviction, split, zoom, drag, workspace switch, and rename all return to the same text and undo stack (`buffers.svelte.ts` header).
- **`EditorState`, `EditorView`, and `MergeView` are plain fields, never `$state`.** The same rule applies to xterm instances (`buffers.svelte.ts`, `DiffView.svelte`).
- **The file is the truth.** An unchanged file is never rewritten differently. A file with mixed line endings opens read-only (`textCodec.ts` header).
- **A save carries a content-hash precondition.** A disk change under a dirty buffer goes to merge or conflict, never to a silent overwrite (`buffers.svelte.ts` header).
- **A journal failure shows.** A storage failure in `drafts.ts` reads as "not journaled", never as safe.
- **Office files and notebook HTML are untrusted.** Office views render into detached nodes, run `sanitizeRendered`, then attach, so nothing a document names is fetched (`officeSafety.ts`). Notebook HTML runs in an iframe with no sandbox permissions (`NotebookHtml.svelte`).
- **Live and reading modes parse with one configuration** (`doc/parser.ts`), so the two views cannot disagree about a line.
- **Live mode is decoration-only.** It changes the document only for a task toggle, as a normal dispatched change (`mdLive.ts` header).
- **Do not touch `location.hash`.** Same-document anchors scroll in place; the hash is the app's bootstrap channel (`docLinks.ts` header).
- **A parked view stops its recurring work.** Gate polls and follow ticks on `paneVisibility(...).shown` (`viewLifecycle.svelte.ts`: the `visible` prop Pane passes through FileView, FinderView and DiffView, else `watchPaneVisible`, and `$pageVisible`; rule bullet on recurring work).
- **A preview's loads end with the view; disk changes while parked are applied on reveal.** Fetches carry a `viewSignal` / `onDiskVersion` signal, so a closed or superseded load is aborted and its result dropped. Pptx, Docx, Notebook, Board and Table views read a parked change off screen and apply it in the reveal's update, before it paints; PdfView pauses its page-size walk while parked. MarkdownView, CodeView and `buffers.svelte.ts` keep their own disk handling (they own "never lose an edit").
- **The presence heartbeat is a deliberate visibility exemption.** It runs only while this window holds unsaved edits (`Presence` doc comment).
- **Large reading documents window their DOM** (`readingWindow.ts`) because WebKit restyles every inline node on a tab switch.

## Checks

```sh
npm --prefix web-ui run check
npm --prefix web-ui run test -- src/lib/previews
```

A preview change needs a live check in the isolated preview with a real file of that type (**verify-app**).

## Neighbours

[layout](../layout/AGENTS.md) (`Pane.svelte` keeps views alive) · [workspace](../workspace/AGENTS.md) (`fsEvents.ts`, `diskWatch.ts`, `git.ts`) · [shared](../shared/AGENTS.md) (`editing.ts`, `math.ts`, `embed/`, `locator.ts`) · [chat](../chat/AGENTS.md) · [plugins](../plugins/AGENTS.md).
