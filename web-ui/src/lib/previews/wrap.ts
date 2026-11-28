/**
 * Soft wrap for every CodeMirror surface (file editor, diff, compare, the
 * settings JSON), from the one `editor.wordWrap` setting. A wrapped line's
 * continuation rows hang under its first non-blank character, as VS Code's
 * default `wrappingIndent: "same"` does: plain `lineWrapping` alone sends them
 * back to column 0, which breaks an indented block's shape and reads as new,
 * unindented lines.
 *
 * The indent is a per-line padding + negative text-indent pair, so the first
 * row stays where its whitespace puts it and only the rows after it move.
 * Built for visible lines only. A surface that styles its own lines (the
 * markdown live view) sets `ownLineLayout` and keeps plain wrapping.
 */
import { Facet, type Extension } from "@codemirror/state";
import {
  Decoration,
  EditorView,
  ViewPlugin,
  type DecorationSet,
  type ViewUpdate,
} from "@codemirror/view";

/** The left padding every code theme gives `.cm-line` (`makeCodeTheme`,
 *  the settings JSON theme); the hanging indent adds to it. */
export const LINE_PAD_LEFT_PX = 8;

/** Columns of a line's leading whitespace (a tab runs to the next stop), or
 *  0 for a blank or whitespace-only line, which has nothing to hang. */
export function indentColumns(text: string, tabSize: number): number {
  let col = 0;
  for (let i = 0; i < text.length; i++) {
    const ch = text.charCodeAt(i);
    if (ch === 32) col++;
    else if (ch === 9) col += tabSize - (col % tabSize);
    else return col;
  }
  return 0;
}

/** At most half the wrap width hangs: past that the continuation rows would
 *  be a sliver (deep nesting in a narrow pane), so they stop at the middle. */
function maxHang(view: EditorView): number {
  const width = view.contentDOM.clientWidth - LINE_PAD_LEFT_PX;
  const cw = view.defaultCharacterWidth;
  return width > 0 && cw > 0 ? Math.max(0, Math.floor(width / cw / 2)) : 0;
}

/** Set by a surface that lays out its own lines (the markdown live view):
 *  it keeps `lineWrapping` but draws no hanging indent. */
export const ownLineLayout = Facet.define<boolean, boolean>({
  combine: (v) => v.some(Boolean),
});

/** Shared per indent width, so an unchanged rebuild compares equal. The
 *  width is the editor's measured character width, not CSS `ch` (the "0"
 *  glyph), which drifts from a space's advance in some editor fonts and
 *  would leave continuation rows a fraction of a column off. */
const hangCache = new Map<string, Decoration>();
function hang(px: number): Decoration {
  const w = px.toFixed(2);
  let deco = hangCache.get(w);
  if (deco === undefined) {
    deco = Decoration.line({
      attributes: {
        style: `padding-left: ${(LINE_PAD_LEFT_PX + px).toFixed(2)}px; text-indent: -${w}px`,
      },
    });
    hangCache.set(w, deco);
  }
  return deco;
}

function build(view: EditorView): DecorationSet {
  if (view.state.facet(ownLineLayout)) return Decoration.none;
  const cap = maxHang(view);
  if (cap === 0) return Decoration.none;
  const cw = view.defaultCharacterWidth;
  const { doc, tabSize } = view.state;
  const out = [];
  for (const { from, to } of view.visibleRanges) {
    for (let pos = from; pos <= to; ) {
      const line = doc.lineAt(pos);
      const cols = Math.min(indentColumns(line.text, tabSize), cap);
      if (cols > 0) out.push(hang(cols * cw).range(line.from));
      pos = line.to + 1;
    }
  }
  return Decoration.set(out);
}

const hangingIndent = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet;
    constructor(view: EditorView) {
      this.decorations = build(view);
    }
    update(u: ViewUpdate) {
      // Decorations are shared per width, so the rebuild after the geometry
      // change a hang itself causes yields equal ones and settles.
      if (
        u.docChanged ||
        u.viewportChanged ||
        u.geometryChanged ||
        u.startState.tabSize !== u.state.tabSize ||
        u.startState.facet(ownLineLayout) !== u.state.facet(ownLineLayout)
      )
        this.decorations = build(u.view);
    }
  },
  { decorations: (v) => v.decorations },
);

/** Soft wrap with hanging indent when `on`, else nothing (scroll sideways). */
export function codeWrap(on: boolean): Extension {
  return on ? [EditorView.lineWrapping, hangingIndent] : [];
}
