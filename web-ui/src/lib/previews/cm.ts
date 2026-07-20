/**
 * Shared CodeMirror 6 chrome — the syntax highlight style and editor theme —
 * used by both the file editor (CodeView) and the side-by-side diff (DiffView),
 * so highlighted code reads identically whether you're editing or reviewing.
 * All colors are theme tokens (var(--syn-*)/var(--fg)…), so light/dark just work.
 */
import { EditorView } from "@codemirror/view";
import { HighlightStyle } from "@codemirror/language";
import { tags as t } from "@lezer/highlight";

/** Syntax highlight mapping onto the app's --syn-* tokens. */
export const codeHighlight = HighlightStyle.define([
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

/** Editor theme (transparent surface, mono gutters) at a given size/line-height. */
export const makeCodeTheme = (fontSize: number, lineHeight: number) =>
  EditorView.theme({
    "&": {
      backgroundColor: "transparent",
      color: "var(--fg)",
      height: "100%",
      fontSize: `${fontSize}px`,
    },
    ".cm-scroller": {
      fontFamily: "var(--editor-font)",
      lineHeight: `${lineHeight}`,
      overflow: "auto",
    },
    ".cm-content": {
      padding: "10px 0 14px",
    },
    ".cm-line": {
      padding: "0 14px 0 8px",
    },
    ".cm-gutters": {
      backgroundColor: "transparent",
      color: "var(--muted)",
      border: "none",
      fontFamily: "var(--editor-font)",
      fontSize: `${Math.max(9, Math.round(fontSize - 1.5))}px`,
      opacity: "0.65",
      userSelect: "none",
    },
    ".cm-lineNumbers .cm-gutterElement": {
      padding: "0 6px 0 14px",
      minWidth: "3ch",
    },
    ".cm-activeLine": { backgroundColor: "color-mix(in srgb, var(--fg) 3%, transparent)" },
    ".cm-activeLineGutter": { backgroundColor: "transparent" },
    "&.cm-focused": { outline: "none" },
    ".cm-cursor": { borderLeftColor: "var(--accent)" },
    ".cm-selectionBackground, &.cm-focused .cm-selectionBackground, ::selection": {
      backgroundColor: "var(--term-selection)",
    },
    ".cm-matchingBracket": {
      backgroundColor: "color-mix(in srgb, var(--accent) 16%, transparent)",
      outline: "none",
    },
  });
