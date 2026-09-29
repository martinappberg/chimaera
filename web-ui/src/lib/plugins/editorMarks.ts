/**
 * A plugin's problems as marks in the editor (docs/plugin-platform-plan.md
 * §4; the LaTeX plan's "errors as editor marks"): every active plugin's
 * `diagnostics/1` items for this file, drawn by `@codemirror/lint` as gutter
 * marks and underlines. Fetched when the editor opens and again on each
 * `diagnostics` surface frame for the workspace; CodeMirror maps them
 * through later edits, and the next publish replaces them. Errors and
 * warnings only: `info` and `hint` (a LaTeX overfull box) stay in the
 * problems list, where they are filtered, not in the text.
 */
import type { Extension, Text } from "@codemirror/state";
import { EditorView, ViewPlugin, type PluginValue } from "@codemirror/view";
import { lintGutter, setDiagnostics, type Diagnostic as Mark } from "@codemirror/lint";
import { fetchDiagnostics, onPlatformFrame, type Diagnostic } from "./platform";

/** `diagnostics/1` items → marks in `doc` (lines and columns from 1). An
 *  item past the end of the text (the file changed since the build) is
 *  dropped, never clamped onto another line. */
export function toMarks(doc: Text, items: readonly Diagnostic[]): Mark[] {
  const out: Mark[] = [];
  for (const d of items) {
    if (d.severity !== "error" && d.severity !== "warning") continue;
    if (!Number.isInteger(d.line) || d.line < 1 || d.line > doc.lines) continue;
    const line = doc.line(d.line);
    const col = typeof d.column === "number" && d.column >= 1 ? d.column : null;
    const from = col !== null ? Math.min(line.from + col - 1, line.to) : line.from;
    let to = line.to;
    if (typeof d.end_line === "number" && d.end_line >= d.line && d.end_line <= doc.lines) {
      const end = doc.line(d.end_line);
      to = typeof d.end_column === "number" && d.end_column >= 1 ? Math.min(end.from + d.end_column - 1, end.to) : end.to;
    }
    out.push({
      from,
      to: Math.max(from, to),
      severity: d.severity,
      message: d.message,
      source: d.source ?? d.plugin,
    });
  }
  return out;
}

class Marks implements PluginValue {
  private seq = 0;
  private gone = false;
  private readonly stop: () => void;

  constructor(
    private readonly view: EditorView,
    private readonly ws: string,
    private readonly file: string,
  ) {
    this.stop = onPlatformFrame((f) => {
      if (f.type === "surface" && f.workspace === ws && typeof f.surface === "string" && f.surface.startsWith("diagnostics")) {
        this.load();
      }
    });
    this.load();
  }

  private load(): void {
    const n = ++this.seq;
    fetchDiagnostics(this.ws, this.file).then(
      (items) => {
        if (this.gone || n !== this.seq) return;
        this.view.dispatch(setDiagnostics(this.view.state, toMarks(this.view.state.doc, items)));
      },
      () => {
        // No marks is the quiet failure: the problems list says why.
      },
    );
  }

  destroy(): void {
    this.gone = true;
    this.stop();
  }
}

/** The app's tokens for lint's gutter, underlines and tooltip (its own
 *  defaults are fixed light-theme colors). */
const marksTheme = EditorView.theme({
  ".cm-lintRange-error": {
    backgroundImage: "none",
    textDecoration: "underline wavy var(--err)",
    textDecorationSkipInk: "none",
    textUnderlineOffset: "3px",
  },
  ".cm-lintRange-warning": {
    backgroundImage: "none",
    textDecoration: "underline wavy var(--warn)",
    textDecorationSkipInk: "none",
    textUnderlineOffset: "3px",
  },
  ".cm-tooltip.cm-tooltip-lint": {
    background: "var(--bg)",
    color: "var(--fg)",
    border: "1px solid var(--edge)",
    borderRadius: "6px",
  },
  ".cm-diagnostic": { padding: "4px 8px", fontSize: "var(--text-sm)" },
  ".cm-diagnostic-error": { borderLeft: "3px solid var(--err)" },
  ".cm-diagnostic-warning": { borderLeft: "3px solid var(--warn)" },
  ".cm-diagnosticSource": { color: "var(--muted)", opacity: "1" },
});

/** The marks for `file` (workspace-relative) in workspace `ws`. */
export function pluginMarks(ws: string, file: string): Extension {
  return [lintGutter(), marksTheme, ViewPlugin.define((view) => new Marks(view, ws, file))];
}
