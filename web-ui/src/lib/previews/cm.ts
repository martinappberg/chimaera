/**
 * Shared CodeMirror 6 chrome — the syntax highlight style and editor theme —
 * used by both the file editor (CodeView) and the side-by-side diff (DiffView),
 * so highlighted code reads identically whether you're editing or reviewing.
 * All colors are theme tokens (var(--syn-*)/var(--fg)…), so light/dark just work.
 */
import { EditorSelection, StateEffect, StateField, type Extension, type Range } from "@codemirror/state";
import { Decoration, EditorView, type DecorationSet } from "@codemirror/view";
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

/**
 * The @codemirror/search panel (Mod-f) and its match marks, in the app's
 * quiet chrome: tokens only, so it holds in light and dark and inside the
 * markdown live surface.
 */
export const codeSearchTheme = EditorView.theme({
  ".cm-panels": {
    backgroundColor: "var(--term-bg)",
    color: "var(--fg)",
  },
  ".cm-panels.cm-panels-top": { borderBottom: "1px solid var(--edge)" },
  ".cm-panels.cm-panels-bottom": { borderTop: "1px solid var(--edge)" },
  ".cm-search": {
    display: "flex",
    flexWrap: "wrap",
    alignItems: "center",
    gap: "4px 6px",
    padding: "5px 30px 5px 10px",
    fontFamily: "var(--ui-font)",
    fontSize: "var(--text-sm)",
  },
  ".cm-search br": { display: "none" },
  ".cm-search label": {
    display: "inline-flex",
    alignItems: "center",
    gap: "3px",
    color: "var(--muted)",
    fontSize: "var(--text-xs)",
  },
  ".cm-textfield": {
    font: "inherit",
    fontFamily: "var(--editor-font)",
    fontSize: "var(--text-sm)",
    color: "var(--fg)",
    backgroundColor: "var(--bg)",
    border: "1px solid var(--edge)",
    borderRadius: "4px",
    padding: "1px 6px",
    margin: "0",
    outline: "none",
  },
  ".cm-textfield:focus": { borderColor: "color-mix(in srgb, var(--accent) 55%, var(--edge))" },
  ".cm-button": {
    font: "inherit",
    fontSize: "var(--text-xs)",
    color: "var(--fg)",
    backgroundImage: "none",
    backgroundColor: "var(--term-bg)",
    border: "1px solid var(--edge)",
    borderRadius: "4px",
    padding: "1px 7px",
    margin: "0",
    cursor: "pointer",
  },
  ".cm-button:hover": { backgroundColor: "var(--row-hover)" },
  ".cm-search [name=close]": {
    color: "var(--muted)",
    fontSize: "var(--text-md)",
    cursor: "pointer",
    top: "4px",
    right: "8px",
  },
  ".cm-searchMatch": {
    backgroundColor: "color-mix(in srgb, var(--warn) 22%, transparent)",
    outline: "1px solid color-mix(in srgb, var(--warn) 45%, transparent)",
  },
  ".cm-searchMatch.cm-searchMatch-selected": {
    backgroundColor: "color-mix(in srgb, var(--accent) 32%, transparent)",
  },
});

/** Flash these lines (`from`..`to` are positions on the first and last); null clears. */
const flashEffect = StateEffect.define<{ from: number; to: number } | null>();
const flashLine = Decoration.line({ class: "cm-reveal-flash" });
/** A reveal of a whole chapter should not decorate thousands of lines. */
const FLASH_MAX_LINES = 400;

const flashField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    let next = deco.map(tr.changes);
    for (const e of tr.effects) {
      if (!e.is(flashEffect)) continue;
      if (e.value === null) {
        next = Decoration.none;
        continue;
      }
      const doc = tr.state.doc;
      const first = doc.lineAt(Math.min(e.value.from, doc.length)).number;
      const last = Math.min(doc.lineAt(Math.min(e.value.to, doc.length)).number, first + FLASH_MAX_LINES);
      const ranges: Range<Decoration>[] = [];
      for (let n = first; n <= last; n++) ranges.push(flashLine.range(doc.line(n).from));
      next = Decoration.set(ranges);
    }
    return next;
  },
  provide: (f) => EditorView.decorations.from(f),
});

/** The reveal-flash state + its look; include once per editor. */
export const revealFlash: Extension = [
  flashField,
  EditorView.theme({
    ".cm-reveal-flash": {
      backgroundColor: "color-mix(in srgb, var(--accent) 16%, transparent)",
    },
  }),
];

/** How long a revealed range stays lit. */
export const REVEAL_FLASH_MS = 1600;

/**
 * Put the cursor at 1-based `line`/`col`, scroll `line`..`endLine` to the
 * middle of the view and light it. Out-of-range numbers clamp to the document
 * (the file may have changed since the link was written). The caller clears
 * the flash with `clearRevealFlash` after REVEAL_FLASH_MS.
 */
export function revealLines(
  view: EditorView,
  r: { line: number; endLine?: number; col?: number },
): void {
  const doc = view.state.doc;
  const clamp = (n: number) => Math.min(Math.max(1, Math.floor(n) || 1), doc.lines);
  const start = doc.line(clamp(r.line));
  const end = doc.line(Math.max(start.number, clamp(r.endLine ?? r.line)));
  const col = Math.max(1, Math.floor(r.col ?? 1) || 1);
  const pos = Math.min(start.from + col - 1, start.to);
  view.dispatch({
    selection: { anchor: pos },
    effects: [
      EditorView.scrollIntoView(EditorSelection.range(start.from, end.to), { y: "center" }),
      flashEffect.of({ from: start.from, to: end.from }),
    ],
  });
}

export function clearRevealFlash(view: EditorView): void {
  view.dispatch({ effects: flashEffect.of(null) });
}
