<script lang="ts">
  /**
   * CodeMirror 6 view onto a file's buffer (`buffers.svelte.ts`). The buffer —
   * text, undo history, cursor, dirty/conflict/save state — lives in the store
   * and outlives this component, so unmounting (close, keep-alive eviction,
   * split, zoom, a drag to another pane, a workspace switch) never loses an
   * edit; a remount re-attaches to the same state. This view owns only what is
   * about display: theme/settings, the host's `extra` extensions, the language
   * pack, the selection reference chip, reveal-at-line, and the bars that
   * surface the buffer's state (conflict, merge notice, recovered draft,
   * another window's edits, save status).
   *
   * Editable for whole text files under the 1MB cap; view-only (with a
   * "load more" tail past the cap) otherwise. Cmd/Ctrl+S saves; Mod-f searches.
   * The editor instance is plain module-ish state, never $state (same rule as
   * xterm instances in termPool). `path` is fixed for this instance's life —
   * every host remounts per path.
   */
  import { onMount, untrack, type Component } from "svelte";
  import { Compartment, EditorState, type Extension } from "@codemirror/state";
  import { EditorView, lineNumbers, highlightSpecialChars, drawSelection } from "@codemirror/view";
  import {
    LanguageDescription,
    syntaxHighlighting,
    bracketMatching,
    indentUnit,
  } from "@codemirror/language";
  import { languages } from "@codemirror/language-data";
  import {
    codeHighlight as highlight,
    makeCodeTheme as makeTheme,
    codeSearchTheme,
    revealFlash,
    revealLines,
    clearRevealFlash,
    REVEAL_FLASH_MS,
  } from "./cm";
  import { basename, humanSize, type FileChunk } from "./files";
  import { openBuffer, releaseBuffer, presence } from "./buffers.svelte";
  import { getSetting } from "../settings/store.svelte";
  import { isMac } from "../shared/keys";
  import { clearSelection, setSelection } from "../shared/reference";
  import { revealRequest, takeReveal } from "../shared/reveal";
  import ReferenceChip from "../shared/ReferenceChip.svelte";

  const SAVE_HINT = isMac ? "⌘S to save" : "Ctrl+S to save";

  interface Props {
    path: string;
    /** First chunk, already fetched (and sniffed as text) by the host. Seeds
     *  a NEW buffer only; an existing buffer for this path is re-attached. */
    first: FileChunk;
    /** Live buffer sink: called with the current editor text on mount and on
     *  every change (a keystroke, a load, a reload, a merge). The split
     *  edit|preview shell uses it to re-render the preview as you type — the
     *  file is still only written on save. */
    onDoc?: (text: string) => void;
    /** Host-supplied surface extensions, swapped live in their own compartment
     *  (the markdown live preview rides here). Reconfigured only when the
     *  reference changes, so an unstable host degrades to no-ops instead of
     *  state-destroying reconfigures. */
    extra?: Extension;
    /** Set false when `extra` already provides the language — skips the lazy
     *  filename-matched pack, which would otherwise load a second, permanently
     *  inert language instance into the config. */
    autoLanguage?: boolean;
    /** Consume "open at this line" requests (shared/reveal) for this path. A
     *  host that shows another surface for the same path (markdown reading)
     *  passes false while that surface handles reveals itself. */
    acceptReveal?: boolean;
  }

  let {
    path,
    first,
    onDoc = undefined,
    extra = [],
    autoLanguage = true,
    acceptReveal = true,
  }: Props = $props();

  const buf = openBuffer(
    untrack(() => path),
    untrack(() => first),
  );

  let host = $state<HTMLDivElement | null>(null);
  let view: EditorView | null = null;
  /** The view exists (reveal and other view-bound effects wait for it). */
  let ready = $state(false);
  /** Another view of this buffer took over (a transient overlap while a tab
   *  moves between panes): this one must not accept keystrokes. */
  let superseded = $state(false);
  const settingsCompartment = new Compartment();
  const extraCompartment = new Compartment();
  const langCompartment = new Compartment();

  // Context bridge: this view's selection, published for the reference
  // affordance + chord. The chip floats near the selection's end.
  const selOwner = {};
  let wrapEl = $state<HTMLDivElement | null>(null);
  let chipPos = $state<{ x: number; y: number } | null>(null);

  /** Publish/clear the selection and (re)place the chip near its end. */
  function syncSelection(v: EditorView): void {
    const sel = v.state.selection.main;
    if (sel.empty) {
      chipPos = null;
      clearSelection(selOwner);
      return;
    }
    const startLine = v.state.doc.lineAt(sel.from).number;
    const endAt = v.state.doc.lineAt(sel.to);
    // A selection ending exactly at a line start doesn't include that line.
    const endLine = endAt.number > startLine && endAt.from === sel.to ? endAt.number - 1 : endAt.number;
    setSelection(selOwner, {
      kind: "file",
      path: buf.path,
      startLine,
      endLine,
      text: v.state.sliceDoc(sel.from, sel.to),
    });
    placeChip(v);
  }

  /** Chip position: just under the selection head, clamped into the view. */
  function placeChip(v: EditorView): void {
    const wrap = wrapEl;
    if (wrap === null) return;
    const sel = v.state.selection.main;
    if (sel.empty) return;
    const coords = v.coordsAtPos(sel.head);
    if (coords === null) {
      // Selection end scrolled out of the viewport: hide the chip, keep the
      // selection registered (the chord still works).
      chipPos = null;
      return;
    }
    const rect = wrap.getBoundingClientRect();
    const clamp = (n: number, lo: number, hi: number) => Math.min(Math.max(n, lo), Math.max(lo, hi));
    chipPos = {
      x: clamp(coords.left - rect.left + 4, 4, rect.width - 170),
      y: clamp(coords.bottom - rect.top + 6, 4, rect.height - 58),
    };
  }

  /** Settings-driven extensions (swapped live via settingsCompartment). */
  function settingsExtensions() {
    const tabSize = getSetting("editor.tabSize");
    return [
      makeTheme(getSetting("editor.fontSize"), getSetting("editor.lineHeight")),
      getSetting("editor.lineNumbers") ? lineNumbers() : [],
      getSetting("editor.wordWrap") ? EditorView.lineWrapping : [],
      EditorState.tabSize.of(tabSize),
      indentUnit.of(" ".repeat(tabSize)),
    ];
  }

  // Live settings changes (this window or any other) reconfigure in place.
  $effect(() => {
    const extensions = settingsExtensions();
    if (view !== null) {
      view.dispatch({ effects: settingsCompartment.reconfigure(extensions) });
    }
  });

  // Host extension swaps (e.g. markdown live ⇄ source) reconfigure in place —
  // the document, undo history, and dirty state all survive the flip. The
  // identity guard absorbs the effect's first post-mount run (onMount records
  // the value the view was created with) and any unstable-host churn.
  let lastExtra: Extension | null = null;
  $effect(() => {
    const ext = extra;
    if (view !== null && ext !== lastExtra) {
      lastExtra = ext;
      view.dispatch({ effects: extraCompartment.reconfigure(ext) });
    }
  });

  /** This view's own extensions; the buffer adds history, keymaps, search. */
  function viewExtensions(): Extension {
    return [
      settingsCompartment.of(settingsExtensions()),
      // Before the filename-matched language pack, so a host language in
      // `extra` (markdown live) wins the language facet.
      extraCompartment.of((lastExtra = extra)),
      langCompartment.of([]),
      highlightSpecialChars(),
      drawSelection(),
      bracketMatching(),
      syntaxHighlighting(highlight, { fallback: true }),
      codeSearchTheme,
      revealFlash,
      // Context bridge + live-buffer sink + autosave-on-blur, in both
      // read-only and editable modes.
      EditorView.updateListener.of((u) => {
        if (u.view !== view) return;
        if (u.selectionSet || u.docChanged) syncSelection(u.view);
        else if (u.geometryChanged) placeChip(u.view);
        if (u.docChanged) onDoc?.(u.state.doc.toString());
        if (u.focusChanged && !u.view.hasFocus) buf.flushAutosave();
      }),
    ];
  }

  onMount(() => {
    const el = host;
    if (el === null) return () => releaseBuffer(buf);
    const v = new EditorView({ state: buf.stateFor(viewExtensions()), parent: el });
    view = v;
    buf.attach(v, () => {
      superseded = true;
      view = null;
    });
    ready = true;
    // Seed the live-buffer sink with the current text (the split preview shows
    // it before the first keystroke — or a re-attached buffer's unsaved text).
    onDoc?.(v.state.doc.toString());

    // Keep the chip pinned to the selection end while the code scrolls.
    const onScroll = () => placeChip(v);
    v.scrollDOM.addEventListener("scroll", onScroll, { passive: true });
    // Autosave on window blur too (the editor keeps DOM focus when the whole
    // window loses it, so its own focus change never fires).
    const onWindowBlur = () => buf.flushAutosave();
    window.addEventListener("blur", onWindowBlur);

    // Language by filename, loaded lazily into its compartment.
    const desc = autoLanguage ? LanguageDescription.matchFilename(languages, basename(buf.path)) : null;
    if (desc !== null) {
      void desc
        .load()
        .then((support) => {
          if (view === v) v.dispatch({ effects: langCompartment.reconfigure(support) });
        })
        .catch(() => {
          // language pack failed to load; plain text is fine
        });
    }

    return () => {
      ready = false;
      if (view === v) view = null;
      if (flashTimer !== null) clearTimeout(flashTimer);
      if (savedTimer !== null) clearTimeout(savedTimer);
      if (nudgeTimer !== null) clearTimeout(nudgeTimer);
      clearSelection(selOwner);
      v.scrollDOM.removeEventListener("scroll", onScroll);
      window.removeEventListener("blur", onWindowBlur);
      buf.detach(v);
      releaseBuffer(buf);
      v.destroy();
    };
  });

  // --- reveal: "open this file at line N" ----------------------------------
  let flashTimer: ReturnType<typeof setTimeout> | null = null;
  $effect(() => {
    void $revealRequest;
    if (!acceptReveal || !ready || buf.loading) return;
    untrack(() => {
      const v = view;
      if (v === null) return;
      const r = takeReveal(buf.path);
      if (r === null) return;
      revealLines(v, r);
      if (flashTimer !== null) clearTimeout(flashTimer);
      flashTimer = setTimeout(() => {
        flashTimer = null;
        if (view === v) clearRevealFlash(v);
      }, REVEAL_FLASH_MS);
    });
  });

  // --- status chrome ----------------------------------------------------------
  let savedFlash = $state(false);
  let savedTimer: ReturnType<typeof setTimeout> | null = null;
  let seenSaved = untrack(() => buf.savedCount);
  $effect(() => {
    const n = buf.savedCount;
    if (n === seenSaved) return;
    seenSaved = n;
    savedFlash = true;
    if (savedTimer !== null) clearTimeout(savedTimer);
    savedTimer = setTimeout(() => {
      savedTimer = null;
      savedFlash = false;
    }, 1600);
  });
  // A typed key retires the "saved" flash.
  $effect(() => {
    if (buf.dirty) savedFlash = false;
  });

  // Cmd+S refused because a conflict is open: pulse the bar that explains why.
  let nudging = $state(false);
  let nudgeTimer: ReturnType<typeof setTimeout> | null = null;
  let seenNudge = untrack(() => buf.conflictNudge);
  $effect(() => {
    const n = buf.conflictNudge;
    if (n === seenNudge) return;
    seenNudge = n;
    nudging = true;
    if (nudgeTimer !== null) clearTimeout(nudgeTimer);
    nudgeTimer = setTimeout(() => {
      nudgeTimer = null;
      nudging = false;
    }, 700);
  });

  const status = $derived.by((): { text: string; tone: "" | "warn" | "err" } => {
    switch (buf.saveState) {
      case "saving":
        return { text: "saving…", tone: "" };
      case "retrying":
        return { text: "not saved, retrying…", tone: "warn" };
      case "offline":
        return { text: "offline — not saved, will retry", tone: "warn" };
      case "failed":
        return { text: "not saved", tone: "err" };
    }
    if (buf.dirty) return { text: "unsaved", tone: "" };
    if (savedFlash) return { text: "saved", tone: "" };
    return { text: "editable", tone: "" };
  });

  const elsewhere = $derived(presence.elsewhere.has(buf.path));

  function recoveredWhen(ms: number): string {
    if (!Number.isFinite(ms) || ms <= 0) return "an earlier session";
    const d = new Date(ms);
    const sameDay = d.toDateString() === new Date().toDateString();
    const time = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
    return sameDay ? time : `${d.toLocaleDateString()} ${time}`;
  }

  // --- compare overlay (loaded on demand) ------------------------------------
  type Compare = {
    title: string;
    a: string;
    b: string;
    aLabel: string;
    bLabel: string;
    conflict: boolean;
  };
  let compare = $state<Compare | null>(null);
  let CompareView = $state<Component<any> | null>(null);
  let compareError = $state<string | null>(null);

  async function openCompare(c: Compare): Promise<void> {
    compareError = null;
    if (CompareView === null) {
      try {
        CompareView = (await import("./CompareView.svelte")).default;
      } catch {
        compareError = "failed to load the comparison";
        return;
      }
    }
    compare = c;
  }

  function compareConflict(): void {
    const c = buf.conflict;
    if (c === null || c.kind !== "changed" || c.disk.text === null) return;
    void openCompare({
      title: "Your edits vs. the file on disk",
      a: buf.current.doc.toString(),
      b: c.disk.text,
      aLabel: "your edits",
      bLabel: "on disk",
      conflict: true,
    });
  }

  function viewMergeDiff(): void {
    const n = buf.notice;
    if (n === null) return;
    void openCompare({
      title: "Merged changes from disk",
      a: n.before,
      b: n.after,
      aLabel: "before the merge",
      bLabel: "after",
      conflict: false,
    });
  }

  function closeCompare(): void {
    compare = null;
    view?.focus();
  }

  // A resolved conflict retires a comparison opened from it.
  $effect(() => {
    if (buf.conflict === null && compare?.conflict === true) compare = null;
  });

  function keepMine(): void {
    buf.keepMine();
    compare = null;
  }

  function takeDisk(): void {
    buf.takeDiskVersion();
    compare = null;
  }

  function recreate(): void {
    buf.keepMine();
    void buf.save();
  }

  const conflictText = $derived.by(() => {
    const c = buf.conflict;
    if (c === null) return "";
    if (c.kind === "deleted") return buf.dirty ? "deleted on disk — your edits are kept here" : "deleted on disk";
    if (c.disk.note !== null) return `changed on disk (${c.disk.note.replace(/ — .*$/, "")}) — can't merge`;
    return "changed on disk — your edits overlap";
  });
</script>

{#snippet conflictActions()}
  <button class="bar-btn" onclick={keepMine} title="keep your text; the next save replaces the disk version">keep mine</button>
  <button class="bar-btn danger" onclick={takeDisk} title="discard your edits for the disk version (undo brings them back)">take disk</button>
{/snippet}

<div class="code-view" bind:this={wrapEl}>
  {#if chipPos !== null}
    <ReferenceChip x={chipPos.x} y={chipPos.y} />
  {/if}
  {#if buf.recovered !== null}
    <div class="strip recover" role="status">
      <span class="strip-msg">Recovered unsaved changes from {recoveredWhen(buf.recovered.updatedMs)}</span>
      <button class="bar-btn" onclick={() => buf.restoreDraft()}>restore</button>
      <button class="bar-btn" onclick={() => buf.discardDraft()}>discard</button>
    </div>
  {/if}
  {#if buf.conflict !== null}
    <!-- The file changed on disk under unsaved edits and could not be merged,
         or vanished. Saving waits until this is resolved. -->
    <div class="strip conflict" class:nudge={nudging} role="alert">
      <span class="strip-msg">{conflictText}</span>
      {#if buf.conflict.kind === "deleted"}
        <button class="bar-btn" onclick={recreate}>recreate</button>
        {#if buf.dirty}
          <button class="bar-btn danger" onclick={takeDisk}>discard edits</button>
        {/if}
      {:else}
        {#if buf.conflict.disk.text !== null}
          <button class="bar-btn" onclick={compareConflict}>compare</button>
        {/if}
        {@render conflictActions()}
      {/if}
    </div>
  {:else if buf.notice !== null}
    <div class="strip notice" role="status">
      <span class="strip-msg">Merged changes from disk</span>
      <button class="bar-btn" onclick={viewMergeDiff}>view diff</button>
      <button class="bar-btn" onclick={() => buf.undoMerge()}>undo</button>
      <span class="spacer"></span>
      <button class="strip-close" aria-label="dismiss" onclick={() => buf.clearNotice()}>&times;</button>
    </div>
  {/if}
  {#if elsewhere}
    <div class="strip subtle" role="status">
      <span class="strip-msg">Unsaved edits in another window</span>
    </div>
  {/if}
  <div class="editor" class:superseded bind:this={host}></div>
  {#if superseded}
    <div class="superseded-note">open in another pane</div>
  {/if}
  {#if compare !== null && CompareView !== null}
    <CompareView
      path={buf.path}
      title={compare.title}
      a={compare.a}
      b={compare.b}
      aLabel={compare.aLabel}
      bLabel={compare.bLabel}
      onClose={closeCompare}
      actions={compare.conflict && buf.conflict !== null ? conflictActions : undefined}
    />
  {/if}
  <footer class="bar">
    {#if buf.editable}
      <span class="status" class:warn={status.tone === "warn"} class:err={status.tone === "err"}>{status.text}</span>
      {#if buf.saveError !== null}
        <span class="bar-err" title={buf.saveError}>{buf.saveError}</span>
      {/if}
      {#if buf.journalFailed}
        <span
          class="bar-warn"
          title="The unsaved text could not be written to the browser's storage or to the daemon — save when you can."
          >draft not backed up</span
        >
      {/if}
      {#if compareError !== null}<span class="bar-err">{compareError}</span>{/if}
      <span class="spacer"></span>
      <span class="hint">{SAVE_HINT}</span>
    {:else if buf.loading}
      <span class="status">loading…</span>
      <span class="spacer"></span>
    {:else if buf.truncated}
      <span class="status">showing {humanSize(buf.loadedBytes)} of {humanSize(buf.totalBytes)}</span>
      {#if buf.loadError !== null}<span class="bar-err">{buf.loadError}</span>{/if}
      <span class="spacer"></span>
      {#if buf.note !== null}<span class="hint">{buf.note}</span>{/if}
      <button class="more-btn" disabled={buf.loadingMore} onclick={() => void buf.loadMore()}>
        {buf.loadingMore ? "loading…" : "load more"}
      </button>
    {:else}
      <span class="status">read-only</span>
      {#if buf.loadError !== null}<span class="bar-err">{buf.loadError}</span>{/if}
      <span class="spacer"></span>
      {#if buf.note !== null}<span class="hint">{buf.note}</span>{/if}
    {/if}
  </footer>
</div>

<style>
  .code-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  .editor {
    flex: 1;
    min-height: 0;
  }

  .editor.superseded {
    display: none;
  }

  .superseded-note {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--muted);
    font-size: var(--text-sm);
  }

  .editor :global(.cm-editor) {
    height: 100%;
  }

  /* One quiet strip per concern, stacked above the editor. */
  .strip {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    min-height: 28px;
    padding: 0 0.7rem;
    font-size: var(--text-sm);
    color: var(--fg);
    border-bottom: 1px solid var(--edge);
  }

  .strip-msg {
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .strip.conflict {
    background: color-mix(in srgb, var(--warn) 12%, var(--term-bg));
    border-bottom-color: color-mix(in srgb, var(--warn) 40%, var(--edge));
  }

  .strip.conflict .strip-msg {
    color: var(--warn);
  }

  .strip.conflict.nudge {
    animation: conflict-nudge 0.7s ease-out;
  }

  @keyframes conflict-nudge {
    0%,
    100% {
      background: color-mix(in srgb, var(--warn) 12%, var(--term-bg));
    }
    30% {
      background: color-mix(in srgb, var(--warn) 30%, var(--term-bg));
    }
  }

  .strip.recover {
    background: color-mix(in srgb, var(--accent) 9%, var(--term-bg));
    border-bottom-color: color-mix(in srgb, var(--accent) 35%, var(--edge));
  }

  .strip.notice {
    background: color-mix(in srgb, var(--accent) 6%, var(--term-bg));
  }

  .strip.notice .strip-msg,
  .strip.subtle .strip-msg {
    font-weight: 400;
    color: var(--muted);
  }

  .strip.subtle {
    min-height: 24px;
    font-size: var(--text-xs);
  }

  .strip-close {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-md);
    color: var(--muted);
    cursor: pointer;
    padding: 0 0.2rem;
    line-height: 1;
  }

  .strip-close:hover {
    color: var(--fg);
  }

  .bar-btn {
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--term-bg);
    font: inherit;
    font-size: var(--text-sm);
    color: var(--fg);
    cursor: pointer;
    padding: 0.1rem 0.5rem;
    border-radius: 4px;
    white-space: nowrap;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .bar-btn:hover {
    background: var(--row-hover);
  }

  .bar-btn.danger:hover {
    color: var(--err);
    border-color: color-mix(in srgb, var(--err) 45%, var(--edge));
  }

  .bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    height: 26px;
    padding: 0 0.7rem;
    border-top: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
    font-variant-numeric: tabular-nums;
    min-width: 0;
  }

  .status {
    color: var(--muted);
    white-space: nowrap;
  }

  .status.warn,
  .bar-warn {
    color: var(--warn);
    white-space: nowrap;
  }

  .status.err,
  .bar-err {
    color: var(--err);
  }

  .bar-err {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }

  .spacer {
    flex: 1;
  }

  .hint {
    font-family: var(--mono);
    opacity: 0.7;
    white-space: nowrap;
  }

  .more-btn {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    padding: 0.1rem 0.4rem;
    border-radius: 4px;
  }

  .more-btn:hover:not(:disabled) {
    background: var(--row-hover);
    color: var(--fg);
  }

  .more-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
