<script lang="ts">
  /**
   * A read-only side-by-side comparison of two texts of one file (@codemirror/
   * merge, the same renderer as the git diff) layered over an editor: the
   * conflict bar's "compare" (my edits vs the disk) and the merge notice's
   * "view diff" (before vs after the merge). Actions ride in the header so a
   * decision can be taken while looking at the difference. Loaded on demand —
   * CodeView imports it only when a comparison opens.
   *
   * The MergeView instance is plain, never $state (the CodeView/DiffView rule).
   */
  import { onMount, type Snippet } from "svelte";
  import { EditorState, StateEffect } from "@codemirror/state";
  import { EditorView, highlightSpecialChars, lineNumbers } from "@codemirror/view";
  import { MergeView } from "@codemirror/merge";
  import { LanguageDescription, syntaxHighlighting } from "@codemirror/language";
  import { languages } from "@codemirror/language-data";
  import { codeHighlight, makeCodeTheme } from "./cm";
  import { basename } from "./files";
  import { getSetting } from "../settings/store.svelte";

  interface Props {
    path: string;
    title: string;
    a: string;
    b: string;
    aLabel: string;
    bLabel: string;
    onClose(): void;
    /** Header buttons (e.g. keep mine / take disk). */
    actions?: Snippet;
  }

  let { path, title, a, b, aLabel, bLabel, onClose, actions }: Props = $props();

  let host = $state<HTMLDivElement | null>(null);

  function sideExtensions() {
    return [
      highlightSpecialChars(),
      syntaxHighlighting(codeHighlight, { fallback: true }),
      makeCodeTheme(getSetting("editor.fontSize"), getSetting("editor.lineHeight")),
      getSetting("editor.lineNumbers") ? lineNumbers() : [],
      getSetting("editor.wordWrap") ? EditorView.lineWrapping : [],
      EditorState.tabSize.of(getSetting("editor.tabSize")),
      EditorState.readOnly.of(true),
      EditorView.editable.of(false),
    ];
  }

  onMount(() => {
    const el = host;
    if (el === null) return;
    const view = new MergeView({
      a: { doc: a, extensions: sideExtensions() },
      b: { doc: b, extensions: sideExtensions() },
      parent: el,
      collapseUnchanged: { margin: 3, minSize: 6 },
      highlightChanges: true,
      gutter: true,
    });
    let live = true;
    const desc = LanguageDescription.matchFilename(languages, basename(path));
    if (desc !== null) {
      void desc
        .load()
        .then((support) => {
          if (!live) return;
          view.a.dispatch({ effects: StateEffect.appendConfig.of(support) });
          view.b.dispatch({ effects: StateEffect.appendConfig.of(support) });
        })
        .catch(() => {
          // language pack failed to load; plain text is fine
        });
    }
    return () => {
      live = false;
      view.destroy();
    };
  });
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
  class="compare"
  role="dialog"
  aria-label={title}
  tabindex="-1"
  onkeydown={(e) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      onClose();
    }
  }}
>
  <header class="cbar">
    <span class="ctitle">{title}</span>
    <span class="clabels"><span class="side">{aLabel}</span> ↔ <span class="side">{bLabel}</span></span>
    <span class="spacer"></span>
    {#if actions !== undefined}{@render actions()}{/if}
    <button class="cbtn" onclick={onClose} title="close (Esc)">close</button>
  </header>
  <div class="merge-host" bind:this={host}></div>
</div>

<style>
  .compare {
    position: absolute;
    inset: 0;
    z-index: 3;
    display: flex;
    flex-direction: column;
    background: var(--term-bg);
    outline: none;
  }

  .cbar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    height: 30px;
    padding: 0 0.7rem;
    border-bottom: 1px solid var(--edge);
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .ctitle {
    color: var(--fg);
    font-weight: 500;
    white-space: nowrap;
  }

  .clabels {
    font-size: var(--text-xs);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .side {
    font-family: var(--mono);
  }

  .spacer {
    flex: 1;
  }

  .cbtn,
  .compare :global(.cbtn) {
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--term-bg);
    font: inherit;
    font-size: var(--text-sm);
    color: var(--fg);
    cursor: pointer;
    padding: 0.1rem 0.5rem;
    border-radius: 4px;
  }

  .cbtn:hover,
  .compare :global(.cbtn:hover) {
    background: var(--row-hover);
  }

  .merge-host {
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }

  /* MergeView is the shared scroll container (see DiffView). */
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
    font-size: var(--text-xs);
  }
</style>
