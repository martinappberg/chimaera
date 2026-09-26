<script module lang="ts">
  import { createModeMemory } from "./mdDoc";

  /** The mode each file was last shown in: a per-viewer convenience in this
   *  browser's storage (never shared, never read back by the daemon). */
  const modeMemory = createModeMemory(() => localStorage);
  const PROPS_KEY = "chimaera.markdownPropsCollapsed";
  const OUTLINE_KEY = "chimaera.markdownOutline";
  function readFlag(key: string): boolean {
    try {
      return localStorage.getItem(key) === "1";
    } catch {
      return false;
    }
  }
  function writeFlag(key: string, on: boolean): void {
    try {
      localStorage.setItem(key, on ? "1" : "0");
    } catch {
      // storage unavailable: the choice lasts for this view
    }
  }
  /** Source bytes as text, decoded the way the editor decodes them. */
  const utf8 = new TextDecoder();
</script>

<script lang="ts">
  /**
   * Markdown with Obsidian-style modes: live | reading | source.
   *
   * READING is the complete, non-editable render, drawn HERE from the
   * document's current text (`currentText`: the editor's buffer once it
   * holds the file, unsaved edits included, else the file as read) by the
   * shared renderer (`doc/`: one parser for every view, lezer; the reader
   * keeps unchanged blocks' DOM across updates, so an agent rewriting one
   * paragraph re-flows nothing else). A file the client can't hold — over
   * the 1 MB edit cap, binary, unreadable — falls back to the daemon's
   * render (comrak → ammonia, the same markup). Either way its frontmatter
   * shows as a properties panel, its links open in the workbench
   * (docLinks.ts), and every block carries its source lines
   * (`data-sourcepos`), so a selection references real line numbers and a
   * reveal lands on the right block. LIVE is the reading view you type in —
   * the shared CodeMirror editor where every block the cursor is not in is
   * the same renderer's DOM under the same CSS (`.md-doc`, below; mdBlocks),
   * and only the block being edited shows as source, styled inline (mdLive).
   * Task boxes toggle the source in both. SOURCE is the same editor as plain
   * raw markdown. A mode switch keeps the block at the top of the view on
   * top. Live and source share ONE editor instance (an
   * extension swap, never a remount), and the editor mounts once and survives
   * every toggle, so flipping modes never drops an unsaved buffer or its undo
   * history. Saves, the dirty dot, and conflict handling all come from
   * CodeView (Cmd/Ctrl+S). The OUTLINE panel lists the headings in every
   * mode and follows the scroll.
   * A file opens in the mode it was last shown in, else the
   * `editor.markdownDefaultMode` setting (live by default). Editing is
   * offered only for files under the 1MB cap; larger markdown opens in
   * reading and stays there.
  */
  import { tick, untrack, type Component } from "svelte";
  import type { Extension } from "@codemirror/state";
  import type { EditorView } from "@codemirror/view";
  import {
    EDIT_MAX_BYTES,
    fsFile,
    lastRawTicketUrl,
    looksBinary,
    rawTicketUrl,
    resolveDocPath,
    safeDecodeUri,
    type FileChunk,
  } from "./files";
  import { retain, release, type FileEntry } from "./fileStore.svelte";
  import { openBuffer, releaseBuffer } from "./buffers.svelte";
  import { clearSelection, setSelection } from "../shared/reference";
  import { activeTheme, getSetting } from "../settings/store.svelte";
  import { getActiveWorkspaceId } from "../net/api";
  import { DocReader, MathTypesetter, markTasks, mathSpans } from "./doc/reader";
  import { DocEmbeds } from "./doc/embeds";
  import { createReadingWindow, type ReadingWindow } from "./readingWindow";
  import { copyText } from "../shared/clipboard";
  import { copyLabel, copyPayload, decorateCopyTargets } from "../shared/copyDecor";
  import { markScrollRegions, watchWidth } from "../shared/scrollRegion";
  import ReferenceChip from "../shared/ReferenceChip.svelte";
  import Chevron from "../shared/Chevron.svelte";
  import Spinner from "./Spinner.svelte";
  import DocIssues from "./DocIssues.svelte";
  import { hasUrlScheme, isWebUrl, urlMenuEntries } from "../shared/urlOpen";
  import { contextMenu } from "../shared/contextMenu.svelte";
  import { revealRequest, takeReveal, type Reveal } from "../shared/reveal";
  import { readingWindow } from "./readingWindow";
  import {
    frontmatterLineSpan,
    isMdMode,
    parseFrontmatter,
    parseSourcepos,
    placeOffset,
    revealIndex,
    spanLines,
    stripFences,
    type MdMode,
    type SourceRange,
  } from "./mdDoc";
  import {
    anchorRequests,
    findAnchor,
    followDocHref,
    revealAnchorInSource,
    showLinkHint,
    takeAnchor,
    type LinkContext,
  } from "./docLinks";

  interface Props {
    path: string;
    /** Per-pane text-size override (px); the preview body scales to it. The
     *  A−/A+ pane controls and the Cmd/Ctrl +/− chords both drive this. */
    fontSize?: number;
    /** Active workspace root: a root-relative `/docs/x.md` link resolves
     *  against it when it names no absolute path. */
    wsRoot?: string | null;
  }

  let { path, fontSize = undefined, wsRoot = null }: Props = $props();

  /** The path as a value: the host can hand this view its props anew with
   *  nothing changed (it does when the buffer turns dirty), and a per-path
   *  reset on that would remount the editor under the first keystroke. */
  const filePath = $derived(path);

  /** Read at click time, so the memoized live extension set never has to
   *  change when the workspace does. The window's workspace enables the
   *  daemon's by-name fallback (a wikilink, a moved file). */
  const linkContext = (): LinkContext => {
    let workspaceId: string | null = null;
    try {
      workspaceId = getActiveWorkspaceId();
    } catch {
      // storage unavailable: resolve without the workspace fallback
    }
    return { wsRoot, workspaceId };
  };
  /** The document's embed answers: every image reference it holds, resolved
   *  in one round trip that both views draw from. Per path; construction
   *  asks nothing (the first render does). */
  const docEmbeds = $derived(new DocEmbeds(filePath, linkContext));
  $effect(() => {
    const e = docEmbeds;
    return () => e.dispose();
  });
  // The file changed on disk (an agent's rewrite regenerates its figures
  // too): its references are asked again, and changed files redraw. The
  // first version seen is the one the first render already asked about.
  let embedsSeen: { embeds: DocEmbeds; mtime: string | null } | null = null;
  $effect(() => {
    const mtime = entry?.mtime ?? null;
    const embeds = docEmbeds;
    const seen = embedsSeen;
    embedsSeen = { embeds, mtime };
    if (seen !== null && seen.embeds === embeds && seen.mtime !== null && seen.mtime !== mtime) embeds.refresh();
  });
  /** What live mode reads at draw time (stable, like the link context). */
  const liveHost = { theme: (): "light" | "dark" => themeMode, embeds: (): DocEmbeds => docEmbeds };

  // Prose base size: the pane override, else the Markdown preference. Drives
  // the reading body AND the live editor, so the two views read identically.
  const bodyFont = $derived(fontSize ?? getSetting("editor.markdownFontSize"));
  /** The reading pane's accessible name: the file, not a generic word. */
  const fileLabel = $derived(path.split("/").filter(Boolean).pop() ?? path);
  const bodyLineHeight = $derived(getSetting("editor.markdownLineHeight"));

  type Mode = MdMode;
  let mode = $state<Mode>("reading");
  let chunk = $state<FileChunk | null>(null);
  let chunkError = $state<string | null>(null);
  /** Why an editor mode just refused (the file turned out over the cap or
   *  binary) — the buttons disable too, but a click deserves an answer. */
  let barNote = $state<string | null>(null);
  /** Size + binary sniff of the source, once probed (null = not yet known). */
  let srcSize = $state<number | null>(null);
  let srcBinary = $state(false);
  /** Whether the editor modes are offered; null until the first probe. */
  const editable = $derived(
    srcSize === null ? null : !srcBinary && srcSize <= EDIT_MAX_BYTES,
  );
  const disabledReason = $derived(
    srcBinary ? "binary content — reading only" : "over 1 MB — reading only",
  );
  /** The editor mounts on the first live/source entry and then persists
   *  (CSS-hidden in reading) so no toggle drops the unsaved buffer. */
  let entered = $state(false);
  /** Stamps user mode choices; async continuations apply only when theirs is
   *  still the latest, so a pending fetch can't override a later click. */
  let modeReq = 0;
  let CodeView = $state<
    Component<{
      path: string;
      first: FileChunk;
      /** The editor's text on mount and after every change: the reading
       *  render's source while the editor holds the file. */
      onDoc?: (text: string) => void;
      extra?: Extension;
      autoLanguage?: boolean;
      /** Whether the editor takes reveal requests for its path. The hidden
       *  editor behind reading must not: reading maps the line itself. */
      acceptReveal?: boolean;
    }> | null
  >(null);
  let liveMod = $state<typeof import("./mdLive") | null>(null);
  let codeLoadError = $state<string | null>(null);
  // Loaded eagerly on mount (not gated on entering an editor mode) so a
  // live/source open doesn't serialize the bundle import behind the chunk
  // fetch; both are cached after the first markdown file.
  $effect(() => {
    if (CodeView !== null) return;
    void Promise.all([import("./CodeView.svelte"), import("./mdLive")]).then(
      ([cv, live]) => {
        liveMod = live;
        CodeView = cv.default;
      },
      () => (codeLoadError = "failed to load the editor"),
    );
  });
  // The two extra-extension sets are memoized so mode flips hand CodeMirror
  // the SAME extension objects and it preserves their state: the markdown
  // language is a module singleton active in BOTH editor modes (a live ⇄
  // source flip never reparses) and the live set is per-path. Entering
  // reading changes nothing — the hidden editor keeps its current set.
  const liveSet = $derived(
    liveMod === null
      ? null
      : [liveMod.markdownLanguageExt, liveMod.markdownLive(filePath, linkContext, liveHost)],
  );
  const sourceSet = $derived(liveMod === null ? null : [liveMod.markdownLanguageExt]);
  let editorMode = $state<"live" | "source">("live");
  const extra = $derived.by(
    (): Extension => (editorMode === "live" ? liveSet : sourceSet) ?? [],
  );

  // The shared store entry: the source's first chunk lives here (cached
  // across tab switches, and refreshed in place when the file changes on
  // disk — a save in the editor, or an agent write, both flow through the
  // store), and the daemon's render for the fallback.
  let entry = $state<FileEntry | null>(null);

  /** Whether this view draws the document itself: known once the source is
   *  probed; false over the edit cap, for binary content, or when the source
   *  can't be read — the daemon's render stands in then. */
  let clientRender = $state<boolean | null>(null);
  /** The editor's buffer, once the editor holds the file (CodeView's sink:
   *  on mount and after every change, a reload from disk included). */
  let editorText = $state<string | null>(null);
  /** A whole-file read for a source past the store's first chunk (256 KB)
   *  and under the edit cap, with the version it read. */
  let wholeText = $state<{ mtime: string | null; text: string } | null>(null);
  /** The store's first chunk as text, when it IS the whole file. */
  const chunkText = $derived.by(() => {
    const c = entry?.chunk ?? null;
    return c === null || c.truncated || c.size > EDIT_MAX_BYTES ? null : utf8.decode(c.bytes);
  });

  /**
   * The document's current text, the reading render's one input: the
   * editor's buffer while the editor holds the file (unsaved edits
   * included), else the file as last read. Null while unknown. The source
   * sits behind this one function so it can move to a shared buffer store.
   */
  function currentText(): string | null {
    if (editorText !== null) return editorText;
    if (chunkText !== null) return chunkText;
    return wholeText?.text ?? null;
  }
  const docText = $derived(currentText());

  // The daemon's render (the fallback).
  const html = $derived(clientRender === false ? (entry?.markdown?.html ?? null) : null);
  const error = $derived(clientRender === false ? (entry?.markdownError ?? null) : null);
  /** The leading YAML block's text from whichever render shows (null on a
   *  document without one, and on an older daemon that renders it inline). */
  let clientFrontmatter = $state<string | null>(null);
  const frontmatter = $derived(
    clientRender === true ? clientFrontmatter : (entry?.markdown?.frontmatter ?? null),
  );
  const fmEntries = $derived(frontmatter === null ? null : parseFrontmatter(frontmatter));
  /** The panel stands in for the block's lines, fences included. */
  const fmSourcepos = $derived(
    frontmatter === null ? null : `1:1-${frontmatterLineSpan(frontmatter)}:3`,
  );
  let propsCollapsed = $state(readFlag(PROPS_KEY));
  function toggleProps(): void {
    propsCollapsed = !propsCollapsed;
    writeFlag(PROPS_KEY, propsCollapsed);
    // Live draws the same panel: it folds with this one.
    const view = editorView();
    if (view !== null) liveMod?.setLivePropsCollapsed(view, propsCollapsed);
  }
  /** Bumped after every client render: what follows the render (reveals,
   *  anchors, scroll regions, the outline's place) re-checks on it. */
  let renderTick = $state(0);
  const readingReady = $derived(clientRender === true ? renderTick > 0 : html !== null);

  // Reset per path — BEFORE the retain effect in source order, so a path swap
  // resets the view before the new entry is opened. The opening mode is the
  // one this file was last shown in, else the setting — read untracked, so a
  // settings change never resets an open document.
  $effect(() => {
    const p = filePath;
    const initial = untrack(() => {
      const setting = getSetting("editor.markdownDefaultMode");
      return modeMemory.get(p) ?? (isMdMode(setting) ? setting : "live");
    });
    mode = initial;
    editorMode = initial === "source" ? "source" : "live";
    modeReq++;
    chunk = null;
    chunkError = null;
    barNote = null;
    srcSize = null;
    srcBinary = false;
    entered = false;
    codeLoadError = null;
    clientRender = null;
    editorText = null;
    wholeText = null;
    clientFrontmatter = null;
    renderTick = 0;
    readingOutline = [];
    liveOutline = [];
    currentHeading = -1;
  });

  // Retain + open the chosen mode. The path is the only tracked dependency —
  // the store's retain()/ensure* guards are untracked by design (and
  // openDefault's read of `mode` is untracked here), so an in-place payload
  // refresh (a save, an agent write) or a mode click can never re-run this
  // effect and remount the editor over a dirty buffer.
  $effect(() => {
    const p = filePath;
    const e = retain(p);
    entry = e;
    untrack(() => void openDefault(e));
    return () => release(p);
  });

  /** Adopt the fetched source into local state. Oversized/binary chunks are
   *  dropped from the store — this view can never use them, and a retained
   *  useless payload would be re-downloaded on every disk revalidation. */
  function adoptChunk(e: FileEntry): "ok" | "failed" | "unusable" {
    if (e.chunk === null) return "failed";
    srcSize = e.chunk.size;
    srcBinary = looksBinary(e.chunk.bytes);
    if (srcBinary || e.chunk.size > EDIT_MAX_BYTES) {
      e.dropChunk();
      return "unusable";
    }
    chunk = e.chunk;
    chunkError = null;
    return "ok";
  }

  async function openDefault(e: FileEntry): Promise<void> {
    // Reading needs no source: the chunk is fetched (and the editor mounted)
    // on the first live/source click, so a plain read costs one request.
    if (mode === "reading") return;
    const req = modeReq;
    await e.ensureChunk();
    if (entry !== e || chunk !== null) return; // path changed, or a toggle won
    const r = adoptChunk(e);
    if (r === "ok") {
      // Only auto-enter while the user hasn't picked a mode themselves — a
      // reading click during the fetch must not get a hidden editor mount.
      if (req === modeReq) entered = true;
      return;
    }
    // Binary/oversized falls back to reading; so does a TRANSIENT fetch
    // failure, quietly — the error belongs to an explicit edit attempt
    // (enterEditor), not to a plain open that renders fine.
    if (req === modeReq) mode = "reading";
  }

  // Reading draws from the source, so its first entry probes it (the store's
  // first chunk: one request, reused by the editor modes). A source the
  // client can't hold turns to the daemon's render, which the store then
  // refreshes in place on every disk change or in-app save.
  $effect(() => {
    if (mode !== "reading") return;
    const e = entry;
    if (e !== null) untrack(() => void prepareReading(e));
  });

  async function prepareReading(e: FileEntry): Promise<void> {
    if (clientRender !== null) return;
    if (srcSize === null) {
      await e.ensureChunk();
      if (entry !== e || clientRender !== null) return;
      // A concurrent editor entry may have adopted it first.
      if (srcSize === null && adoptChunk(e) === "failed") {
        clientRender = false;
        void e.ensureMarkdown();
        return;
      }
    }
    clientRender = editable === true;
    if (!clientRender) void e.ensureMarkdown();
  }

  // A source past the first chunk is read whole (under the cap), and again
  // when the file changes on disk — unless the editor already holds it.
  $effect(() => {
    const e = entry;
    const c = e?.chunk ?? null;
    if (mode !== "reading" || clientRender !== true || editorText !== null) return;
    if (e === null || c === null || !c.truncated || c.size > EDIT_MAX_BYTES) return;
    const mtime = e.mtime;
    if (untrack(() => wholeText !== null && wholeText.mtime === mtime && mtime !== null)) return;
    let live = true;
    fsFile(path, 0, EDIT_MAX_BYTES).then(
      (full) => {
        if (live && entry === e) wholeText = { mtime: full.mtime ?? mtime, text: utf8.decode(full.bytes) };
      },
      () => {
        // Unreadable now: the last text stays; with none, the daemon's render.
        if (!live || entry !== e || untrack(() => wholeText !== null)) return;
        clientRender = false;
        void e.ensureMarkdown();
      },
    );
    return () => {
      live = false;
    };
  });

  // --- the client render --------------------------------------------------------
  let clientArticle = $state<HTMLElement | null>(null);
  /** The reader and the article's window, owned by the effect below (plain:
   *  never proxied); `readerTick` announces a new pair to the render. */
  let reader: DocReader | null = null;
  let articleWindow: ReadingWindow | null = null;
  let readerTick = $state(0);
  const themeMode = $derived(activeTheme().kind);

  $effect(() => {
    const el = clientArticle;
    const docPath = filePath;
    if (el === null) return;
    const win = createReadingWindow(el);
    const r = new DocReader(el, {
      docPath,
      links: linkContext,
      embeds: untrack(() => docEmbeds),
      theme: untrack(() => themeMode),
      // A wide equation typeset late is a scroller the last pass missed.
      onLayout: () => {
        if (readingEl !== null) markScrollRegions(readingEl, [[".md-math-display", null]]);
      },
    });
    reader = r;
    articleWindow = win;
    untrack(() => readerTick++);
    return () => {
      r.destroy();
      win.destroy();
      if (reader === r) reader = null;
      if (articleWindow === win) articleWindow = null;
    };
  });

  // Diagrams follow the theme, in both views.
  $effect(() => {
    const theme = themeMode;
    void readerTick;
    reader?.setTheme(theme);
    const view = untrack(editorView);
    if (view !== null) liveMod?.setLiveTheme(view, theme);
  });

  // The render: the current text into the article, incrementally — only
  // while reading shows (a hidden pane catches up on its next entry).
  $effect(() => {
    void readerTick;
    const text = docText;
    const el = clientArticle;
    const r = reader;
    if (mode !== "reading" || r === null || el === null || text === null) return;
    articleWindow?.restore();
    const res = r.update(text);
    clientFrontmatter = res.frontmatter;
    readingOutline = res.outline;
    decorateCopyTargets(el);
    markTasks(el);
    articleWindow?.schedule();
    untrack(() => renderTick++);
  });

  async function enterEditor(target: "live" | "source", place: Place | null = null): Promise<void> {
    const e = entry;
    if (e === null || editable === false) return;
    const req = modeReq;
    if (chunk === null) {
      await e.ensureChunk();
      // Bail when the path changed OR the user clicked another mode while the
      // fetch was in flight — finishing would override their later choice.
      if (entry !== e || req !== modeReq) return;
      const r = adoptChunk(e);
      if (r === "failed") {
        chunkError = e.chunkError ?? "failed to load source";
        return;
      }
      if (r === "unusable") {
        // binary / too large; stay in reading
        barNote = disabledReason;
        return;
      }
    } else if (!entered && e.chunk !== null && e.chunk !== chunk) {
      // Reading probed the source earlier and the file has changed on disk
      // since (the store refreshed it): the editor mounts on today's bytes,
      // never on a stale snapshot it would then reload over a first keystroke.
      adoptChunk(e);
    }
    entered = true;
    mode = target;
    editorMode = target;
    modeMemory.set(path, target);
    if (place !== null) void placeEditor(place, req);
  }

  function setMode(m: Mode): void {
    if (m === mode) return;
    const place = capturePlace();
    modeReq++;
    barNote = null;
    if (m === "reading") {
      mode = "reading";
      modeMemory.set(path, "reading");
      // Live may have folded the panel meanwhile.
      propsCollapsed = readFlag(PROPS_KEY);
      if (place !== null) void placeReading(place);
    } else {
      void enterEditor(m, place);
    }
  }

  // --- keeping your place across modes ----------------------------------------
  // The top visible block's first source line, how far its text sits below
  // the view's top edge, and its height: every block of every mode knows
  // its lines, so a switch puts the same block at the same height (one
  // scrolled partly by keeps that share of itself by: `placeOffset`).

  interface Place {
    line: number;
    offset: number;
    height: number;
  }

  /** The live/source editor, found from its DOM (null until mounted). */
  function editorView(): EditorView | null {
    const layer = editLayerEl;
    return layer === null || liveMod === null ? null : liveMod.editorIn(layer);
  }

  /** The top-level blocks of the reading render, in order. */
  function readingBlocks(): HTMLElement[] {
    const root = readingEl;
    const article = clientArticle ?? articleEl;
    if (root === null || article === null) return [];
    const out: HTMLElement[] = [];
    const props = root.querySelector<HTMLElement>(":scope > .md-props");
    if (props !== null) out.push(props);
    for (const el of article.children) if (el instanceof HTMLElement) out.push(el);
    return out;
  }

  function capturePlace(): Place | null {
    if (mode !== "reading") {
      const view = editorView();
      return view === null || liveMod === null ? null : liveMod.editorPlace(view);
    }
    const root = readingEl;
    if (root === null) return null;
    const top = root.getBoundingClientRect().top;
    for (const el of readingBlocks()) {
      const r = el.getBoundingClientRect();
      if (r.bottom <= top + 1) continue;
      const range = parseSourcepos(el.getAttribute("data-sourcepos"));
      if (range !== null) return { line: range.start, offset: r.top - top, height: r.height };
    }
    return null;
  }

  /** Once reading has rendered and laid out, the block holding the line
   *  goes back to its height. */
  async function placeReading(place: Place): Promise<void> {
    const req = modeReq;
    await tick();
    afterLayout(() => {
      const root = readingEl;
      if (root === null || req !== modeReq || mode !== "reading") return;
      const blocks = readingBlocks();
      const i = revealIndex(
        blocks.map((el) => parseSourcepos(el.getAttribute("data-sourcepos"))),
        place.line,
      );
      if (i < 0) return;
      const r = blocks[i].getBoundingClientRect();
      root.scrollTop += r.top - root.getBoundingClientRect().top - placeOffset(place.offset, place.height, r.height);
    });
  }

  /** Once the editor shows (a first entry mounts it), the same. */
  async function placeEditor(place: Place, req: number): Promise<void> {
    await tick();
    for (let tries = 0; tries < 60; tries++) {
      if (req !== modeReq) return;
      const view = editorView();
      if (view !== null && view.scrollDOM.clientHeight > 0) {
        liveMod?.restoreEditorPlace(view, place.line, place.offset, place.height);
        return;
      }
      await new Promise((r) => requestAnimationFrame(r));
    }
  }

  // --- task boxes in reading -----------------------------------------------------
  /** `- [ ]` at the head of a list item's first line (inside quotes too):
   *  the text before the box, and its mark. */
  const TASK_LINE = /^([ \t]*(?:>[ \t]?)*[ \t]*(?:[-+*]|\d{1,9}[.)])[ \t]+)\[([ xX])\]/;

  /**
   * A box clicked in reading toggles the source, through the file's one
   * buffer (the editor's, when it holds the file) — an edit like any other:
   * dirty, undoable, autosaved. The editor mounts (hidden) so the render
   * follows the buffer from here on.
   */
  async function toggleTaskInReading(box: HTMLElement): Promise<void> {
    const text = docText;
    const e = entry;
    const first = e?.chunk ?? chunk;
    if (clientRender !== true || editable !== true || text === null || first === null) return;
    const range = parseSourcepos(box.closest("[data-sourcepos]")?.getAttribute("data-sourcepos"));
    if (range === null) return;
    const lines = text.split("\n");
    const lineText = lines[range.start - 1];
    const m = lineText === undefined ? null : TASK_LINE.exec(lineText);
    if (m === null) return;
    const buf = openBuffer(path, first);
    try {
      for (let tries = 0; buf.loading && tries < 100; tries++) await new Promise((r) => setTimeout(r, 30));
      const doc = buf.current.doc;
      // The buffer must hold the text reading drew (it can differ only when
      // unsaved edits live in a buffer this view never showed).
      if (range.start > doc.lines || doc.line(range.start).text !== lineText) {
        entered = true;
        return;
      }
      const from = doc.line(range.start).from + m[1].length;
      if (!buf.edit({ changes: { from, to: from + 3, insert: m[2] === " " ? "[x]" : "[ ]" } })) return;
      if (editorText === null) editorText = buf.current.doc.toString();
      entered = true;
    } finally {
      releaseBuffer(buf);
    }
  }

  // --- context bridge: selection in the RENDERED reading view ---------------
  // Every block of the render carries its source lines (`data-sourcepos`),
  // so a reference names the lines the selection's two ends sit in; a
  // render without them (an older daemon) sends the quoted excerpt alone.
  const selOwner = {};
  let contentEl = $state<HTMLDivElement | null>(null);

  // The daemon's render (the fallback) gets the chrome the client render
  // draws itself: copy buttons on fences and quotes (the chat transcript's
  // affordance, via the shared decorator), document-relative images, task
  // marks, typeset equations, and the outline read off its headings. Scoped
  // to the rendered article (never the editor subtree, nor the Svelte-owned
  // properties panel) and gated on reading being shown — a hidden render
  // pane skips the DOM walk and catches up when reading is next entered.
  let readingEl = $state<HTMLDivElement | null>(null);
  let articleEl = $state<HTMLElement | null>(null);
  $effect(() => {
    void html;
    if (mode !== "reading" || clientRender !== false) return;
    const content = articleEl;
    if (content === null) return;
    decorateCopyTargets(content);
    stampImages(content);
    markTasks(content);
    readingOutline = outlineOfRender(content);
    const math = new MathTypesetter(() => {
      if (readingEl !== null) markScrollRegions(readingEl, [[".md-math-display", null]]);
    });
    math.add(mathSpans([content]));
    return () => math.stop();
  });

  /** The outline of a daemon render: its headings, their ids and lines. */
  function outlineOfRender(root: HTMLElement): OutlineItem[] {
    const out: OutlineItem[] = [];
    for (const h of root.querySelectorAll<HTMLElement>("h1, h2, h3, h4, h5, h6")) {
      const line = parseSourcepos(h.getAttribute("data-sourcepos"))?.start ?? 0;
      out.push({
        level: Number(h.tagName.slice(1)),
        text: (h.textContent ?? "").replace(/\s+/g, " ").trim(),
        id: (h.id ?? "").replace(/^user-content-/, ""),
        from: line,
        line,
      });
    }
    return out;
  }

  // --- place: anchors, line reveals ------------------------------------------

  /** Block elements a line reveal can land on (inline marks may carry
   *  sourcepos too; a flash on a word reads as noise). */
  const REVEAL_TAGS = new Set([
    "P", "H1", "H2", "H3", "H4", "H5", "H6", "LI", "UL", "OL", "BLOCKQUOTE", "PRE",
    "TABLE", "TR", "HR", "DIV", "DL", "DT", "DD", "SECTION", "DETAILS", "FIGURE",
  ]);

  /** Scroll the reading pane (only — never its ancestors, which
   *  scrollIntoView would drag along) so `el` sits near the top. */
  function scrollToEl(root: HTMLElement, el: Element): void {
    const top = el.getBoundingClientRect().top - root.getBoundingClientRect().top + root.scrollTop;
    root.scrollTop = Math.max(0, top - Math.min(72, root.clientHeight / 4));
  }

  const FLASH_MS = 1600;
  /** Mark where a jump landed. A readingWindow placeholder (inert, aria-
   *  hidden, same tag and sourcepos) is swapped for its block once scrolled
   *  into view, so the flash waits a few frames for the real one. */
  function flash(el: HTMLElement, tries = 4): void {
    const pos = el.getAttribute("data-sourcepos");
    if (el.inert && pos !== null && tries > 0) {
      requestAnimationFrame(() => {
        const real = readingEl?.querySelector<HTMLElement>(
          `[data-sourcepos="${CSS.escape(pos)}"]:not([inert])`,
        );
        flash(real ?? el, real === null || real === undefined ? tries - 1 : 0);
      });
      return;
    }
    el.classList.remove("md-flash");
    void el.offsetWidth; // restart the animation on a repeat jump
    el.classList.add("md-flash");
    setTimeout(() => el.classList.remove("md-flash"), FLASH_MS);
  }

  function toAnchorInReading(anchor: string): boolean {
    const root = readingEl;
    if (root === null) return false;
    const el = findAnchor(root, anchor);
    if (el === null) return false;
    scrollToEl(root, el);
    // A heading's id sits on an empty anchor inside it; flash the block.
    const block = el.closest<HTMLElement>("[data-sourcepos], h1, h2, h3, h4, h5, h6, li, p");
    flash(block !== null && root.contains(block) ? block : el);
    return true;
  }

  function revealInReading(r: Reveal): void {
    const root = readingEl;
    if (root === null) return;
    const els = Array.from(root.querySelectorAll<HTMLElement>("[data-sourcepos]")).filter((el) =>
      REVEAL_TAGS.has(el.tagName),
    );
    const ranges = els.map((el) => parseSourcepos(el.getAttribute("data-sourcepos")));
    const i = revealIndex(ranges, r.line);
    if (i < 0) return;
    scrollToEl(root, els[i]);
    const end = r.endLine ?? r.line;
    if (end <= r.line) {
      flash(els[i]);
      return;
    }
    // A range: flash the innermost blocks it touches (bounded — a reveal of
    // a whole long file flashes its first screenfuls, not every block).
    const hits = els.filter((_, j) => {
      const g = ranges[j];
      return g !== null && g.start <= end && g.end >= r.line;
    });
    const leaves = hits.length > 200 ? hits.slice(0, 200) : hits;
    for (const el of leaves) {
      if (!leaves.some((o) => o !== el && el.contains(o))) flash(el);
    }
  }

  /** One frame after the render lays out, jumps run — queued, not tied to
   *  an effect's teardown: taking a reveal clears the store the effect
   *  reads, and a teardown-cancelled frame would drop the very jump that
   *  re-ran it. */
  let layoutJobs: (() => void)[] = [];
  let layoutFrame = 0;
  function afterLayout(job: () => void): void {
    layoutJobs.push(job);
    if (layoutFrame !== 0) return;
    layoutFrame = requestAnimationFrame(() => {
      layoutFrame = 0;
      const jobs = layoutJobs;
      layoutJobs = [];
      for (const j of jobs) j();
    });
  }
  $effect(() => () => {
    if (layoutFrame !== 0) cancelAnimationFrame(layoutFrame);
    layoutFrame = 0;
    layoutJobs = [];
  });

  // A reveal (a `#L12` link, an agent's "look here") is this view's to take
  // only while reading shows; in live/source the editor takes it
  // (acceptReveal). On render, too: the request may predate the fetch.
  $effect(() => {
    void $revealRequest;
    void renderTick;
    if (mode !== "reading" || !readingReady || readingEl === null) return;
    const req = takeReveal(path);
    if (req !== null) afterLayout(() => revealInReading(req));
  });

  // A link from another document (`this.md#heading`) left its anchor
  // pending for this path. Reading scrolls to the element; the editor
  // modes map it to a line (through the buffer's own headings once the
  // editor holds it) and reveal that.
  $effect(() => {
    void $anchorRequests;
    void renderTick;
    if (mode === "reading") {
      if (!readingReady || readingEl === null) return;
      const anchor = takeAnchor(path);
      if (anchor !== null) afterLayout(() => void toAnchorInReading(anchor));
    } else {
      const anchor = takeAnchor(path);
      if (anchor !== null) void revealAnchorInSource(path, anchor, untrack(() => editorText));
    }
  });

  /** Keyboard reach for the reading view's horizontal scrollers
   *  (shared/scrollRegion.ts): a table — comrak's bare <table> is its own
   *  display:block scroller, and ammonia strips any tabindex the file might
   *  carry, so the mark is set here after render, tabindex only so the table
   *  keeps its own role — a fence's code box, and a display equation, each a
   *  tab stop while it overflows. Overflow moves with the column's width, the
   *  pane's text size, and a table's content (an equation typeset at idle, an
   *  image decoded late), so every table's first row group is watched as its
   *  width proxy alongside the pane. The reading pane itself is a named
   *  region in the markup, always. */
  const READING_SCROLLERS = [["table, pre > code, .md-math-display", null]] as const;
  $effect(() => {
    void html;
    void renderTick;
    void bodyFont; // dep: A−/A+ reflows every scroller
    if (mode !== "reading") return;
    const scroll = readingEl;
    if (scroll === null) return;
    const recheck = () => markScrollRegions(scroll, READING_SCROLLERS);
    recheck();
    const stops = [
      watchWidth(scroll, recheck),
      ...Array.from(scroll.querySelectorAll("table > thead, table > tbody"), (g) =>
        watchWidth(g, recheck),
      ),
    ];
    return () => stops.forEach((stop) => stop());
  });

  /** `![](figs/plot.png)` in a document: the rendered src is relative, which
   *  the browser would resolve against the APP origin (a guaranteed 404).
   *  Re-point each such image at a short-lived ticketed /raw/ URL for the
   *  path relative to the file: the last answer at once, so a re-render
   *  keeps the src (no flash), then the daemon's current one if the image
   *  changed since (a new version is a new ticket). Web/data URLs pass
   *  through untouched. */
  function stampImages(root: HTMLElement): void {
    for (const img of root.querySelectorAll("img")) {
      const src = img.getAttribute("src") ?? "";
      if (src === "" || hasUrlScheme(src) || src.startsWith("/raw/")) continue;
      if (img.dataset.mdSrc === src) continue;
      img.dataset.mdSrc = src;
      const target = resolveDocPath(path, safeDecodeUri(src));
      const last = lastRawTicketUrl(target);
      if (last !== null) img.src = last;
      rawTicketUrl(target).then(
        (url) => {
          if (img.isConnected && img.dataset.mdSrc === src && url !== last) img.src = url;
        },
        () => {
          // missing/unreadable target: leave the img alone (alt text shows)
        },
      );
    }
  }

  let copiedBtn: HTMLElement | null = null;
  let copiedTimer: ReturnType<typeof setTimeout> | null = null;
  function flashCopied(btn: HTMLElement): void {
    if (copiedTimer !== null) clearTimeout(copiedTimer);
    if (copiedBtn !== null && copiedBtn.isConnected && copiedBtn !== btn) {
      copiedBtn.classList.remove("copied");
      copiedBtn.setAttribute("aria-label", copyLabel(copiedBtn.closest("pre, blockquote") ?? copiedBtn));
    }
    copiedBtn = btn;
    btn.classList.add("copied");
    btn.setAttribute("aria-label", "copied");
    copiedTimer = setTimeout(() => {
      copiedTimer = null;
      if (btn.isConnected) {
        btn.classList.remove("copied");
        btn.setAttribute("aria-label", copyLabel(btn.closest("pre, blockquote") ?? btn));
      }
      copiedBtn = null;
    }, 1400);
  }
  $effect(() => () => {
    if (copiedTimer !== null) clearTimeout(copiedTimer);
  });

  function onLinkClick(e: MouseEvent): void {
    // Live mode's rendered blocks handle their own presses (mdBlocks), an
    // embed card its own.
    if (!(e.target instanceof Element) || readingEl?.contains(e.target) !== true) return;
    if (e.target.closest(".embed-card") !== null) return;
    const box = e.target.closest<HTMLElement>("span.md-task");
    if (box !== null && box.closest(".md-props") === null) {
      e.preventDefault();
      void toggleTaskInReading(box);
      return;
    }
    const copyBtn = e.target.closest("button.md-copy");
    if (copyBtn instanceof HTMLElement) {
      const payload = copyPayload(copyBtn);
      if (payload.length > 0) {
        void copyText(payload).then((ok) => {
          if (ok && copyBtn.isConnected) flashCopied(copyBtn);
        });
      }
      return;
    }
    followLink(e, e.metaKey || e.ctrlKey);
  }

  /** A middle-click would open the raw href in a new BROWSER tab — for a
   *  document-relative path, the app origin's 404. A file link opens beside
   *  instead; a web link keeps the browser's own new-tab behavior. */
  function onLinkAuxClick(e: MouseEvent): void {
    if (e.button !== 1 || (e.target as Element | null)?.closest?.(".embed-card") != null) return;
    const href = (e.target as Element | null)?.closest?.("a[href]")?.getAttribute("href") ?? "";
    if (isWebUrl(href)) return;
    followLink(e, true);
  }

  function followLink(e: MouseEvent, split: boolean): void {
    const anchor = (e.target as Element | null)?.closest?.("a[href]");
    if (anchor === null || anchor === undefined || readingEl?.contains(anchor) !== true) return;
    const href = anchor.getAttribute("href") ?? "";
    // `mailto:`/`tel:` are the browser's to handle — the OS knows what to do
    // with them and swallowing the click would just make the link look dead.
    // They cannot navigate the workbench away, so letting them through is safe.
    if (/^(mailto|tel):/i.test(href)) return;
    // Everything else is routed, never a native navigation: nothing sets a
    // `target` here, so a click would be a TOP-LEVEL navigation — in a
    // browser that replaces the whole workbench, and in the native app the
    // shell's guard swallows it. A web URL opens in a browser pane (a live
    // local app) or the real browser; a file link opens in the workbench,
    // Cmd/Ctrl beside; `#anchor` / `#L12` scroll this document without
    // touching location.hash; any other scheme is dropped.
    e.preventDefault();
    const x = e.clientX;
    const y = e.clientY;
    void followDocHref(
      href,
      split,
      {
        docPath: path,
        ...linkContext(),
        toAnchor: toAnchorInReading,
        toLines: revealInReading,
        hint: (text) => {
          if (contentEl !== null) showLinkHint(contentEl, x, y, text);
        },
      },
      { byName: anchor.hasAttribute("data-wikilink") },
    );
  }

  function onLinkContextMenu(e: MouseEvent): void {
    if ((e.target as Element | null)?.closest?.(".embed-card") != null) return;
    const anchor = (e.target as Element | null)?.closest?.("a[href]");
    const href = anchor?.getAttribute("href") ?? "";
    if (anchor === null || anchor === undefined || !isWebUrl(href)) return;
    contextMenu.openAt(e, urlMenuEntries(href));
  }
  let chipPos = $state<{ x: number; y: number } | null>(null);

  function syncPreviewSelection(): void {
    const content = contentEl;
    const s = document.getSelection();
    if (content === null || s === null || s.rangeCount === 0 || s.isCollapsed) {
      chipPos = null;
      clearSelection(selOwner);
      return;
    }
    const range = s.getRangeAt(0);
    if (!content.contains(range.commonAncestorContainer)) {
      // A selection elsewhere in the app: drop only what this view owns.
      chipPos = null;
      clearSelection(selOwner);
      return;
    }
    const text = s.toString();
    if (text.trim().length === 0) {
      chipPos = null;
      clearSelection(selOwner);
      return;
    }
    const lines = spanLines(
      linesAt(range.startContainer, range.startOffset),
      linesAt(range.endContainer, Math.max(range.endOffset - 1, 0)),
    );
    const heading = headingAt(range.startContainer);
    setSelection(selOwner, {
      kind: "file",
      path,
      startLine: lines?.start ?? null,
      endLine: lines?.end ?? null,
      text,
      // Where it sits, for the agent: `@doc.md#L40-L42 (§ Results) "…"`.
      ...(heading !== null ? { context: `§ ${heading}` } : {}),
    });
    chipPos = chipPosFor(content, range);
  }

  /** The heading a reading-view node sits under: the last h1–h6 at or
   *  before it in document order (null above the first heading). */
  function headingAt(node: Node): string | null {
    const root = readingEl;
    if (root === null) return null;
    let found: Element | null = null;
    for (const h of root.querySelectorAll("h1, h2, h3, h4, h5, h6")) {
      const before = h === node || h.contains(node) || (h.compareDocumentPosition(node) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0;
      if (!before) break;
      found = h;
    }
    const text = found?.textContent?.replace(/\s+/g, " ").trim() ?? "";
    return text === "" ? null : text;
  }

  /** The source lines of the innermost mapped block holding a selection
   *  boundary (an element boundary names the child at its offset). */
  function linesAt(node: Node, offset: number): SourceRange | null {
    let n: Node = node;
    if (n instanceof Element && n.childNodes.length > 0) {
      n = n.childNodes[Math.min(offset, n.childNodes.length - 1)];
    }
    const el = n instanceof Element ? n : n.parentElement;
    const block = el?.closest<HTMLElement>("[data-sourcepos]") ?? null;
    if (block === null || readingEl?.contains(block) !== true) return null;
    return parseSourcepos(block.getAttribute("data-sourcepos"));
  }

  /** Where the chip sits for a selection: just past its last rect, clamped
   *  inside the content box (one rule for placement and re-anchoring). */
  function chipPosFor(content: HTMLElement, range: Range): { x: number; y: number } {
    const rects = range.getClientRects();
    const last = rects.length > 0 ? rects[rects.length - 1] : range.getBoundingClientRect();
    const rect = content.getBoundingClientRect();
    const clamp = (n: number, lo: number, hi: number) => Math.min(Math.max(n, lo), Math.max(lo, hi));
    return {
      x: clamp(last.right - rect.left + 4, 4, rect.width - 170),
      y: clamp(last.bottom - rect.top + 6, 4, rect.height - 58),
    };
  }

  /** Geometry only: re-anchor an existing chip to the selection's last rect
   *  (a scroll moves the selection; it doesn't change what is selected). */
  function placeChip(): void {
    const content = contentEl;
    const s = document.getSelection();
    if (content === null || chipPos === null || s === null || s.rangeCount === 0) return;
    chipPos = chipPosFor(content, s.getRangeAt(0));
  }

  $effect(() => {
    if (mode !== "reading") {
      chipPos = null;
      clearSelection(selOwner);
      return;
    }
    // `scroll` doesn't bubble, so the reading pane's own scroll would miss
    // the inner scrollers (a wide table, a fence, display math): a capturing
    // listener on the root sees every descendant's scroll and re-anchors the
    // chip once per frame — geometry only, no selection-store churn.
    const root = readingEl;
    const scrollOpts = { capture: true, passive: true } as const;
    let frame = 0;
    const onScroll = () => {
      if (frame !== 0) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        placeChip();
      });
    };
    document.addEventListener("selectionchange", syncPreviewSelection);
    root?.addEventListener("scroll", onScroll, scrollOpts);
    return () => {
      document.removeEventListener("selectionchange", syncPreviewSelection);
      root?.removeEventListener("scroll", onScroll, scrollOpts);
      if (frame !== 0) cancelAnimationFrame(frame);
      chipPos = null;
      clearSelection(selOwner);
    };
  });

  // --- outline ---------------------------------------------------------------------
  // The headings of the document in every mode: reading reads them off its
  // render, live and source off the editor's own syntax tree (with the ids
  // the reading view gives them). The current one follows the scroll; a
  // click jumps — reading scrolls to the heading, the editor scrolls its
  // line to the top without moving the cursor.

  interface OutlineItem {
    level: number;
    text: string;
    /** The anchor, unprefixed. */
    id: string;
    /** Source offset (editor) — the reading render's line for a daemon render. */
    from: number;
    line: number;
  }

  let outlineOpen = $state(readFlag(OUTLINE_KEY));
  let readingOutline = $state.raw<OutlineItem[]>([]);
  let liveOutline = $state.raw<OutlineItem[]>([]);
  let currentHeading = $state(-1);
  const outline = $derived(mode === "reading" ? readingOutline : liveOutline);
  /** Indentation starts at the shallowest level present. */
  const outlineBase = $derived(outline.reduce((m, h) => Math.min(m, h.level), 6));

  function toggleOutline(): void {
    outlineOpen = !outlineOpen;
    writeFlag(OUTLINE_KEY, outlineOpen);
  }

  /** The heading the reading pane's top sits in: the last one at or above
   *  the line a jump lands headings on (`scrollToEl`), so a click and the
   *  scroll agree — document order is geometric order, so a binary search
   *  over the headings' rects. Scrolled to the end, the last heading in
   *  view wins: a short final section never reaches the top. */
  function headingAtTop(root: HTMLElement, items: readonly OutlineItem[]): number {
    const box = root.getBoundingClientRect();
    const atEnd = root.scrollTop + root.clientHeight >= root.scrollHeight - 2 && root.scrollTop > 0;
    const top = atEnd ? box.bottom - 24 : box.top + Math.min(72, root.clientHeight / 4) + 8;
    const rectTop = (i: number): number =>
      findAnchor(root, items[i].id)?.getBoundingClientRect().top ?? Infinity;
    let lo = 0;
    let hi = items.length - 1;
    let found = -1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (rectTop(mid) <= top) {
        found = mid;
        lo = mid + 1;
      } else {
        hi = mid - 1;
      }
    }
    return found;
  }

  $effect(() => {
    if (!outlineOpen || mode !== "reading") return;
    const root = readingEl;
    const items = readingOutline;
    void renderTick;
    if (root === null) return;
    let frame = 0;
    const measure = (): void => {
      frame = 0;
      currentHeading = headingAtTop(root, items);
    };
    const onScroll = (): void => {
      if (frame === 0) frame = requestAnimationFrame(measure);
    };
    measure();
    root.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      root.removeEventListener("scroll", onScroll);
      if (frame !== 0) cancelAnimationFrame(frame);
    };
  });

  // The editor behind live/source, found from its DOM once CodeView has
  // mounted it (a few frames after the layer appears).
  let editLayerEl = $state<HTMLDivElement | null>(null);
  let liveView = $state.raw<EditorView | null>(null);
  $effect(() => {
    const layer = editLayerEl;
    const live = liveMod;
    void CodeView;
    liveView = null;
    if (layer === null || live === null || !outlineOpen) return;
    let tries = 0;
    let frame = 0;
    const find = (): void => {
      frame = 0;
      const view = live.editorIn(layer);
      if (view !== null) liveView = view;
      else if (++tries < 120) frame = requestAnimationFrame(find);
    };
    find();
    return () => {
      if (frame !== 0) cancelAnimationFrame(frame);
    };
  });

  // Live/source: the outline follows the buffer (settled, not per key).
  $effect(() => {
    void docText;
    const view = liveView;
    const live = liveMod;
    if (!outlineOpen || mode === "reading" || view === null || live === null) return;
    const read = (): void => {
      const doc = view.state.doc;
      liveOutline = live.editorOutline(view).map((h) => ({ ...h, line: doc.lineAt(h.from).number }));
    };
    if (untrack(() => liveOutline.length === 0)) {
      read();
      return;
    }
    const timer = setTimeout(read, 150);
    return () => clearTimeout(timer);
  });

  $effect(() => {
    const view = liveView;
    const live = liveMod;
    const items = liveOutline;
    if (!outlineOpen || mode === "reading" || view === null || live === null) return;
    let frame = 0;
    const measure = (): void => {
      frame = 0;
      const top = live.editorTopPos(view);
      let found = -1;
      for (let i = 0; i < items.length && items[i].from <= top; i++) found = i;
      currentHeading = found;
    };
    const onScroll = (): void => {
      if (frame === 0) frame = requestAnimationFrame(measure);
    };
    measure();
    view.scrollDOM.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      view.scrollDOM.removeEventListener("scroll", onScroll);
      if (frame !== 0) cancelAnimationFrame(frame);
    };
  });

  function jumpTo(i: number): void {
    const item = outline[i];
    if (item === undefined) return;
    if (mode === "reading") {
      toAnchorInReading(item.id);
    } else if (liveView !== null && liveMod !== null) {
      liveMod.scrollEditorTo(liveView, item.from);
    }
    currentHeading = i;
  }
</script>

<!-- The frontmatter as properties: a sibling of the article, never inside
     it (the fallback's raw-HTML range owns the article's first and last
     nodes — readingWindow's boundary rule; the client article is the
     reader's alone). Every value is text-interpolated; the file's YAML is
     never markup. -->
{#snippet properties(fm: string)}
  <section
    class="md-props"
    class:collapsed={propsCollapsed}
    style:font-size="{bodyFont}px"
    data-sourcepos={fmSourcepos}
    aria-label="properties"
  >
    <button
      type="button"
      class="md-props-head"
      aria-expanded={!propsCollapsed}
      onclick={toggleProps}
    >
      <Chevron open={!propsCollapsed} size={10} />
      <span>properties</span>
      {#if fmEntries !== null}<span class="md-props-count">{fmEntries.length}</span>{/if}
    </button>
    {#if !propsCollapsed}
      {#if fmEntries !== null && fmEntries.length > 0}
        <dl class="md-props-list">
          {#each fmEntries as prop, i (i)}
            <dt>{prop.key}</dt>
            <dd>
              {#if prop.value.kind === "list"}
                {#each prop.value.items as item, j (j)}
                  <span class="md-prop-chip">{item}</span>
                {:else}
                  <span class="md-prop-empty">—</span>
                {/each}
              {:else if prop.value.kind === "bool"}
                <span
                  class="md-task"
                  data-task={prop.value.value ? "done" : "todo"}
                  role="img"
                  aria-label={prop.value.value ? "true" : "false"}
                ></span>
              {:else if prop.value.kind === "raw"}
                <pre class="md-prop-raw">{prop.value.text}</pre>
              {:else if prop.value.text === ""}
                <span class="md-prop-empty">—</span>
              {:else}
                <span class="md-prop-text">{prop.value.text}</span>
              {/if}
            </dd>
          {/each}
        </dl>
      {:else}
        <pre class="md-props-raw">{stripFences(fm)}</pre>
      {/if}
    {/if}
  </section>
{/snippet}

<div class="md-view" style:--markdown-line-height={bodyLineHeight}>
  <div class="md-bar">
    <div class="toggle" role="tablist" aria-label="markdown mode">
      <button
        class="seg"
        class:on={mode === "live"}
        role="tab"
        aria-selected={mode === "live"}
        title={editable === false ? disabledReason : "reading view you can edit (live preview)"}
        disabled={editable === false}
        onclick={() => setMode("live")}>live</button
      >
      <button
        class="seg"
        class:on={mode === "reading"}
        role="tab"
        aria-selected={mode === "reading"}
        title="rendered document"
        onclick={() => setMode("reading")}>reading</button
      >
      <button
        class="seg"
        class:on={mode === "source"}
        role="tab"
        aria-selected={mode === "source"}
        title={editable === false ? disabledReason : "raw markdown source"}
        disabled={editable === false}
        onclick={() => setMode("source")}>source</button
      >
    </div>
    {#if chunkError !== null}
      <span class="md-bar-err">{chunkError}</span>
    {:else if barNote !== null}
      <span class="md-bar-note">{barNote}</span>
    {/if}
    <span class="md-bar-fill"></span>
    <DocIssues {path} {wsRoot} mtime={entry?.mtime ?? null} />
    <button
      class="seg"
      class:on={outlineOpen}
      aria-pressed={outlineOpen}
      title="headings of this document"
      onclick={toggleOutline}>outline</button
    >
  </div>

  <div class="md-main">
    <!-- Delegated link handling: the interactive targets are the rendered
         document's own <a> elements, which are already focusable and fire a
         native click on Enter that bubbles here — so keyboard access needs
         no separate handler on the container. -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div
      class="md-content"
      bind:this={contentEl}
      onclick={onLinkClick}
      onauxclick={onLinkAuxClick}
      oncontextmenu={onLinkContextMenu}
    >
      {#if mode === "reading" && chipPos !== null}
        <ReferenceChip x={chipPos.x} y={chipPos.y} />
      {/if}

      <!-- The rendered document. Shown in reading mode; kept in the DOM
           (just hidden) so re-entering reading re-renders only what changed.
           Focusable so keyboard scrolling works in WKWebView (Safari never
           auto-focuses scrollers), named after the file for the landmark
           list. -->
      <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
      <div
        class="md-scroll"
        class:hidden={mode !== "reading"}
        role="region"
        aria-label={fileLabel}
        tabindex="0"
        bind:this={readingEl}
      >
        {#if clientRender === true}
          {#if frontmatter !== null}{@render properties(frontmatter)}{/if}
          <!-- The client render: the reader owns every child. -->
          <article
            class="md-body md-doc"
            class:after-props={frontmatter !== null}
            style:font-size="{bodyFont}px"
            bind:this={clientArticle}
          ></article>
          {#if renderTick === 0}
            <Spinner />
          {/if}
        {:else if clientRender === false && error !== null}
          <div class="file-error">{error}</div>
        {:else if clientRender === false && html !== null}
          {#if frontmatter !== null}{@render properties(frontmatter)}{/if}
          <!-- The daemon's render (the fallback), sanitized server-side. -->
          <article
            class="md-body md-doc"
            class:after-props={frontmatter !== null}
            style:font-size="{bodyFont}px"
            bind:this={articleEl}
            use:readingWindow={[html, bodyFont, bodyLineHeight]}
          >
            <!-- eslint-disable-next-line svelte/no-at-html-tags — sanitized server-side -->
            {@html html}
          </article>
        {:else}
          <Spinner />
        {/if}
      </div>

      <!-- The one editor (live preview ⇄ raw source via the extra-extension
           swap). Mounts on the first live/source entry and then persists,
           CSS-hidden in reading, so no toggle drops the buffer. The prose
           size rides CSS variables so an A−/A+ resize never reconfigures the
           editor (the live theme is static — see mdLive). -->
      {#if entered && chunk !== null}
        {@const first = chunk}
        <div
          class="edit-layer"
          class:hidden={mode === "reading"}
          style:--lp-font-size="{bodyFont}px"
          style:--lp-line-height={bodyLineHeight}
          bind:this={editLayerEl}
        >
          {#if CodeView !== null}
            <CodeView
              {path}
              {first}
              {extra}
              autoLanguage={false}
              acceptReveal={mode !== "reading"}
              onDoc={(text: string) => (editorText = text)}
            />
          {:else if codeLoadError !== null}
            <div class="file-error">{codeLoadError}</div>
          {:else}
            <Spinner />
          {/if}
        </div>
      {:else if mode !== "reading"}
        <!-- The source is still on its way in (openDefault's first fetch); a
             fetch failure lands in reading, so this is only ever a wait. -->
        <div class="md-scroll">
          <Spinner />
        </div>
      {/if}
    </div>

    {#if outlineOpen}
      <nav class="md-outline" aria-label="outline">
        {#if outline.length === 0}
          <p class="md-outline-empty">no headings</p>
        {:else}
          <ul>
            {#each outline as item, i (item.from)}
              <li>
                <button
                  type="button"
                  class="md-outline-item"
                  class:current={i === currentHeading}
                  aria-current={i === currentHeading ? "location" : undefined}
                  title={item.text}
                  style:padding-left="{0.7 + (item.level - outlineBase) * 0.85}em"
                  onclick={() => jumpTo(i)}>{item.text === "" ? "(untitled)" : item.text}</button
                >
              </li>
            {/each}
          </ul>
        {/if}
      </nav>
    {/if}
  </div>
</div>

<style>
  .md-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  /* Quiet mode toggle bar, matching the pane top-bar treatment. */
  .md-bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    height: 26px;
    padding: 0 0.6rem;
    border-bottom: 1px solid var(--edge);
  }

  .toggle {
    display: flex;
    align-items: center;
    gap: 1px;
  }

  .seg {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    letter-spacing: 0.04em;
    color: var(--muted);
    cursor: pointer;
    padding: 2px 8px;
    border-radius: 4px;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .seg:hover:not(:disabled) {
    color: var(--fg);
  }

  .seg.on {
    color: var(--fg);
    background: var(--row-active);
  }

  .seg:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .md-bar-err {
    font-size: var(--text-xs);
    color: var(--err);
  }

  .md-bar-note {
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .md-bar-fill {
    flex: 1;
  }

  /* The document and, when open, the outline beside it. */
  .md-main {
    flex: 1;
    display: flex;
    min-height: 0;
  }

  .md-content {
    flex: 1;
    position: relative;
    min-width: 0;
  }

  .md-outline {
    flex: none;
    width: 15rem;
    max-width: 40%;
    overflow-y: auto;
    scrollbar-width: thin;
    border-left: 1px solid var(--edge);
    padding: 0.55rem 0;
    font-size: var(--text-sm);
  }

  .md-outline ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .md-outline-item {
    appearance: none;
    display: block;
    width: 100%;
    border: none;
    border-left: 2px solid transparent;
    background: none;
    font: inherit;
    text-align: left;
    color: var(--muted);
    padding-top: 0.2rem;
    padding-bottom: 0.2rem;
    padding-right: 0.7rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    cursor: pointer;
  }

  .md-outline-item:hover {
    color: var(--fg);
    background: var(--row-hover);
  }

  .md-outline-item.current {
    color: var(--fg);
    border-left-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }

  .md-outline-empty {
    margin: 0;
    padding: 0.2rem 0.8rem;
    color: var(--muted);
  }

  .md-scroll {
    position: absolute;
    inset: 0;
    overflow-y: auto;
    overflow-x: hidden;
    scrollbar-width: thin; /* like the transcript's own bar (chat/ChatView) */
  }
  /* The pane clips at its padding box (layout/Pane) and this scroller is
     inset:0 against it — the global outside ring (app.css) would be cut on
     three sides, so paint it inside, like GitView's .grow. */
  .md-scroll:focus-visible {
    outline-offset: -2px;
  }

  .edit-layer {
    position: absolute;
    inset: 0;
  }

  .hidden {
    display: none;
  }

  .file-error {
    padding: 2rem;
    color: var(--muted);
    font-size: var(--text-md);
    text-align: center;
  }

  /* Base font-size is set inline (per-pane text size); every size below is in
     `em` so A−/A+ scales the whole document uniformly, like the terminal. */
  .md-body {
    max-width: 70ch;
    margin: 0 auto;
    padding: 2.2rem 2rem 3.5rem;
    font-size: var(--text-lg);
    line-height: var(--markdown-line-height);
    color: var(--fg);
    /* overflow-wrap, never word-break / anywhere: those shrink a table
       column's min-content and crush numeric cells letter-per-line (chat's
       root does, and needs a per-cell reset; this one doesn't). */
    overflow-wrap: break-word;
    /* Tables: the shared recipe in app.css ("Markdown tables"), whose default
       spacing is this surface's; comrak's bare <table> is its own
       display:block scroller there. Headers wrap like any cell: one-line
       headers would only turn tables that fit into scrollers, since
       overflow-wrap above never shrinks a column's min-content. */
  }

  .md-body.after-props {
    padding-top: 1.1rem;
  }

  /* Properties (the frontmatter): a quiet key/value card on the reading
     column, sized in em off the same per-pane text size as the body. */
  /* Same box as .md-body (border-box via app.css), so the card lines up
     with the column under it. */
  .md-view :global(.md-props) {
    max-width: 70ch;
    margin: 1.6rem auto 0;
    padding: 0 2rem;
    line-height: 1.45;
  }

  .md-view :global(.md-props-head) {
    appearance: none;
    display: inline-flex;
    align-items: center;
    gap: 0.45em;
    border: none;
    background: none;
    padding: 0.15em 0.3em 0.15em 0;
    font: inherit;
    font-size: 0.78em;
    letter-spacing: 0.04em;
    color: var(--muted);
    cursor: pointer;
    border-radius: 4px;
  }

  .md-view :global(.md-props-head:hover) {
    color: var(--fg);
  }

  .md-view :global(.md-props-count) {
    font-variant-numeric: tabular-nums;
    opacity: 0.7;
  }

  .md-view :global(.md-props-list) {
    display: grid;
    grid-template-columns: minmax(6em, max-content) 1fr;
    gap: 0.3em 1.2em;
    margin: 0.45em 0 0;
    padding: 0.65em 0.9em;
    border: 1px solid var(--edge);
    border-radius: 8px;
    background: color-mix(in srgb, var(--fg) 2.5%, transparent);
    font-size: 0.86em;
  }

  .md-view :global(.md-props-list dt) {
    color: var(--muted);
    overflow-wrap: anywhere;
  }

  .md-view :global(.md-props-list dd) {
    margin: 0;
    min-width: 0;
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.3em;
    overflow-wrap: anywhere;
  }

  .md-view :global(.md-prop-text) {
    white-space: pre-wrap;
  }

  /* An empty box has no baseline of its own to align by. */
  .md-view :global(.md-props-list dd > .md-task) {
    align-self: center;
  }

  .md-view :global(.md-prop-chip) {
    padding: 0 0.5em;
    border-radius: 999px;
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 28%, var(--edge));
    font-size: 0.94em;
    line-height: 1.5;
  }

  .md-view :global(.md-prop-empty) {
    color: var(--muted);
    opacity: 0.6;
  }

  .md-view :global(.md-prop-raw),
  .md-view :global(.md-props-raw) {
    margin: 0;
    font-family: var(--mono);
    font-size: 0.9em;
    white-space: pre-wrap;
    color: var(--muted);
  }

  .md-view :global(.md-props-raw) {
    margin-top: 0.45em;
    padding: 0.65em 0.9em;
    border: 1px solid var(--edge);
    border-radius: 8px;
    font-size: 0.78em;
  }

  /* A task box (the reading render's `span.md-task`, and a boolean
     property): drawn to match live mode's native checkbox — an edge-toned
     box, an accent fill with a knocked-out check when done. */
  .md-view :global(.md-task) {
    display: inline-block;
    position: relative;
    flex: none;
    width: 0.92em;
    height: 0.92em;
    margin: 0 0.45em 0 0;
    vertical-align: -0.12em;
    box-sizing: border-box;
    border: 1.5px solid color-mix(in srgb, var(--fg) 38%, transparent);
    border-radius: 3px;
    background: var(--term-bg);
  }

  .md-view :global(.md-task[data-task="done"]) {
    background: var(--accent);
    border-color: var(--accent);
  }

  .md-view :global(.md-task[data-task="done"]::after) {
    content: "";
    position: absolute;
    left: 30%;
    top: 8%;
    width: 28%;
    height: 58%;
    border: solid var(--term-bg);
    border-width: 0 0.13em 0.13em 0;
    transform: rotate(45deg);
  }

  /* The box stands in for a bullet, hanging in the marker's place; an
     ordered task keeps its number (live does the same). */
  .md-view :global(.md-doc ul > li.md-task-item) {
    list-style: none;
  }

  .md-view :global(.md-doc ul > li.md-task-item > .md-task:first-child),
  .md-view :global(.md-doc ul > li.md-task-item > p:first-child > .md-task:first-child) {
    margin-left: -1.35em;
    margin-right: 0.43em;
  }

  .md-view :global(.md-doc .md-task-text) {
    color: var(--muted);
    text-decoration: line-through;
    text-decoration-color: color-mix(in srgb, var(--muted) 70%, transparent);
  }

  /* GitHub alerts (comrak's classes): a tinted card with a colored rule and
     a title row led by the type's glyph. Colors are semantic theme tokens,
     so every curated theme restyles them; the glyph is a mask painted in
     the title's own color. */
  .md-view :global(.md-doc .markdown-alert) {
    --md-alert: var(--syn-func);
    margin: 0.9em 0;
    padding: 0.55em 1em 0.6em;
    border-left: 3px solid color-mix(in srgb, var(--md-alert) 75%, transparent);
    border-radius: 0 8px 8px 0;
    background: color-mix(in srgb, var(--md-alert) 7%, transparent);
  }

  .md-view :global(.md-doc .markdown-alert-note) {
    --md-alert: var(--syn-func);
    --md-alert-icon: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3E%3Ccircle cx='12' cy='12' r='9'/%3E%3Cpath d='M12 8h.01M11 12h1v4h1'/%3E%3C/svg%3E");
  }

  .md-view :global(.md-doc .markdown-alert-tip) {
    --md-alert: var(--syn-string);
    --md-alert-icon: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='M3 12h1m8-9v1m8 8h1M5.6 5.6l.7.7m12.1-.7-.7.7M9 16a5 5 0 1 1 6 0a3.5 3.5 0 0 0-1 3a2 2 0 0 1-4 0a3.5 3.5 0 0 0-1-3M9.7 17h4.6'/%3E%3C/svg%3E");
  }

  .md-view :global(.md-doc .markdown-alert-important) {
    --md-alert: var(--rate);
    --md-alert-icon: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='M18 4a3 3 0 0 1 3 3v8a3 3 0 0 1-3 3h-5l-5 3v-3H6a3 3 0 0 1-3-3V7a3 3 0 0 1 3-3zM12 8v3M12 14v.01'/%3E%3C/svg%3E");
  }

  .md-view :global(.md-doc .markdown-alert-warning) {
    --md-alert: var(--warn);
    --md-alert-icon: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='M12 9v4M10.4 3.6L2.3 17.1a1.9 1.9 0 0 0 1.6 2.9h16.2a1.9 1.9 0 0 0 1.6-2.9L13.6 3.6a1.9 1.9 0 0 0-3.2 0zM12 16h.01'/%3E%3C/svg%3E");
  }

  .md-view :global(.md-doc .markdown-alert-caution) {
    --md-alert: var(--err);
    --md-alert-icon: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='M12.8 2.6l8.6 8.6a1.1 1.1 0 0 1 0 1.6l-8.6 8.6a1.1 1.1 0 0 1-1.6 0l-8.6-8.6a1.1 1.1 0 0 1 0-1.6l8.6-8.6a1.1 1.1 0 0 1 1.6 0zM12 8v4M12 16h.01'/%3E%3C/svg%3E");
  }

  .md-view :global(.md-doc .markdown-alert-title) {
    display: flex;
    align-items: center;
    gap: 0.45em;
    margin: 0 0 0.25em;
    font-weight: 600;
    font-size: 0.94em;
    color: var(--md-alert);
  }

  .md-view :global(.md-doc .markdown-alert-title::before) {
    content: "";
    flex: none;
    width: 1.05em;
    height: 1.05em;
    background: currentColor;
    -webkit-mask: var(--md-alert-icon) center / contain no-repeat;
    mask: var(--md-alert-icon) center / contain no-repeat;
  }

  .md-view :global(.md-doc .markdown-alert > :last-child) {
    margin-bottom: 0;
  }

  .md-view :global(.md-doc .markdown-alert > .markdown-alert-title + *) {
    margin-top: 0;
  }

  /* Footnotes: comrak's section, quieter than the prose it annotates. */
  .md-view :global(.md-doc section.footnotes) {
    margin-top: 2.2em;
    padding-top: 0.6em;
    border-top: 1px solid var(--edge);
    font-size: 0.88em;
    color: color-mix(in srgb, var(--fg) 75%, var(--muted));
  }

  .md-view :global(.md-doc .footnote-ref a),
  .md-view :global(.md-doc a.footnote-backref) {
    font-variant-numeric: tabular-nums;
  }

  /* Where a jump landed: a brief accent wash that fades out. */
  .md-view :global(.md-flash) {
    animation: md-flash 1.5s ease-out;
    border-radius: 4px;
  }

  @keyframes md-flash {
    0%,
    20% {
      background-color: color-mix(in srgb, var(--accent) 22%, transparent);
    }
    100% {
      background-color: transparent;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .md-view :global(.md-flash) {
      animation: none;
      outline: 2px solid color-mix(in srgb, var(--accent) 45%, transparent);
      outline-offset: 2px;
    }
  }

  /* A followed link that goes nowhere says so where the click was (both
     modes: the live editor mounts inside .md-content too). */
  .md-content :global(.md-link-hint) {
    position: absolute;
    z-index: 5;
    max-width: min(26rem, calc(100% - 16px));
    padding: 0.3rem 0.6rem;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: var(--overlay-bg);
    box-shadow: 0 4px 14px color-mix(in srgb, var(--fg) 12%, transparent);
    color: var(--muted);
    font-size: var(--text-xs);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    pointer-events: none;
    animation: md-hint-in 0.12s ease-out;
  }

  @keyframes md-hint-in {
    from {
      opacity: 0;
      transform: translateY(-3px);
    }
  }

  .md-view :global(.md-doc h1),
  .md-view :global(.md-doc h2),
  .md-view :global(.md-doc h3),
  .md-view :global(.md-doc h4),
  .md-view :global(.md-doc h5),
  .md-view :global(.md-doc h6) {
    line-height: 1.25;
    margin: 1.6em 0 0.55em;
    font-weight: 600;
    letter-spacing: -0.01em;
  }

  .md-view :global(.md-doc h1) {
    font-size: 1.576em;
    margin-top: 0.2em;
    padding-bottom: 0.35em;
    border-bottom: 1px solid var(--edge);
  }

  .md-view :global(.md-doc h2) {
    font-size: 1.25em;
    padding-bottom: 0.25em;
    border-bottom: 1px solid var(--edge);
  }

  .md-view :global(.md-doc h3) {
    font-size: 1.087em;
  }

  .md-view :global(.md-doc h4),
  .md-view :global(.md-doc h5),
  .md-view :global(.md-doc h6) {
    font-size: 1em;
  }

  .md-view :global(.md-doc p) {
    margin: 0.7em 0;
  }

  .md-view :global(.md-doc a) {
    color: var(--accent);
    text-decoration: none;
  }

  .md-view :global(.md-doc a:hover) {
    text-decoration: underline;
  }

  .md-view :global(.md-doc code) {
    font-family: var(--mono);
    font-size: 0.82em;
    background: color-mix(in srgb, var(--fg) 6%, transparent);
    border-radius: 4px;
    padding: 0.12em 0.34em;
  }

  /* The CODE child is the horizontal scroller (not the pre), so the pinned
     copy button never rides away with scrolled content. */
  .md-view :global(.md-doc pre) {
    position: relative; /* the copy button's anchor */
    background: color-mix(in srgb, var(--fg) 4.5%, transparent);
    border: 1px solid var(--edge);
    border-radius: 8px;
    padding: 0.8em 1em;
    overflow: hidden;
    line-height: 1.5;
  }

  .md-view :global(.md-doc pre code) {
    display: block;
    overflow-x: auto;
    scrollbar-width: thin;
    background: none;
    padding: 0;
    font-size: 0.848em;
  }

  /* Quoted material as a quiet card — the same treatment as the chat
     transcript: an accent→neutral wash a half-step off the page. */
  .md-view :global(.md-doc blockquote) {
    position: relative; /* the copy button's anchor */
    margin: 0.8em 0;
    padding: 0.55em 1em;
    border-left: 3px solid color-mix(in srgb, var(--accent) 60%, transparent);
    border-radius: 0 8px 8px 0;
    background: linear-gradient(
      to right,
      color-mix(in srgb, var(--accent) 5%, transparent),
      color-mix(in srgb, var(--fg) 3%, transparent) 55%
    );
    color: color-mix(in srgb, var(--fg) 45%, var(--muted));
  }

  .md-view :global(.md-doc blockquote > :first-child) {
    margin-top: 0;
  }

  .md-view :global(.md-doc blockquote > :nth-last-child(1 of :not(.md-copy))) {
    margin-bottom: 0;
  }

  /* Hover-reveal copy chrome (shared decorator; the chat transcript's
     language). Token-only scrim so both themes hold. */
  .md-view :global(.md-doc .md-copy) {
    position: absolute;
    top: 6px;
    right: 6px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 4px;
    background: color-mix(in srgb, var(--term-bg) 82%, transparent);
    border: 1px solid var(--edge);
    border-radius: 5px;
    color: var(--muted);
    cursor: pointer;
    opacity: 0;
    transition:
      opacity 0.12s ease,
      color 0.12s ease;
  }

  .md-view :global(.md-doc pre:hover .md-copy),
  .md-view :global(.md-doc blockquote:hover > .md-copy),
  .md-view :global(.md-doc .md-copy:focus-visible),
  .md-view :global(.md-doc .md-copy.copied) {
    opacity: 1;
  }

  .md-view :global(.md-doc .md-copy:hover),
  .md-view :global(.md-doc .md-copy.copied) {
    color: var(--accent);
  }

  .md-view :global(.md-doc .md-copy .ic-check),
  .md-view :global(.md-doc .md-copy.copied .ic-copy) {
    display: none;
  }

  .md-view :global(.md-doc .md-copy.copied .ic-check) {
    display: block;
  }

  /* Equations (typeset client-side into comrak's math spans; typography is
     the global .katex rule in app.css): display math scrolls within the
     reading column instead of widening the workbench — the chat's treatment. */
  .md-view :global(.md-doc .md-math) {
    color: inherit;
  }

  .md-view :global(.md-doc .md-math-display) {
    display: block;
    max-width: 100%;
    overflow-x: auto;
    overflow-y: hidden;
    scrollbar-width: thin;
    margin: 0.55em 0;
    padding: 0.1em 0;
  }

  .md-view :global(.md-doc ul),
  .md-view :global(.md-doc ol) {
    padding-left: 1.6em;
    margin: 0.6em 0;
  }

  .md-view :global(.md-doc li) {
    margin: 0.2em 0;
  }

  .md-view :global(.md-doc li)::marker {
    color: color-mix(in srgb, var(--accent) 70%, var(--muted));
  }

  .md-view :global(.md-doc hr) {
    border: none;
    border-top: 1px solid var(--edge);
    margin: 1.8em 0;
  }

  /* The width and height a resolved picture carries reserve its box before
     a byte loads; the height follows when the column caps the width. An
     embed card sizes its own. */
  .md-view :global(.md-doc img:not(.embed-card *)) {
    max-width: 100%;
    height: auto;
  }

  /* An image-syntax block: an embed card (shared/embed) inside its
     paragraph, which keeps the paragraph's spacing (live's ghosts read it
     so). */
  .md-view :global(.md-doc .md-embed) {
    display: block;
    max-width: 100%;
  }

  /* A picture reads as a picture: its card drops the frame and header and
     keeps the image body — the box reserved from the header dimensions, a
     region crop, the size hint, a click that opens it. Before the
     document's answers arrive (one round trip) it takes no room rather
     than a card's loading state; a missing one keeps the card, which says
     so. */
  .md-view :global(.md-doc .md-embed-image > .embed-card[data-embed-kind="image"]) {
    min-width: 0;
    margin: 0;
    border: none;
    border-radius: 0;
    background: none;
  }

  .md-view :global(.md-doc .md-embed-image > .embed-card[data-embed-kind="image"] > .head) {
    display: none;
  }

  .md-view :global(.md-doc .md-embed-image > .embed-card[data-embed-kind="image"] .image-body) {
    padding: 0;
    background: none;
  }

  .md-view :global(.md-doc .md-embed-image > .embed-card[data-embed-kind="pending"]:not(.missing)) {
    display: none;
  }

  /* A wikilink reads as a link with a quieter, dotted rule: it resolves by
     name (docLinks), so it may land somewhere a path link wouldn't. */
  .md-view :global(.md-doc a.wikilink) {
    text-decoration: underline dotted color-mix(in srgb, var(--accent) 55%, transparent);
    text-underline-offset: 0.18em;
  }

  /* Mermaid: the laid-out diagram, centered and never wider than the
     column (a wide one scrolls); the source shows while it lays out and,
     with the parser's message, when it can't. */
  .md-view :global(.md-doc .md-mermaid) {
    margin: 0.9em 0;
  }

  .md-view :global(.md-doc .md-mermaid-svg) {
    overflow-x: auto;
    scrollbar-width: thin;
  }

  .md-view :global(.md-doc .md-mermaid-svg svg) {
    display: block;
    max-width: 100%;
    height: auto;
    margin: 0 auto;
  }

  /* The parser's message keeps its lines: its caret points into the one
     above it. */
  /* The source shown while a diagram lays out (or fails) keeps the box's
     own margins: a bare `pre`'s would collapse through it, and the box's
     spacing would change when the diagram arrives. */
  .md-view :global(.md-doc .md-mermaid > pre) {
    margin: 0;
  }

  .md-view :global(.md-doc .md-mermaid-note) {
    margin: 0 0 0.4em;
    font-family: var(--mono);
    font-size: 0.76em;
    line-height: 1.45;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    color: var(--err);
  }

  .md-view :global(.md-doc input[type="checkbox"]) {
    accent-color: var(--accent);
    margin-right: 0.4em;
  }
</style>
