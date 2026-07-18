<script lang="ts">
  /**
   * Side-by-side git diff for one file. The daemon returns the two full blob
   * versions (before/after) and @codemirror/merge computes and renders the
   * changes — no hunk parsing, and full surrounding context for review. Files
   * past the daemon's cap, and binary ones, degrade to a quiet message.
   *
   * The MergeView instance is plain (never $state), the same rule the xterm
   * instances and CodeView's EditorView follow.
   *
   * Context bridge: a selection on the RIGHT side publishes a file reference —
   * but only when that side is the working tree (in "staged" mode the right
   * side is the index, whose line numbers need not match the file on disk).
   */
  import { untrack } from "svelte";
  import { EditorState, StateEffect } from "@codemirror/state";
  import { EditorView, lineNumbers, highlightSpecialChars } from "@codemirror/view";
  import { MergeView } from "@codemirror/merge";
  import { LanguageDescription, syntaxHighlighting } from "@codemirror/language";
  import { languages } from "@codemirror/language-data";
  import { codeHighlight, makeCodeTheme } from "./cm";
  import { fetchGitDiff, gitStatus, type DiffMode, type GitDiff } from "../workspace/git";
  import { basename } from "./files";
  import { getSetting } from "../settings/store.svelte";
  import { clearSelection, setSelection } from "../shared/reference";
  import ReferenceChip from "../shared/ReferenceChip.svelte";

  interface Props {
    path: string;
    /** The comparison this tab was opened at. */
    mode: DiffMode;
    wsId: string | null;
  }
  let { path, mode, wsId }: Props = $props();

  const MODES: { id: DiffMode; label: string; title: string }[] = [
    { id: "unstaged", label: "Unstaged", title: "working tree vs index" },
    { id: "staged", label: "Staged", title: "index vs HEAD" },
    { id: "head", label: "All", title: "working tree vs HEAD" },
  ];

  // The bar toggle changes the comparison shown WITHOUT changing the tab's
  // identity (which encodes the mode it was opened at). Capturing the prop once
  // is deliberate — Pane keys this component per (path, mode), so a different
  // diff tab remounts rather than reusing this instance with a stale toggle.
  let viewMode = $state<DiffMode>(untrack(() => mode));
  let host = $state<HTMLDivElement | null>(null);
  let wrapEl = $state<HTMLDivElement | null>(null);
  let diff = $state<GitDiff | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(true);
  let chipPos = $state<{ x: number; y: number } | null>(null);

  let merge: MergeView | null = null;
  let loadSeq = 0;
  // Plain (non-reactive) so writing it inside the epoch effect cannot loop.
  let lastEpoch = -1;
  const selOwner = {};

  /** Nothing to render in the merge host: a message takes the surface instead. */
  const identical = $derived(diff !== null && !diff.binary && !diff.too_large && diff.a === diff.b);
  const showsMessage = $derived(
    loading || error !== null || diff === null || diff.binary || diff.too_large === true || identical,
  );

  function destroy(): void {
    merge?.destroy();
    merge = null;
    chipPos = null;
    clearSelection(selOwner);
  }

  function baseExtensions() {
    return [
      lineNumbers(),
      highlightSpecialChars(),
      syntaxHighlighting(codeHighlight, { fallback: true }),
      makeCodeTheme(getSetting("editor.fontSize"), getSetting("editor.lineHeight")),
      EditorState.readOnly.of(true),
      EditorView.editable.of(false),
    ];
  }

  /** Publish the right-side selection as a file reference (+ place the chip). */
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

  function placeChip(v: EditorView): void {
    const wrap = wrapEl;
    if (wrap === null) return;
    const sel = v.state.selection.main;
    if (sel.empty) return;
    const coords = v.coordsAtPos(sel.head);
    if (coords === null) {
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

  function build(d: GitDiff, m: DiffMode): void {
    const el = host;
    if (el === null) return;
    destroy();
    // Only the working tree's line numbers match the file on disk, so the
    // reference bridge is armed for those comparisons only.
    const bIsWorkingTree = m !== "staged";
    const bExtensions = bIsWorkingTree
      ? [
          ...baseExtensions(),
          EditorView.updateListener.of((u) => {
            if (u.selectionSet || u.docChanged) syncSelection(u.view);
            else if (u.geometryChanged) placeChip(u.view);
          }),
        ]
      : baseExtensions();

    const view = new MergeView({
      a: { doc: d.a, extensions: baseExtensions() },
      b: { doc: d.b, extensions: bExtensions },
      parent: el,
      // Long unchanged stretches fold away: review reads changes, not the file.
      collapseUnchanged: { margin: 3, minSize: 6 },
      highlightChanges: true,
      gutter: true,
    });
    merge = view;

    // Language by filename, loaded lazily, appended to both sides.
    const desc = LanguageDescription.matchFilename(languages, basename(path));
    if (desc !== null) {
      void desc
        .load()
        .then((support) => {
          if (merge !== view) return;
          view.a.dispatch({ effects: StateEffect.appendConfig.of(support) });
          view.b.dispatch({ effects: StateEffect.appendConfig.of(support) });
        })
        .catch(() => {
          // language pack failed to load; plain text is fine
        });
    }
  }

  async function load(id: string, p: string, m: DiffMode): Promise<void> {
    const seq = ++loadSeq;
    loading = true;
    error = null;
    try {
      const d = await fetchGitDiff(id, p, m);
      if (seq !== loadSeq) return;
      diff = d;
      if (d.binary || d.too_large || d.a === d.b) destroy();
      else build(d, m);
    } catch (e) {
      if (seq !== loadSeq) return;
      destroy();
      diff = null;
      error = e instanceof Error ? e.message : "failed to load diff";
    } finally {
      if (seq === loadSeq) loading = false;
    }
  }

  // Load on mount and whenever the file, workspace, or comparison changes.
  // untrack keeps settings reads inside load()/build() from re-triggering us.
  $effect(() => {
    const id = wsId;
    const p = path;
    const m = viewMode;
    const el = host;
    if (id === null || el === null) return;
    untrack(() => void load(id, p, m));
  });

  // Keep the diff live: an agent write, a save, or a terminal `git` command
  // bumps the workspace epoch, and the view refetches.
  $effect(() => {
    const epoch = $gitStatus?.epoch ?? -1;
    if (epoch < 0) return;
    if (lastEpoch < 0) {
      lastEpoch = epoch;
      return;
    }
    if (epoch !== lastEpoch) {
      lastEpoch = epoch;
      const id = wsId;
      const el = host;
      if (id !== null && el !== null) untrack(() => void load(id, path, viewMode));
    }
  });

  $effect(() => () => {
    loadSeq++;
    destroy();
  });
</script>

<div class="diff-view" bind:this={wrapEl}>
  {#if chipPos !== null}
    <ReferenceChip x={chipPos.x} y={chipPos.y} />
  {/if}
  <header class="dbar">
    <span class="dpath" title={path}>{basename(path)}</span>
    {#if diff !== null && !showsMessage}
      <span class="dlabels">{diff.a_label} → {diff.b_label}</span>
    {/if}
    {#if diff?.added}<span class="dtag added">added</span>{/if}
    {#if diff?.deleted}<span class="dtag deleted">deleted</span>{/if}
    <span class="spacer"></span>
    <div class="modes" role="group" aria-label="comparison">
      {#each MODES as m (m.id)}
        <button
          class="mode"
          class:on={viewMode === m.id}
          title={m.title}
          onclick={() => (viewMode = m.id)}>{m.label}</button
        >
      {/each}
    </div>
  </header>

  <div class="merge-host" class:hidden={showsMessage} bind:this={host}></div>

  {#if showsMessage}
    <div class="msg">
      {#if loading}
        loading diff…
      {:else if error !== null}
        <span class="err">{error}</span>
      {:else if diff === null}
        no diff
      {:else if diff.binary}
        binary file — not shown
      {:else if diff.too_large}
        diff too large — open the file instead
      {:else}
        no changes in this comparison
      {/if}
    </div>
  {/if}
</div>

<style>
  .diff-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    background: var(--term-bg);
  }

  .dbar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    height: 28px;
    padding: 0 0.6rem;
    border-bottom: 1px solid var(--edge);
    font-size: 0.7rem;
    color: var(--muted);
  }

  .dpath {
    font-family: var(--mono);
    color: var(--fg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .dlabels {
    font-family: var(--mono);
    opacity: 0.75;
    white-space: nowrap;
  }

  .dtag {
    font-size: 0.62rem;
    padding: 0.05rem 0.32rem;
    border-radius: 3px;
    font-weight: 600;
    letter-spacing: 0.02em;
  }
  .dtag.added {
    color: var(--git-added);
    background: color-mix(in srgb, var(--git-added) 14%, transparent);
  }
  .dtag.deleted {
    color: var(--git-deleted);
    background: color-mix(in srgb, var(--git-deleted) 14%, transparent);
  }

  .spacer {
    flex: 1;
  }

  /* Segmented comparison control — quiet until it matters. */
  .modes {
    flex: none;
    display: flex;
    gap: 1px;
    background: var(--edge);
    border-radius: 5px;
    overflow: hidden;
  }

  .mode {
    appearance: none;
    border: none;
    background: var(--term-bg);
    font: inherit;
    font-size: 0.66rem;
    color: var(--muted);
    cursor: pointer;
    padding: 0.16rem 0.5rem;
    transition:
      background-color 0.1s ease,
      color 0.1s ease;
  }
  .mode:hover {
    background: var(--row-hover);
    color: var(--fg);
  }
  .mode.on {
    background: var(--row-active);
    color: var(--fg);
  }

  .merge-host {
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }
  .merge-host.hidden {
    display: none;
  }

  .msg {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 0.74rem;
    color: var(--muted);
  }
  .msg .err {
    color: var(--err);
  }

  /* MergeView is the shared scroll container. Its editors must stay
     auto-height so their full content contributes to its scroll range. */
  .merge-host :global(.cm-mergeView) {
    height: 100%;
    overflow: auto;
  }
  .merge-host :global(.cm-mergeViewEditors) {
    min-height: 100%;
  }
  .merge-host :global(.cm-merge-a),
  .merge-host :global(.cm-merge-b) {
    min-width: 0;
  }

  /* Change highlighting in the semantic git tints (both schemes via tokens). */
  .merge-host :global(.cm-changedLine) {
    background: color-mix(in srgb, var(--git-modified) 9%, transparent);
  }
  .merge-host :global(.cm-changedText) {
    background: color-mix(in srgb, var(--git-modified) 24%, transparent);
  }
  .merge-host :global(.cm-deletedChunk) {
    background: color-mix(in srgb, var(--git-deleted) 9%, transparent);
  }
  .merge-host :global(.cm-changeGutter) {
    background: transparent;
  }
  .merge-host :global(.cm-collapsedLines) {
    color: var(--muted);
    background: color-mix(in srgb, var(--fg) 3%, transparent);
    font-family: var(--mono);
    font-size: 0.66rem;
  }
</style>
