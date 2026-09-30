<script lang="ts">
  /**
   * Both versions of one kept file side by side, differences highlighted:
   * this computer's on the left, the cloud's on the right. Read-only
   * @codemirror/merge, the renderer the git diff and the editor's compare
   * view use, with unchanged runs folded. The parent keys this component on
   * the pair, so a new pair mounts a fresh view.
   *
   * The MergeView instance is plain, never $state (the CodeView/DiffView rule).
   */
  import { onMount } from "svelte";
  import { EditorState, StateEffect } from "@codemirror/state";
  import { EditorView, highlightSpecialChars, lineNumbers } from "@codemirror/view";
  import { MergeView } from "@codemirror/merge";
  import { LanguageDescription, syntaxHighlighting } from "@codemirror/language";
  import { languages } from "../previews/languages";
  import { codeHighlight, makeCodeTheme } from "../previews/cm";
  import { basename } from "../previews/files";
  import { getSetting } from "../settings/store.svelte";

  interface Props {
    /** The file's project-relative path (for the language mode). */
    path: string;
    mine: string;
    cloud: string;
  }

  let { path, mine, cloud }: Props = $props();

  let host = $state<HTMLDivElement | null>(null);

  function sideExtensions() {
    return [
      highlightSpecialChars(),
      syntaxHighlighting(codeHighlight, { fallback: true }),
      makeCodeTheme(getSetting("editor.fontSize"), getSetting("editor.lineHeight")),
      getSetting("editor.lineNumbers") ? lineNumbers() : [],
      // Prose and data files read better wrapped in half a pane.
      EditorView.lineWrapping,
      EditorState.tabSize.of(getSetting("editor.tabSize")),
      EditorState.readOnly.of(true),
      EditorView.editable.of(false),
    ];
  }

  onMount(() => {
    const el = host;
    if (el === null) return;
    const view = new MergeView({
      a: { doc: mine, extensions: sideExtensions() },
      b: { doc: cloud, extensions: sideExtensions() },
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
          // The language pack failed to load; plain text reads fine.
        });
    }
    return () => {
      live = false;
      view.destroy();
    };
  });
</script>

<div class="merge-host" bind:this={host}></div>

<style>
  .merge-host {
    height: 100%;
    min-height: 0;
    overflow: hidden;
    background: var(--term-bg);
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
  /* Neither side is "the old one": both carry the same quiet change tint
     (the editor's compare view uses it too), so the choice stays open. */
  .merge-host :global(.cm-changedLine) {
    background: color-mix(in srgb, var(--git-modified) 9%, transparent);
  }
  .merge-host :global(.cm-changedText) {
    background: color-mix(in srgb, var(--git-modified) 26%, transparent);
  }
  .merge-host :global(.cm-deletedChunk) {
    background: color-mix(in srgb, var(--git-modified) 9%, transparent);
  }
  .merge-host :global(.cm-changeGutter) {
    background: transparent;
  }
  .merge-host :global(.cm-changedLineGutter) {
    background: var(--git-modified);
  }
  .merge-host :global(.cm-collapsedLines) {
    color: var(--muted);
    background: color-mix(in srgb, var(--fg) 3%, transparent);
    font-family: var(--mono);
    font-size: var(--text-xs);
  }
</style>
