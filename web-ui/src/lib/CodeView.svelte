<script lang="ts">
  /**
   * CodeMirror 6 view for code/text files. Read-only for oversized/truncated
   * files (the "load more" tail); editable for text files < 1MB. Cmd/Ctrl+S
   * saves through the daemon with an mtime precondition — a concurrent
   * on-disk change surfaces a quiet reload/overwrite conflict bar. Dirty state
   * lives in the editing store (drives the tab dot + the beforeunload guard).
   *
   * The editor instance is plain module-ish state, never $state (same rule as
   * xterm instances in termPool).
   */
  import { onMount } from "svelte";
  import { Compartment, EditorState, StateEffect } from "@codemirror/state";
  import {
    EditorView,
    lineNumbers,
    highlightSpecialChars,
    keymap,
    drawSelection,
  } from "@codemirror/view";
  import {
    LanguageDescription,
    syntaxHighlighting,
    bracketMatching,
    indentOnInput,
    indentUnit,
  } from "@codemirror/language";
  import { languages } from "@codemirror/language-data";
  import {
    defaultKeymap,
    history,
    historyKeymap,
    indentWithTab,
  } from "@codemirror/commands";
  import { codeHighlight as highlight, makeCodeTheme as makeTheme } from "./cm";
  import {
    basename,
    fsFile,
    fsWrite,
    humanSize,
    looksBinary,
    FileConflictError,
    EDIT_MAX_BYTES,
    FILE_CHUNK,
    type FileChunk,
  } from "./files";
  import { ApiError } from "./api";
  import { setDirty, forgetDirty } from "./editing";
  import { getSetting } from "./settings/store.svelte";
  import { isMac } from "./keys";
  import { clearSelection, setSelection } from "./reference";
  import ReferenceChip from "./ReferenceChip.svelte";

  const SAVE_HINT = isMac ? "⌘S to save" : "Ctrl+S to save";

  interface Props {
    path: string;
    /** First chunk, already fetched (and sniffed as text) by FileView. */
    first: FileChunk;
  }

  let { path, first }: Props = $props();

  let host = $state<HTMLDivElement | null>(null);
  let loadedBytes = $state(0);
  let totalBytes = $state(0);
  let truncated = $state(false);
  let loadingMore = $state(false);
  let loadError = $state<string | null>(null);

  // Editing state.
  let editable = $state(false);
  let dirty = $state(false);
  let savedMtime = $state<string | null>(null);
  let saving = $state(false);
  let saveError = $state<string | null>(null);
  let conflict = $state(false);
  let savedFlash = $state(false);
  let flashTimer: ReturnType<typeof setTimeout> | null = null;

  let view: EditorView | null = null;
  const editCompartment = new Compartment();
  const settingsCompartment = new Compartment();

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
      path,
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
  // Streaming decoder: chunk boundaries may split a UTF-8 sequence; the
  // decoder carries the partial bytes across load-more calls.
  const decoder = new TextDecoder("utf-8", { fatal: false });

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

  /** The editable/read-only extension set for the compartment. */
  function editExtensions(canEdit: boolean) {
    return canEdit
      ? [
          history(),
          keymap.of([
            { key: "Mod-s", run: () => (triggerSave(), true), preventDefault: true },
            indentWithTab,
            ...defaultKeymap,
            ...historyKeymap,
          ]),
          indentOnInput(),
          EditorView.editable.of(true),
          EditorView.updateListener.of((u) => {
            if (u.docChanged) markDirty();
          }),
        ]
      : [EditorState.readOnly.of(true), EditorView.editable.of(false)];
  }

  function markDirty(): void {
    if (!editable) return;
    if (!dirty) {
      dirty = true;
      setDirty(path, true);
    }
    // A fresh edit invalidates the "saved" flash and any stale conflict/error.
    savedFlash = false;
  }

  function clearDirty(): void {
    dirty = false;
    setDirty(path, false);
  }

  onMount(() => {
    const el = host;
    if (el === null) return;
    const text = decoder.decode(first.bytes, { stream: true });
    loadedBytes = first.bytes.length;
    totalBytes = first.size;
    truncated = first.truncated;
    savedMtime = first.mtime;
    // Editable when the whole file fits under the 1MB cap and is text. Large
    // truncated files stay read-only with the load-more tail.
    editable = totalBytes <= EDIT_MAX_BYTES && !truncated;

    const state = EditorState.create({
      doc: text,
      extensions: [
        settingsCompartment.of(settingsExtensions()),
        highlightSpecialChars(),
        drawSelection(),
        bracketMatching(),
        syntaxHighlighting(highlight, { fallback: true }),
        // Context bridge: track the selection in both read-only and editable
        // modes (this listener lives outside the edit compartment).
        EditorView.updateListener.of((u) => {
          if (u.selectionSet || u.docChanged) syncSelection(u.view);
          else if (u.geometryChanged) placeChip(u.view);
        }),
        editCompartment.of(editExtensions(editable)),
      ],
    });
    const v = new EditorView({ state, parent: el });
    view = v;

    // Keep the chip pinned to the selection end while the code scrolls.
    const onScroll = () => placeChip(v);
    v.scrollDOM.addEventListener("scroll", onScroll, { passive: true });

    // Under-cap files that came back truncated (256KB < size ≤ 1MB): pull the
    // rest in the background so the editor holds the full document and can save.
    if (totalBytes <= EDIT_MAX_BYTES && truncated) {
      void fillToEnd(v);
    }

    // Language by filename, loaded lazily; appended once ready.
    const desc = LanguageDescription.matchFilename(languages, basename(path));
    if (desc !== null) {
      void desc
        .load()
        .then((support) => {
          if (view === v) v.dispatch({ effects: StateEffect.appendConfig.of(support) });
        })
        .catch(() => {
          // language pack failed to load; plain text is fine
        });
    }

    return () => {
      view = null;
      if (flashTimer !== null) clearTimeout(flashTimer);
      forgetDirty(path);
      clearSelection(selOwner);
      v.scrollDOM.removeEventListener("scroll", onScroll);
      v.destroy();
    };
  });

  /** Load remaining chunks (silently) so an under-cap file becomes editable. */
  async function fillToEnd(v: EditorView): Promise<void> {
    while (view === v && truncated) {
      try {
        const chunk = await fsFile(path, loadedBytes, FILE_CHUNK);
        if (view !== v) return;
        const text = decoder.decode(chunk.bytes, { stream: true });
        v.dispatch({ changes: { from: v.state.doc.length, insert: text } });
        loadedBytes += chunk.bytes.length;
        totalBytes = chunk.size;
        truncated = chunk.truncated;
        if (chunk.bytes.length === 0) break;
      } catch {
        return; // leave it read-only-ish; user can still view
      }
    }
    if (view === v && !truncated && totalBytes <= EDIT_MAX_BYTES && !editable) {
      editable = true;
      // The background fill shouldn't leave the doc marked dirty.
      v.dispatch({ effects: editCompartment.reconfigure(editExtensions(true)) });
      clearDirty();
    }
  }

  async function loadMore(): Promise<void> {
    const v = view;
    if (v === null || loadingMore) return;
    loadingMore = true;
    loadError = null;
    try {
      const chunk = await fsFile(path, loadedBytes, FILE_CHUNK);
      if (view !== v) return;
      const text = decoder.decode(chunk.bytes, { stream: true });
      v.dispatch({ changes: { from: v.state.doc.length, insert: text } });
      loadedBytes += chunk.bytes.length;
      totalBytes = chunk.size;
      truncated = chunk.truncated;
    } catch (e) {
      loadError = e instanceof Error ? e.message : "failed to load more";
    } finally {
      loadingMore = false;
    }
  }

  function triggerSave(): void {
    void save(false);
  }

  async function save(force: boolean): Promise<void> {
    const v = view;
    if (v === null || !editable || saving) return;
    if (!dirty && !force) return;
    saving = true;
    saveError = null;
    const text = v.state.doc.toString();
    const bytes = new TextEncoder().encode(text);
    try {
      const mtime = await fsWrite(path, bytes, force ? null : savedMtime);
      if (view !== v) return;
      savedMtime = mtime;
      conflict = false;
      saveError = null;
      clearDirty();
      flashSaved();
    } catch (e) {
      if (view !== v) return;
      if (e instanceof FileConflictError) {
        conflict = true;
      } else if (e instanceof ApiError) {
        saveError = e.message;
      } else {
        saveError = e instanceof Error ? e.message : "save failed";
      }
    } finally {
      if (view === v) saving = false;
    }
  }

  function flashSaved(): void {
    savedFlash = true;
    if (flashTimer !== null) clearTimeout(flashTimer);
    flashTimer = setTimeout(() => (savedFlash = false), 1600);
  }

  /** Conflict → reload: discard local edits, take the on-disk version. */
  async function reloadFromDisk(): Promise<void> {
    const v = view;
    if (v === null) return;
    try {
      const chunk = await fsFile(path, 0, EDIT_MAX_BYTES);
      if (view !== v) return;
      const fresh = new TextDecoder("utf-8", { fatal: false }).decode(chunk.bytes);
      if (looksBinary(chunk.bytes)) return;
      v.dispatch({ changes: { from: 0, to: v.state.doc.length, insert: fresh } });
      loadedBytes = chunk.bytes.length;
      totalBytes = chunk.size;
      truncated = chunk.truncated;
      savedMtime = chunk.mtime;
      conflict = false;
      saveError = null;
      clearDirty();
    } catch (e) {
      saveError = e instanceof Error ? e.message : "reload failed";
    }
  }
</script>

<div class="code-view" bind:this={wrapEl}>
  {#if chipPos !== null}
    <ReferenceChip x={chipPos.x} y={chipPos.y} />
  {/if}
  {#if conflict}
    <!-- Quiet concurrent-modification bar: the file changed on disk under us. -->
    <div class="conflict" role="alert">
      <span class="conflict-msg">changed on disk</span>
      <button class="conflict-btn" onclick={() => void reloadFromDisk()}>reload</button>
      <button class="conflict-btn danger" onclick={() => void save(true)}>overwrite</button>
    </div>
  {/if}
  <div class="editor" bind:this={host}></div>
  <footer class="bar">
    {#if editable}
      <span class="status">
        {#if saving}saving…{:else if dirty}unsaved{:else if savedFlash}saved{:else}editable{/if}
      </span>
      {#if saveError !== null}
        <span class="bar-err">{saveError}</span>
      {/if}
      <span class="spacer"></span>
      <span class="hint">{SAVE_HINT}</span>
    {:else}
      {#if truncated}
        <span class="status">showing {humanSize(loadedBytes)} of {humanSize(totalBytes)}</span>
        {#if loadError !== null}<span class="bar-err">{loadError}</span>{/if}
        <span class="spacer"></span>
        <button class="more-btn" disabled={loadingMore} onclick={() => void loadMore()}>
          {loadingMore ? "loading…" : "load more"}
        </button>
      {:else}
        <span class="status">read-only</span>
        <span class="spacer"></span>
        {#if totalBytes > EDIT_MAX_BYTES}<span class="hint">over 1 MB — view only</span>{/if}
      {/if}
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

  .editor :global(.cm-editor) {
    height: 100%;
  }

  .conflict {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    height: 28px;
    padding: 0 0.7rem;
    background: color-mix(in srgb, var(--warn) 12%, var(--term-bg));
    border-bottom: 1px solid color-mix(in srgb, var(--warn) 40%, var(--edge));
    font-size: 0.72rem;
    color: var(--fg);
  }

  .conflict-msg {
    color: var(--warn);
    font-weight: 500;
  }

  .conflict-btn {
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--term-bg);
    font: inherit;
    font-size: 0.7rem;
    color: var(--fg);
    cursor: pointer;
    padding: 0.1rem 0.5rem;
    border-radius: 4px;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .conflict-btn:hover {
    background: var(--row-hover);
  }

  .conflict-btn.danger:hover {
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
    font-size: 0.68rem;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }

  .status {
    color: var(--muted);
  }

  .bar-err {
    color: var(--err);
  }

  .spacer {
    flex: 1;
  }

  .hint {
    font-family: var(--mono);
    opacity: 0.7;
  }

  .more-btn {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: 0.68rem;
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
