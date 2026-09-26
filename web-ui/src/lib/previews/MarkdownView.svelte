<script module lang="ts">
  import { createModeMemory } from "./mdDoc";

  /** The mode each file was last shown in: a per-viewer convenience in this
   *  browser's storage (never shared, never read back by the daemon). */
  const modeMemory = createModeMemory(() => localStorage);
  const PROPS_KEY = "chimaera.markdownPropsCollapsed";
  function readPropsCollapsed(): boolean {
    try {
      return localStorage.getItem(PROPS_KEY) === "1";
    } catch {
      return false;
    }
  }
</script>

<script lang="ts">
  /**
   * Markdown with Obsidian-style modes: live | reading | source.
   *
   * READING is the complete, non-editable render — the authoritative
   * server-side comrak GFM (sanitized; `$`/`$$` math arrives as LaTeX
   * literals this view typesets), which refreshes from disk on save or an
   * agent write. Its frontmatter shows as a properties panel, its links open
   * in the workbench (docLinks.ts), and every block carries its source lines
   * (`data-sourcepos`), so a selection references real line numbers and a
   * reveal lands on the right block. LIVE is an editable reading view — the
   * shared CodeMirror editor with the mdLive decoration set rendering
   * formatting inline (marks hidden off the selection's lines,
   * images/checkboxes/rules/equations as widgets). SOURCE is the same editor
   * as plain raw markdown. Live and source share ONE editor instance (an
   * extension swap, never a remount), and the editor mounts once and survives
   * every toggle, so flipping modes never drops an unsaved buffer or its undo
   * history. Saves, the dirty dot, and conflict handling all come from
   * CodeView (Cmd/Ctrl+S).
   * A file opens in the mode it was last shown in, else the
   * `editor.markdownDefaultMode` setting (reading by default). Editing is
   * offered only for files under the 1MB cap; larger markdown opens in
   * reading and stays there.
  */
  import { untrack, type Component } from "svelte";
  import type { Extension } from "@codemirror/state";
  import {
    EDIT_MAX_BYTES,
    looksBinary,
    rawTicketUrl,
    resolveDocPath,
    safeDecodeUri,
    type FileChunk,
  } from "./files";
  import { retain, release, type FileEntry } from "./fileStore.svelte";
  import { clearSelection, setSelection } from "../shared/reference";
  import { getSetting } from "../settings/store.svelte";
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
  import { loadMath, mathNow } from "./mathLoad";
  import { readingWindow } from "./readingWindow";
  import {
    frontmatterLineSpan,
    isMdMode,
    parseFrontmatter,
    parseSourcepos,
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

  /** Read at click time, so the memoized live extension set never has to
   *  change when the workspace does. */
  const linkContext = (): LinkContext => ({ wsRoot, workspaceId: null });

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
      : [liveMod.markdownLanguageExt, liveMod.markdownLive(path, linkContext)],
  );
  const sourceSet = $derived(liveMod === null ? null : [liveMod.markdownLanguageExt]);
  let editorMode = $state<"live" | "source">("live");
  const extra = $derived.by(
    (): Extension => (editorMode === "live" ? liveSet : sourceSet) ?? [],
  );

  // The shared store entry: reading HTML lives here (cached across tab
  // switches, and re-rendered in place when the file changes on disk — a save
  // in the editor, or an agent write, both flow through the store).
  let entry = $state<FileEntry | null>(null);
  const html = $derived(entry?.markdown?.html ?? null);
  const error = $derived(entry?.markdownError ?? null);
  /** The leading YAML block, lifted out of the render by the daemon (null on
   *  a document without one, and on an older daemon that renders it inline). */
  const frontmatter = $derived(entry?.markdown?.frontmatter ?? null);
  const fmEntries = $derived(frontmatter === null ? null : parseFrontmatter(frontmatter));
  /** The panel stands in for the block's lines, fences included. */
  const fmSourcepos = $derived(
    frontmatter === null ? null : `1:1-${frontmatterLineSpan(frontmatter)}:3`,
  );
  let propsCollapsed = $state(readPropsCollapsed());
  function toggleProps(): void {
    propsCollapsed = !propsCollapsed;
    try {
      localStorage.setItem(PROPS_KEY, propsCollapsed ? "1" : "0");
    } catch {
      // storage unavailable: the choice lasts for this view
    }
  }

  // Reset per path — BEFORE the retain effect in source order, so a path swap
  // resets the view before the new entry is opened. The opening mode is the
  // one this file was last shown in, else the setting — read untracked, so a
  // settings change never resets an open document.
  $effect(() => {
    void path;
    const initial = untrack(() => {
      const setting = getSetting("editor.markdownDefaultMode");
      return modeMemory.get(path) ?? (isMdMode(setting) ? setting : "reading");
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
  });

  // Retain + open the chosen mode. `path` is the only tracked dependency —
  // the store's retain()/ensure* guards are untracked by design (and
  // openDefault's read of `mode` is untracked here), so an in-place payload
  // refresh (a save, an agent write) or a mode click can never re-run this
  // effect and remount the editor over a dirty buffer.
  $effect(() => {
    void path;
    const e = retain(path);
    entry = e;
    untrack(() => void openDefault(e));
    return () => release(path);
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

  // The server render is fetched on the first reading entry (not eagerly —
  // the editor modes only need the raw source). Once populated, the store
  // refreshes it in place on every disk change or in-app save.
  $effect(() => {
    if (mode !== "reading") return;
    void entry?.ensureMarkdown();
  });

  async function enterEditor(target: "live" | "source"): Promise<void> {
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
    }
    entered = true;
    mode = target;
    editorMode = target;
    modeMemory.set(path, target);
  }

  function setMode(m: Mode): void {
    modeReq++;
    barNote = null;
    if (m === "reading") {
      mode = "reading";
      modeMemory.set(path, "reading");
    } else {
      void enterEditor(m);
    }
  }

  // --- context bridge: selection in the RENDERED reading view ---------------
  // Every block of the render carries its source lines (`data-sourcepos`),
  // so a reference names the lines the selection's two ends sit in; a
  // render without them (an older daemon) sends the quoted excerpt alone.
  const selOwner = {};
  let contentEl = $state<HTMLDivElement | null>(null);

  // Copy chrome on fenced blocks + blockquotes (the same affordance as the
  // chat transcript, via the shared decorator), document-relative image
  // resolution, and task-item marks. Scoped to the rendered article (never
  // the editor subtree, nor the Svelte-owned properties panel) and gated on
  // reading being shown — a hidden render pane skips the DOM walk and
  // catches up when reading is next entered (mode is a dependency).
  let readingEl = $state<HTMLDivElement | null>(null);
  let articleEl = $state<HTMLElement | null>(null);
  $effect(() => {
    void html;
    if (mode !== "reading") return;
    const content = articleEl;
    if (content === null) return;
    decorateCopyTargets(content);
    stampImages(content);
    markTasks(content);
    typesetMath(content);
    return cancelTypeset;
  });

  /** Task items (`- [x]`) arrive as `span.md-task[data-task]` at the head of
   *  their item. The item drops its bullet (the box stands in for it, as in
   *  live), and a done item's own text — up to its first nested block, so a
   *  sub-list keeps its ink — is wrapped to read muted and struck through,
   *  matching live's `lp-task-done`. A CSS `:has()` would do the first half,
   *  but its invalidation cost on a long document in WebKit is exactly the
   *  restyle churn the pane parking fights; a class set once is free.
   *  Idempotent: a fresh server render brings fresh spans. */
  const BLOCK_TAGS = new Set(["UL", "OL", "P", "DIV", "PRE", "BLOCKQUOTE", "TABLE"]);
  function markTasks(root: HTMLElement): void {
    for (const box of root.querySelectorAll<HTMLElement>("span.md-task[data-task]")) {
      const item = box.closest("li");
      if (item !== null && !item.classList.contains("md-task-item")) {
        item.classList.add("md-task-item");
      }
      if (box.dataset.task !== "done") continue;
      if (box.nextElementSibling?.classList.contains("md-task-text")) continue;
      const wrap = document.createElement("span");
      wrap.className = "md-task-text";
      let n = box.nextSibling;
      while (n !== null && !(n instanceof HTMLElement && BLOCK_TAGS.has(n.tagName))) {
        const next = n.nextSibling;
        wrap.append(n);
        n = next;
      }
      // The space after the box stays outside, or the strike starts on it.
      const head = wrap.firstChild;
      const lead = head instanceof Text ? /^\s+/.exec(head.data) : null;
      if (head instanceof Text && lead !== null) head.data = head.data.slice(lead[0].length);
      box.after(wrap);
      if (lead !== null) box.after(document.createTextNode(lead[0]));
    }
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
    if (mode !== "reading" || html === null || readingEl === null) return;
    const req = takeReveal(path);
    if (req !== null) afterLayout(() => revealInReading(req));
  });

  // A link from another document (`this.md#heading`) left its anchor
  // pending for this path. Reading scrolls to the element; the editor
  // modes map it to a line through the render and reveal that.
  $effect(() => {
    void $anchorRequests;
    if (mode === "reading") {
      if (html === null || readingEl === null) return;
      const anchor = takeAnchor(path);
      if (anchor !== null) afterLayout(() => void toAnchorInReading(anchor));
    } else {
      const anchor = takeAnchor(path);
      if (anchor !== null) void revealAnchorInSource(path, anchor);
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

  /** Equations in a rendered document. The server emits each one — inline
   *  `$…$`/`$$…$$`, a `$$` block, a ```math fence — as an escaped LaTeX
   *  literal in `span[data-math-style]` (the one non-default attribute the
   *  sanitizer keeps; blocks are promoted to comrak's math fence and its
   *  `<pre><code>` rewritten to the same span, so this is the ONE seam) and the
   *  client typesets it under the shared KaTeX policy (`shared/math`,
   *  loaded on demand at the first equation, memoized). The pass is
   *  idempotent (a typeset span carries `.md-math`; a fresh server render
   *  brings fresh spans) and time-sliced: lecture notes with thousands of
   *  equations typeset their first screen synchronously and the rest at
   *  idle, so a refresh mid agent-rewrite can't stall the workbench. */
  type MathModule = typeof import("../shared/math");
  let typesetJob: { cancelled: boolean; handle: number | null; idle: boolean } | null = null;

  function cancelTypeset(): void {
    const job = typesetJob;
    if (job === null) return;
    job.cancelled = true;
    if (job.handle !== null) {
      if (job.idle) cancelIdleCallback(job.handle);
      else clearTimeout(job.handle);
    }
    typesetJob = null;
  }

  /** Past this, a "source" is not an equation but a document — an unclosed
   *  ```math fence runs to the end of the file by CommonMark's rules — and
   *  one KaTeX job over it would stall the workbench for seconds. It stays
   *  readable text (live shows the same shape as mono source). */
  const MAX_MATH_SOURCE = 16 * 1024;

  function typesetSpan(span: HTMLElement, math: MathModule): void {
    if (!span.isConnected || span.classList.contains("md-math")) return;
    const display = span.dataset.mathStyle === "display";
    const source = span.textContent ?? "";
    span.classList.add("md-math");
    // `$$ $$` has nothing to typeset, as in live.
    if (source.trim().length === 0 || source.length > MAX_MATH_SOURCE) return;
    if (display) span.classList.add("md-math-display");
    span.innerHTML = math.safeMathHtml(source, display);
  }

  function typesetMath(root: HTMLElement): void {
    cancelTypeset();
    const spans = Array.from(
      root.querySelectorAll<HTMLElement>("span[data-math-style]:not(.md-math)"),
    );
    if (spans.length === 0) return;
    const job = { cancelled: false, handle: null as number | null, idle: false };
    typesetJob = job;
    let i = 0;
    const slice = (math: MathModule): void => {
      if (job.cancelled) return;
      job.handle = null;
      const deadline = performance.now() + 8;
      while (i < spans.length && performance.now() < deadline) typesetSpan(spans[i++], math);
      // The equations this slice just laid out: a wide one is a scroller
      // that didn't exist when the reading view was marked, and no box the
      // width watcher sees changes when it appears.
      markScrollRegions(root, [[".md-math-display", null]]);
      if (i >= spans.length) {
        typesetJob = null;
        return;
      }
      // WKWebView (the native app) has no requestIdleCallback: a short
      // timeout stands in — the chat's path-stamping fallback.
      if (typeof requestIdleCallback === "function") {
        job.idle = true;
        job.handle = requestIdleCallback(() => slice(math), { timeout: 500 });
      } else {
        job.idle = false;
        job.handle = window.setTimeout(() => slice(math), 16);
      }
    };
    const math = mathNow();
    if (math !== null) slice(math);
    else
      void loadMath().then(slice, () => {
        // KaTeX failed to load: the LaTeX literals stay readable as text.
      });
  }

  /** `![](figs/plot.png)` in a document: the rendered src is relative, which
   *  the browser would resolve against the APP origin (a guaranteed 404).
   *  Re-point each such image at a short-lived ticketed /raw/ URL for the
   *  path relative to the file — the same mechanism as inline chat previews;
   *  `rawTicketUrl` memoizes so re-renders keep the src stable (no flash).
   *  Web/data URLs pass through untouched. */
  function stampImages(root: HTMLElement): void {
    for (const img of root.querySelectorAll("img")) {
      const src = img.getAttribute("src") ?? "";
      if (src === "" || hasUrlScheme(src) || src.startsWith("/raw/")) continue;
      if (img.dataset.mdSrc === src) continue;
      img.dataset.mdSrc = src;
      const target = resolveDocPath(path, safeDecodeUri(src));
      rawTicketUrl(target).then(
        (url) => {
          if (img.isConnected && img.dataset.mdSrc === src) img.src = url;
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
    const copyBtn = (e.target as Element | null)?.closest?.("button.md-copy");
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
    if (e.button !== 1) return;
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
    void followDocHref(href, split, {
      docPath: path,
      ...linkContext(),
      toAnchor: toAnchorInReading,
      toLines: revealInReading,
      hint: (text) => {
        if (contentEl !== null) showLinkHint(contentEl, x, y, text);
      },
    });
  }

  function onLinkContextMenu(e: MouseEvent): void {
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
    setSelection(selOwner, {
      kind: "file",
      path,
      startLine: lines?.start ?? null,
      endLine: lines?.end ?? null,
      text,
    });
    chipPos = chipPosFor(content, range);
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

</script>

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
    <DocIssues {path} {wsRoot} mtime={entry?.mtime ?? null} />
  </div>

  <!-- Delegated link handling: the interactive targets are the rendered
       document's own <a> elements, which are already focusable and fire a
       native click on Enter that bubbles here — so keyboard access needs no
       separate handler on the container. -->
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

    <!-- Authoritative server render (comrak). Shown in reading mode; kept in
         the DOM (just hidden) so re-entering reading needs no re-render.
         Focusable so keyboard scrolling works in WKWebView (Safari never
         auto-focuses scrollers), named after the file for the landmark list. -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div
      class="md-scroll"
      class:hidden={mode !== "reading"}
      role="region"
      aria-label={fileLabel}
      tabindex="0"
      bind:this={readingEl}
    >
      {#if error !== null}
        <div class="file-error">{error}</div>
      {:else if html !== null}
        {#if frontmatter !== null}
          <!-- The frontmatter as properties: a sibling of the article, never
               inside it (the raw-HTML range owns the article's first and
               last nodes — readingWindow's boundary rule). Every value is
               text-interpolated; the file's YAML is never markup. -->
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
                <pre class="md-props-raw">{stripFences(frontmatter)}</pre>
              {/if}
            {/if}
          </section>
        {/if}
        <article
          class="md-body"
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
         CSS-hidden in reading, so no toggle drops the buffer. The prose size
         rides CSS variables so an A−/A+ resize never reconfigures the editor
         (the live theme is static — see mdLive). -->
    {#if entered && chunk !== null}
      {@const first = chunk}
      <div
        class="edit-layer"
        class:hidden={mode === "reading"}
        style:--lp-font-size="{bodyFont}px"
        style:--lp-line-height={bodyLineHeight}
      >
        {#if CodeView !== null}
          <CodeView
            {path}
            {first}
            {extra}
            autoLanguage={false}
            acceptReveal={mode !== "reading"}
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

  .md-content {
    flex: 1;
    position: relative;
    min-height: 0;
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
  .md-props {
    max-width: 70ch;
    margin: 1.6rem auto 0;
    padding: 0 2rem;
    line-height: 1.45;
  }

  .md-props-head {
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

  .md-props-head:hover {
    color: var(--fg);
  }

  .md-props-count {
    font-variant-numeric: tabular-nums;
    opacity: 0.7;
  }

  .md-props-list {
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

  .md-props-list dt {
    color: var(--muted);
    overflow-wrap: anywhere;
  }

  .md-props-list dd {
    margin: 0;
    min-width: 0;
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.3em;
    overflow-wrap: anywhere;
  }

  .md-prop-text {
    white-space: pre-wrap;
  }

  /* An empty box has no baseline of its own to align by. */
  .md-props-list dd > :global(.md-task) {
    align-self: center;
  }

  .md-prop-chip {
    padding: 0 0.5em;
    border-radius: 999px;
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 28%, var(--edge));
    font-size: 0.94em;
    line-height: 1.5;
  }

  .md-prop-empty {
    color: var(--muted);
    opacity: 0.6;
  }

  .md-prop-raw,
  .md-props-raw {
    margin: 0;
    font-family: var(--mono);
    font-size: 0.9em;
    white-space: pre-wrap;
    color: var(--muted);
  }

  .md-props-raw {
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
  .md-body :global(ul > li.md-task-item) {
    list-style: none;
  }

  .md-body :global(ul > li.md-task-item > .md-task:first-child),
  .md-body :global(ul > li.md-task-item > p:first-child > .md-task:first-child) {
    margin-left: -1.35em;
    margin-right: 0.43em;
  }

  .md-body :global(.md-task-text) {
    color: var(--muted);
    text-decoration: line-through;
    text-decoration-color: color-mix(in srgb, var(--muted) 70%, transparent);
  }

  /* GitHub alerts (comrak's classes): a tinted card with a colored rule and
     a title row led by the type's glyph. Colors are semantic theme tokens,
     so every curated theme restyles them; the glyph is a mask painted in
     the title's own color. */
  .md-body :global(.markdown-alert) {
    --md-alert: var(--syn-func);
    margin: 0.9em 0;
    padding: 0.55em 1em 0.6em;
    border-left: 3px solid color-mix(in srgb, var(--md-alert) 75%, transparent);
    border-radius: 0 8px 8px 0;
    background: color-mix(in srgb, var(--md-alert) 7%, transparent);
  }

  .md-body :global(.markdown-alert-note) {
    --md-alert: var(--syn-func);
    --md-alert-icon: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3E%3Ccircle cx='12' cy='12' r='9'/%3E%3Cpath d='M12 8h.01M11 12h1v4h1'/%3E%3C/svg%3E");
  }

  .md-body :global(.markdown-alert-tip) {
    --md-alert: var(--syn-string);
    --md-alert-icon: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='M3 12h1m8-9v1m8 8h1M5.6 5.6l.7.7m12.1-.7-.7.7M9 16a5 5 0 1 1 6 0a3.5 3.5 0 0 0-1 3a2 2 0 0 1-4 0a3.5 3.5 0 0 0-1-3M9.7 17h4.6'/%3E%3C/svg%3E");
  }

  .md-body :global(.markdown-alert-important) {
    --md-alert: var(--rate);
    --md-alert-icon: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='M18 4a3 3 0 0 1 3 3v8a3 3 0 0 1-3 3h-5l-5 3v-3H6a3 3 0 0 1-3-3V7a3 3 0 0 1 3-3zM12 8v3M12 14v.01'/%3E%3C/svg%3E");
  }

  .md-body :global(.markdown-alert-warning) {
    --md-alert: var(--warn);
    --md-alert-icon: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='M12 9v4M10.4 3.6L2.3 17.1a1.9 1.9 0 0 0 1.6 2.9h16.2a1.9 1.9 0 0 0 1.6-2.9L13.6 3.6a1.9 1.9 0 0 0-3.2 0zM12 16h.01'/%3E%3C/svg%3E");
  }

  .md-body :global(.markdown-alert-caution) {
    --md-alert: var(--err);
    --md-alert-icon: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='M12.8 2.6l8.6 8.6a1.1 1.1 0 0 1 0 1.6l-8.6 8.6a1.1 1.1 0 0 1-1.6 0l-8.6-8.6a1.1 1.1 0 0 1 0-1.6l8.6-8.6a1.1 1.1 0 0 1 1.6 0zM12 8v4M12 16h.01'/%3E%3C/svg%3E");
  }

  .md-body :global(.markdown-alert-title) {
    display: flex;
    align-items: center;
    gap: 0.45em;
    margin: 0 0 0.25em;
    font-weight: 600;
    font-size: 0.94em;
    color: var(--md-alert);
  }

  .md-body :global(.markdown-alert-title::before) {
    content: "";
    flex: none;
    width: 1.05em;
    height: 1.05em;
    background: currentColor;
    -webkit-mask: var(--md-alert-icon) center / contain no-repeat;
    mask: var(--md-alert-icon) center / contain no-repeat;
  }

  .md-body :global(.markdown-alert > :last-child) {
    margin-bottom: 0;
  }

  .md-body :global(.markdown-alert > .markdown-alert-title + *) {
    margin-top: 0;
  }

  /* Footnotes: comrak's section, quieter than the prose it annotates. */
  .md-body :global(section.footnotes) {
    margin-top: 2.2em;
    padding-top: 0.6em;
    border-top: 1px solid var(--edge);
    font-size: 0.88em;
    color: color-mix(in srgb, var(--fg) 75%, var(--muted));
  }

  .md-body :global(.footnote-ref a),
  .md-body :global(a.footnote-backref) {
    font-variant-numeric: tabular-nums;
  }

  /* Where a jump landed: a brief accent wash that fades out. */
  .md-scroll :global(.md-flash) {
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
    .md-scroll :global(.md-flash) {
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

  .md-body :global(h1),
  .md-body :global(h2),
  .md-body :global(h3),
  .md-body :global(h4),
  .md-body :global(h5),
  .md-body :global(h6) {
    line-height: 1.25;
    margin: 1.6em 0 0.55em;
    font-weight: 600;
    letter-spacing: -0.01em;
  }

  .md-body :global(h1) {
    font-size: 1.576em;
    margin-top: 0.2em;
    padding-bottom: 0.35em;
    border-bottom: 1px solid var(--edge);
  }

  .md-body :global(h2) {
    font-size: 1.25em;
    padding-bottom: 0.25em;
    border-bottom: 1px solid var(--edge);
  }

  .md-body :global(h3) {
    font-size: 1.087em;
  }

  .md-body :global(h4),
  .md-body :global(h5),
  .md-body :global(h6) {
    font-size: 1em;
  }

  .md-body :global(p) {
    margin: 0.7em 0;
  }

  .md-body :global(a) {
    color: var(--accent);
    text-decoration: none;
  }

  .md-body :global(a:hover) {
    text-decoration: underline;
  }

  .md-body :global(code) {
    font-family: var(--mono);
    font-size: 0.82em;
    background: color-mix(in srgb, var(--fg) 6%, transparent);
    border-radius: 4px;
    padding: 0.12em 0.34em;
  }

  /* The CODE child is the horizontal scroller (not the pre), so the pinned
     copy button never rides away with scrolled content. */
  .md-body :global(pre) {
    position: relative; /* the copy button's anchor */
    background: color-mix(in srgb, var(--fg) 4.5%, transparent);
    border: 1px solid var(--edge);
    border-radius: 8px;
    padding: 0.8em 1em;
    overflow: hidden;
    line-height: 1.5;
  }

  .md-body :global(pre code) {
    display: block;
    overflow-x: auto;
    scrollbar-width: thin;
    background: none;
    padding: 0;
    font-size: 0.848em;
  }

  /* Quoted material as a quiet card — the same treatment as the chat
     transcript: an accent→neutral wash a half-step off the page. */
  .md-body :global(blockquote) {
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

  .md-body :global(blockquote > :first-child) {
    margin-top: 0;
  }

  .md-body :global(blockquote > :nth-last-child(1 of :not(.md-copy))) {
    margin-bottom: 0;
  }

  /* Hover-reveal copy chrome (shared decorator; the chat transcript's
     language). Token-only scrim so both themes hold. */
  .md-body :global(.md-copy) {
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

  .md-body :global(pre:hover .md-copy),
  .md-body :global(blockquote:hover > .md-copy),
  .md-body :global(.md-copy:focus-visible),
  .md-body :global(.md-copy.copied) {
    opacity: 1;
  }

  .md-body :global(.md-copy:hover),
  .md-body :global(.md-copy.copied) {
    color: var(--accent);
  }

  .md-body :global(.md-copy .ic-check),
  .md-body :global(.md-copy.copied .ic-copy) {
    display: none;
  }

  .md-body :global(.md-copy.copied .ic-check) {
    display: block;
  }

  /* Equations (typeset client-side into comrak's math spans; typography is
     the global .katex rule in app.css): display math scrolls within the
     reading column instead of widening the workbench — the chat's treatment. */
  .md-body :global(.md-math) {
    color: inherit;
  }

  .md-body :global(.md-math-display) {
    display: block;
    max-width: 100%;
    overflow-x: auto;
    overflow-y: hidden;
    scrollbar-width: thin;
    margin: 0.55em 0;
    padding: 0.1em 0;
  }

  .md-body :global(ul),
  .md-body :global(ol) {
    padding-left: 1.6em;
    margin: 0.6em 0;
  }

  .md-body :global(li) {
    margin: 0.2em 0;
  }

  .md-body :global(li)::marker {
    color: color-mix(in srgb, var(--accent) 70%, var(--muted));
  }

  .md-body :global(hr) {
    border: none;
    border-top: 1px solid var(--edge);
    margin: 1.8em 0;
  }

  .md-body :global(img) {
    max-width: 100%;
  }

  .md-body :global(input[type="checkbox"]) {
    accent-color: var(--accent);
    margin-right: 0.4em;
  }
</style>
