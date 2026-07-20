<script lang="ts">
  /**
   * The raw settings.json editor: the same file the daemon persists at
   * ~/.config/chimaera/settings.json, edited with schema-driven lint
   * (unknown keys, type mismatches, parse errors) and key completion.
   * Cmd/Ctrl+S (or Save) PUTs the whole map; external changes refresh the
   * buffer while it is not dirty.
   */
  import { onMount, untrack } from "svelte";
  import { Compartment, EditorState } from "@codemirror/state";
  import { EditorView, keymap, lineNumbers, drawSelection } from "@codemirror/view";
  import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
  import {
    bracketMatching,
    indentUnit,
    syntaxHighlighting,
    HighlightStyle,
  } from "@codemirror/language";
  import { json as jsonLanguage, jsonParseLinter } from "@codemirror/lang-json";
  import { linter, lintGutter, type Diagnostic } from "@codemirror/lint";
  import {
    autocompletion,
    completeFromList,
    type Completion,
    type CompletionContext,
    type CompletionResult,
  } from "@codemirror/autocomplete";
  import { tags as t } from "@lezer/highlight";
  import { expectedType, sanitize, settingDef, SETTINGS } from "./schema";
  import { getSetting, rawUserSettings, replaceSettings } from "./store.svelte";
  import { isMac } from "../shared/keys";

  const SAVE_HINT = isMac ? "⌘S to save" : "Ctrl+S to save";

  let host = $state<HTMLDivElement | null>(null);
  let view: EditorView | null = null;
  let dirty = $state(false);
  let saveError = $state<string | null>(null);
  let savedFlash = $state(false);
  let flashTimer: ReturnType<typeof setTimeout> | null = null;
  const settingsCompartment = new Compartment();

  function currentText(): string {
    const map = rawUserSettings();
    return Object.keys(map).length === 0 ? "{\n  \n}\n" : JSON.stringify(map, null, 2) + "\n";
  }

  /** Schema lint on top of the parse lint: unknown keys and type mismatches. */
  function schemaLint(v: EditorView): Diagnostic[] {
    const text = v.state.doc.toString();
    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch {
      return []; // jsonParseLinter owns syntax errors
    }
    const out: Diagnostic[] = [];
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
      out.push({
        from: 0,
        to: text.length,
        severity: "error",
        message: "settings.json must be a JSON object of dotted keys",
      });
      return out;
    }
    for (const [key, value] of Object.entries(parsed)) {
      // Locate the key in source for a precise squiggle (first occurrence).
      const needle = JSON.stringify(key);
      const at = text.indexOf(needle);
      const from = at >= 0 ? at : 0;
      const to = at >= 0 ? at + needle.length : 0;
      const def = settingDef(key);
      if (def === undefined) {
        out.push({
          from,
          to,
          severity: "warning",
          message: `Unknown setting "${key}" (kept verbatim, has no effect)`,
        });
      } else if (sanitize(def, value) === null) {
        out.push({
          from,
          to,
          severity: "error",
          message: `"${key}" expects ${expectedType(def)}`,
        });
      }
    }
    return out;
  }

  /** Complete setting ids inside property-name position. */
  function keyCompletions(
    ctx: CompletionContext,
  ): CompletionResult | Promise<CompletionResult | null> | null {
    const word = ctx.matchBefore(/"[\w.]*$/);
    if (word === null && !ctx.explicit) return null;
    const options: Completion[] = SETTINGS.map((def) => ({
      label: `"${def.id}"`,
      type: "property",
      detail: def.type,
      info: `${def.description} Default: ${JSON.stringify(def.default)}`,
      apply: `"${def.id}": ${JSON.stringify(def.default)}`,
    }));
    return completeFromList(options)(ctx);
  }

  const highlight = HighlightStyle.define([
    { tag: t.propertyName, color: "var(--syn-prop)" },
    { tag: t.string, color: "var(--syn-string)" },
    { tag: [t.number, t.bool, t.null], color: "var(--syn-number)" },
    { tag: t.invalid, color: "var(--err)" },
  ]);

  const makeTheme = () =>
    EditorView.theme({
      "&": {
        backgroundColor: "transparent",
        color: "var(--fg)",
        height: "100%",
        fontSize: `${getSetting("editor.fontSize")}px`,
      },
      ".cm-scroller": {
        fontFamily: "var(--editor-font)",
        lineHeight: `${getSetting("editor.lineHeight")}`,
        overflow: "auto",
      },
      ".cm-content": { padding: "10px 0 14px" },
      ".cm-line": { padding: "0 14px 0 8px" },
      ".cm-gutters": {
        backgroundColor: "transparent",
        color: "var(--muted)",
        border: "none",
        fontFamily: "var(--editor-font)",
        fontSize: `${Math.max(9, getSetting("editor.fontSize") - 1.5)}px`,
        opacity: "0.65",
        userSelect: "none",
      },
      "&.cm-focused": { outline: "none" },
      ".cm-cursor": { borderLeftColor: "var(--accent)" },
      ".cm-selectionBackground, &.cm-focused .cm-selectionBackground, ::selection": {
        backgroundColor: "var(--term-selection)",
      },
      ".cm-tooltip": {
        backgroundColor: "var(--overlay-bg)",
        border: "1px solid var(--edge)",
        borderRadius: "7px",
        color: "var(--fg)",
        fontSize: "var(--text-sm)",
      },
      ".cm-tooltip.cm-tooltip-autocomplete > ul > li[aria-selected]": {
        backgroundColor: "var(--row-active)",
        color: "var(--fg)",
      },
      ".cm-completionInfo": {
        backgroundColor: "var(--overlay-bg)",
        border: "1px solid var(--edge)",
        borderRadius: "7px",
        maxWidth: "44ch",
      },
    });

  function editorSettings() {
    const tabSize = getSetting("editor.tabSize");
    return [
      makeTheme(),
      getSetting("editor.lineNumbers") ? lineNumbers() : [],
      getSetting("editor.wordWrap") ? EditorView.lineWrapping : [],
      EditorState.tabSize.of(tabSize),
      indentUnit.of(" ".repeat(tabSize)),
    ];
  }

  onMount(() => {
    const el = host;
    if (el === null) return;
    const state = EditorState.create({
      doc: currentText(),
      extensions: [
        drawSelection(),
        history(),
        bracketMatching(),
        jsonLanguage(),
        syntaxHighlighting(highlight, { fallback: true }),
        linter(jsonParseLinter()),
        linter(schemaLint),
        lintGutter(),
        autocompletion({ override: [keyCompletions] }),
        settingsCompartment.of(editorSettings()),
        keymap.of([
          { key: "Mod-s", run: () => (save(), true), preventDefault: true },
          indentWithTab,
          ...defaultKeymap,
          ...historyKeymap,
        ]),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) {
            dirty = true;
            savedFlash = false;
            saveError = null;
          }
        }),
      ],
    });
    const v = new EditorView({ state, parent: el });
    view = v;
    return () => {
      view = null;
      if (flashTimer !== null) clearTimeout(flashTimer);
      v.destroy();
    };
  });

  // External changes (another window, hand-edit on disk) refresh the buffer
  // while it holds no local edits.
  $effect(() => {
    const text = currentText();
    untrack(() => {
      const v = view;
      if (v === null || dirty) return;
      if (v.state.doc.toString() !== text) {
        v.dispatch({ changes: { from: 0, to: v.state.doc.length, insert: text } });
        dirty = false;
      }
    });
  });

  // Every Editor setting applies to this editor too — not just font size.
  $effect(() => {
    const settings = editorSettings();
    if (view !== null) {
      view.dispatch({ effects: settingsCompartment.reconfigure(settings) });
    }
  });

  function save(): void {
    const v = view;
    if (v === null) return;
    let parsed: unknown;
    try {
      parsed = JSON.parse(v.state.doc.toString());
    } catch (e) {
      saveError = e instanceof Error ? e.message : "not valid JSON";
      return;
    }
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
      saveError = "settings.json must be a JSON object";
      return;
    }
    replaceSettings(parsed as Record<string, unknown>);
    dirty = false;
    saveError = null;
    savedFlash = true;
    if (flashTimer !== null) clearTimeout(flashTimer);
    flashTimer = setTimeout(() => (savedFlash = false), 1600);
  }
</script>

<div class="json-view">
  <div class="editor" bind:this={host}></div>
  <footer class="bar">
    <span class="status">
      {#if dirty}unsaved{:else if savedFlash}saved{:else}synced{/if}
    </span>
    {#if saveError !== null}
      <span class="bar-err">{saveError}</span>
    {/if}
    <span class="spacer"></span>
    {#if dirty}
      <button class="save-btn" onclick={save}>save</button>
    {/if}
    <span class="hint">{SAVE_HINT}</span>
  </footer>
</div>

<style>
  .json-view {
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

  .save-btn {
    appearance: none;
    border: 1px solid var(--edge);
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--fg);
    cursor: pointer;
    padding: 0.05rem 0.5rem;
    border-radius: 4px;
  }

  .save-btn:hover {
    background: var(--row-hover);
    border-color: color-mix(in srgb, var(--accent) 45%, var(--edge));
  }

  .hint {
    font-family: var(--mono);
    opacity: 0.7;
  }
</style>
