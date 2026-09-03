<script lang="ts">
  /**
   * Markdown with Obsidian-style modes: live | reading | source.
   *
   * LIVE (the default) is an editable reading view — the shared CodeMirror
   * editor with the mdLive decoration set rendering formatting inline (marks
   * hidden off the selection's lines, images/checkboxes/rules/equations as
   * widgets). READING is the complete, non-editable render — the
   * authoritative server-side comrak GFM (sanitized; `$`/`$$` math arrives as
   * LaTeX literals this view typesets), which refreshes from disk on save or
   * an agent write. SOURCE is the same editor as plain raw markdown. Live and
   * source share ONE editor instance (an extension swap, never a remount), and
   * the editor mounts once and survives every toggle, so flipping modes never
   * drops an unsaved buffer or its undo history. Saves, the dirty dot, and
   * conflict handling all come from CodeView (Cmd/Ctrl+S).
   * Editing is offered only for files under the 1MB cap; larger markdown
   * opens straight into reading and stays there.
  */
  import type { Component } from "svelte";
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
  import ReferenceChip from "../shared/ReferenceChip.svelte";
  import Spinner from "./Spinner.svelte";
  import { activateUrl, hasUrlScheme, isWebUrl, urlMenuEntries } from "../shared/urlOpen";
  import { contextMenu } from "../shared/contextMenu.svelte";
  import { loadMath, mathNow } from "./mathLoad";

  interface Props {
    path: string;
    /** Per-pane text-size override (px); the preview body scales to it. The
     *  A−/A+ pane controls and the Cmd/Ctrl +/− chords both drive this. */
    fontSize?: number;
  }

  let { path, fontSize = undefined }: Props = $props();

  // Prose base size: the pane override, else the Markdown preference. Drives
  // the reading body AND the live editor, so the two views read identically.
  const bodyFont = $derived(fontSize ?? getSetting("editor.markdownFontSize"));
  const bodyLineHeight = $derived(getSetting("editor.markdownLineHeight"));

  type Mode = "live" | "reading" | "source";
  let mode = $state<Mode>("live");
  let chunk = $state<FileChunk | null>(null);
  let chunkError = $state<string | null>(null);
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
    Component<{ path: string; first: FileChunk; extra?: Extension; autoLanguage?: boolean }> | null
  >(null);
  let liveMod = $state<typeof import("./mdLive") | null>(null);
  let codeLoadError = $state<string | null>(null);
  // Loaded eagerly on mount (not gated on entering an editor mode) so the
  // default live open doesn't serialize the bundle import behind the chunk
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
    liveMod === null ? null : [liveMod.markdownLanguageExt, liveMod.markdownLive(path)],
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
  const html = $derived(entry?.markdown ?? null);
  const error = $derived(entry?.markdownError ?? null);

  // Reset per path — BEFORE the retain effect in source order, so a path swap
  // resets the view before the new entry is opened.
  $effect(() => {
    void path;
    mode = "live";
    editorMode = "live";
    modeReq++;
    chunk = null;
    chunkError = null;
    srcSize = null;
    srcBinary = false;
    entered = false;
    codeLoadError = null;
  });

  // Retain + open the default mode: live when the source is editable (text
  // under the cap), reading otherwise. `path` is the only tracked dependency
  // — the store's retain()/ensure* guards are untracked by design, so an
  // in-place payload refresh (a save, an agent write) can never re-run this
  // effect and remount the editor over a dirty buffer.
  $effect(() => {
    void path;
    const e = retain(path);
    entry = e;
    void openDefault(e);
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
  // the default live mode only needs the raw source). Once populated, the
  // store refreshes it in place on every disk change or in-app save.
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
      if (r === "unusable") return; // binary / too large; stay in reading
    }
    entered = true;
    mode = target;
    editorMode = target;
  }

  function setMode(m: Mode): void {
    modeReq++;
    if (m === "reading") mode = "reading";
    else void enterEditor(m);
  }

  // --- context bridge: selection in the RENDERED reading view ---------------
  // No line mapping exists for rendered markdown, so the reference carries
  // the path + quoted excerpt only (live/source go through CodeView, which
  // has real line numbers).
  const selOwner = {};
  let contentEl = $state<HTMLDivElement | null>(null);

  /**
   * Links in a rendered document. Nothing set a `target` here, so a click was
   * a TOP-LEVEL navigation: in a browser that replaces the whole workbench,
   * and in the native app the shell's navigation guard swallows it. Route it
   * instead — a live local app (loopback / explicit port) opens in a browser
   * pane, anything else in the user's real browser. Delegated on `.md-content`
   * so it covers the reading render (the editor consumes its own clicks).
   */
  // Copy chrome on fenced blocks + blockquotes (the same affordance as the
  // chat transcript, via the shared decorator), plus document-relative image
  // resolution. Scoped to the reading scroll (never the editor subtree) and
  // gated on reading being shown — a hidden render pane skips the DOM walk
  // and catches up when reading is next entered (mode is a dependency).
  let readingEl = $state<HTMLDivElement | null>(null);
  $effect(() => {
    void html;
    if (mode !== "reading") return;
    const content = readingEl;
    if (content === null) return;
    decorateCopyTargets(content);
    stampImages(content);
    typesetMath(content);
    return cancelTypeset;
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
    const anchor = (e.target as Element | null)?.closest?.("a[href]");
    const href = anchor?.getAttribute("href") ?? "";
    if (anchor === null || anchor === undefined) return;
    // Same-document anchors (a heading TOC) keep their native behavior.
    if (href.startsWith("#")) return;
    // `mailto:`/`tel:` are the browser's to handle — the OS knows what to do
    // with them and swallowing the click would just make the link look dead.
    // They cannot navigate the workbench away, so letting them through is safe.
    if (/^(mailto|tel):/i.test(href)) return;
    e.preventDefault();
    if (isWebUrl(href)) activateUrl(href, e.metaKey || e.ctrlKey);
    // A relative/in-repo href resolves against no meaningful base here, so it
    // is swallowed rather than allowed to navigate the workbench to a 404.
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
    setSelection(selOwner, { kind: "file", path, startLine: null, endLine: null, text });
    const rects = range.getClientRects();
    const last = rects.length > 0 ? rects[rects.length - 1] : range.getBoundingClientRect();
    const rect = content.getBoundingClientRect();
    const clamp = (n: number, lo: number, hi: number) => Math.min(Math.max(n, lo), Math.max(lo, hi));
    chipPos = {
      x: clamp(last.right - rect.left + 4, 4, rect.width - 170),
      y: clamp(last.bottom - rect.top + 6, 4, rect.height - 58),
    };
  }

  $effect(() => {
    if (mode !== "reading") {
      chipPos = null;
      clearSelection(selOwner);
      return;
    }
    document.addEventListener("selectionchange", syncPreviewSelection);
    return () => {
      document.removeEventListener("selectionchange", syncPreviewSelection);
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
    {#if chunkError !== null}<span class="md-bar-err">{chunkError}</span>{/if}
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
    oncontextmenu={onLinkContextMenu}
  >
    {#if mode === "reading" && chipPos !== null}
      <ReferenceChip x={chipPos.x} y={chipPos.y} />
    {/if}

    <!-- Authoritative server render (comrak). Shown in reading mode; kept in
         the DOM (just hidden) so re-entering reading needs no re-render. -->
    <div
      class="md-scroll"
      class:hidden={mode !== "reading"}
      bind:this={readingEl}
      onscroll={syncPreviewSelection}
    >
      {#if error !== null}
        <div class="file-error">{error}</div>
      {:else if html !== null}
        <article class="md-body" style:font-size="{bodyFont}px">
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
          <CodeView {path} {first} {extra} autoLanguage={false} />
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
    overflow-wrap: break-word;
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

  .md-body :global(table) {
    border-collapse: collapse;
    margin: 1em 0;
    display: block;
    overflow-x: auto;
    font-size: 0.924em;
  }

  .md-body :global(th),
  .md-body :global(td) {
    border: 1px solid var(--edge);
    padding: 0.35em 0.7em;
    text-align: left;
  }

  .md-body :global(th) {
    font-weight: 600;
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }

  .md-body :global(input[type="checkbox"]) {
    accent-color: var(--accent);
    margin-right: 0.4em;
  }
</style>
