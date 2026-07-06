<script lang="ts">
  /**
   * Read-only CodeMirror 6 view for code/text files. The parent (FileView)
   * fetched — and binary-sniffed — the first 256KB chunk; this component
   * owns the editor and the quiet "load more" tail for truncated files.
   * The editor instance is plain module state, never $state (same rule as
   * xterm instances in termPool).
   */
  import { onMount } from "svelte";
  import { EditorState, StateEffect } from "@codemirror/state";
  import { EditorView, lineNumbers, highlightSpecialChars } from "@codemirror/view";
  import {
    LanguageDescription,
    syntaxHighlighting,
    HighlightStyle,
    bracketMatching,
  } from "@codemirror/language";
  import { languages } from "@codemirror/language-data";
  import { tags as t } from "@lezer/highlight";
  import { basename, fsFile, humanSize, FILE_CHUNK, type FileChunk } from "./files";

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

  let view: EditorView | null = null;
  // Streaming decoder: chunk boundaries may split a UTF-8 sequence; the
  // decoder carries the partial bytes across load-more calls.
  const decoder = new TextDecoder("utf-8", { fatal: false });

  // Syntax colors ride on CSS variables (app.css), so one style serves both
  // light and dark schemes.
  const highlight = HighlightStyle.define([
    { tag: [t.keyword, t.operatorKeyword, t.modifier, t.self], color: "var(--syn-keyword)" },
    { tag: [t.string, t.special(t.string), t.character], color: "var(--syn-string)" },
    { tag: [t.regexp, t.escape], color: "var(--syn-string)" },
    { tag: [t.comment, t.lineComment, t.blockComment], color: "var(--syn-comment)", fontStyle: "italic" },
    { tag: [t.number, t.integer, t.float, t.bool, t.null, t.atom], color: "var(--syn-number)" },
    { tag: [t.typeName, t.className, t.namespace, t.macroName], color: "var(--syn-type)" },
    { tag: [t.function(t.variableName), t.function(t.propertyName)], color: "var(--syn-func)" },
    { tag: [t.definition(t.variableName), t.constant(t.variableName)], color: "var(--syn-def)" },
    { tag: t.propertyName, color: "var(--syn-prop)" },
    { tag: [t.tagName, t.angleBracket], color: "var(--syn-type)" },
    { tag: [t.attributeName], color: "var(--syn-prop)" },
    { tag: t.heading, fontWeight: "600", color: "var(--fg)" },
    { tag: [t.link, t.url], color: "var(--accent)" },
    { tag: t.emphasis, fontStyle: "italic" },
    { tag: t.strong, fontWeight: "600" },
    { tag: t.invalid, color: "var(--err)" },
  ]);

  const theme = EditorView.theme({
    "&": {
      backgroundColor: "transparent",
      color: "var(--fg)",
      height: "100%",
      fontSize: "12.5px",
    },
    ".cm-scroller": {
      fontFamily: "var(--mono)",
      lineHeight: "1.55",
      overflow: "auto",
    },
    ".cm-content": {
      padding: "10px 0 14px",
      caretColor: "transparent",
    },
    ".cm-line": {
      padding: "0 14px 0 8px",
    },
    ".cm-gutters": {
      backgroundColor: "transparent",
      color: "var(--muted)",
      border: "none",
      fontFamily: "var(--mono)",
      fontSize: "11px",
      opacity: "0.65",
      userSelect: "none",
    },
    ".cm-lineNumbers .cm-gutterElement": {
      padding: "0 6px 0 14px",
      minWidth: "3ch",
    },
    "&.cm-focused": { outline: "none" },
    ".cm-selectionBackground, &.cm-focused .cm-selectionBackground, ::selection": {
      backgroundColor: "var(--term-selection)",
    },
    ".cm-matchingBracket": {
      backgroundColor: "color-mix(in srgb, var(--accent) 16%, transparent)",
      outline: "none",
    },
  });

  onMount(() => {
    const el = host;
    if (el === null) return;
    const text = decoder.decode(first.bytes, { stream: true });
    loadedBytes = first.bytes.length;
    totalBytes = first.size;
    truncated = first.truncated;

    const state = EditorState.create({
      doc: text,
      extensions: [
        lineNumbers(),
        highlightSpecialChars(),
        bracketMatching(),
        syntaxHighlighting(highlight, { fallback: true }),
        theme,
        EditorState.readOnly.of(true),
        EditorView.editable.of(false),
      ],
    });
    const v = new EditorView({ state, parent: el });
    view = v;

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
      v.destroy();
    };
  });

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
</script>

<div class="code-view">
  <div class="editor" bind:this={host}></div>
  {#if truncated}
    <footer class="more">
      <span>showing {humanSize(loadedBytes)} of {humanSize(totalBytes)}</span>
      {#if loadError !== null}
        <span class="more-err">{loadError}</span>
      {/if}
      <button class="more-btn" disabled={loadingMore} onclick={() => void loadMore()}>
        {loadingMore ? "loading…" : "load more"}
      </button>
    </footer>
  {/if}
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

  .more {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    height: 28px;
    padding: 0 0.7rem;
    border-top: 1px solid var(--edge);
    font-size: 0.68rem;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }

  .more-err {
    color: var(--err);
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
    margin-left: auto;
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
