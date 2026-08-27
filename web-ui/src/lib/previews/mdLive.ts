/**
 * Obsidian-style LIVE PREVIEW for markdown files: a CodeMirror extension set
 * that renders formatting inline — headings sized, emphasis styled, syntax
 * marks hidden on lines the selection doesn't touch, images/checkboxes/rules
 * as widgets, blockquotes as the shared quote-card treatment — while the
 * document stays plain editable text underneath. The buffer, save, dirty and
 * conflict machinery all stay in CodeView; this module is decoration-only and
 * never mutates the document except for the task-checkbox toggle (a normal
 * dispatched change, so undo/dirty/save all see it).
 *
 * Reveal rule: marks un-hide per line the selection touches (multi-line
 * elements like fenced code keep their chrome visible but muted), so the
 * cursor always edits real text and nothing is ever atomic-trapped.
 *
 * Sanitization boundary: nothing from the document is ever injected as HTML.
 * Widgets are built via createElement/textContent; the only innerHTML is the
 * shared constant copy-icon SVG. Images resolve like the reading view: web/
 * data URLs pass through, document-relative paths go through a ticketed
 * /raw/ URL (the server canonicalizes and enforces access).
 */
import {
  Decoration,
  EditorView,
  ViewPlugin,
  WidgetType,
  type DecorationSet,
  type ViewUpdate,
} from "@codemirror/view";
import { syntaxTree } from "@codemirror/language";
import type { EditorState, Extension, Line } from "@codemirror/state";
import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { languages } from "@codemirror/language-data";
import type { SyntaxNode, SyntaxNodeRef } from "@lezer/common";
import { rawTicketUrl, resolveDocPath } from "./files";
import { copyText } from "../shared/clipboard";
import { COPY_BUTTON_SVG } from "../shared/copyDecor";
import { activateUrl, isWebUrl } from "../shared/urlOpen";

export interface MarkdownLiveOptions {
  /** Absolute path of the document (image targets resolve against its dir). */
  path: string;
  /** Prose base size (px) — the same value the reading view's body uses. */
  fontSize: number;
  /** Prose line-height — editor.markdownLineHeight. */
  lineHeight: number;
}

// --- widgets -----------------------------------------------------------------

/** `-`/`*`/`+` rendered as the reading view's tinted bullet. */
class BulletWidget extends WidgetType {
  eq(): boolean {
    return true;
  }
  toDOM(): HTMLElement {
    const s = document.createElement("span");
    s.className = "lp-bullet";
    s.textContent = "•";
    return s;
  }
}

/** `---` rendered as a rule when its line is inactive. */
class RuleWidget extends WidgetType {
  eq(): boolean {
    return true;
  }
  toDOM(): HTMLElement {
    const s = document.createElement("span");
    s.className = "lp-hr";
    return s;
  }
}

/** `- [ ]`/`- [x]` as a real checkbox; clicking toggles the SOURCE text (a
 *  normal editor change, so dirty/undo/save all apply). */
class CheckboxWidget extends WidgetType {
  constructor(readonly checked: boolean) {
    super();
  }
  override eq(other: CheckboxWidget): boolean {
    return other.checked === this.checked;
  }
  toDOM(view: EditorView): HTMLElement {
    const box = document.createElement("input");
    box.type = "checkbox";
    box.className = "lp-task";
    box.checked = this.checked;
    // Keep the click from moving the cursor/focus before the toggle lands.
    box.addEventListener("mousedown", (e) => e.preventDefault());
    box.addEventListener("click", (e) => {
      e.preventDefault();
      if (view.state.readOnly) return;
      // The widget replaces the marker, so its DOM position IS the marker's
      // current position — resolved at click time, immune to earlier edits.
      const pos = view.posAtDOM(box);
      const marker = view.state.doc.sliceString(pos, pos + 3);
      const m = /^\[( |x|X)\]$/.exec(marker);
      if (m === null) return;
      view.dispatch({
        changes: { from: pos, to: pos + 3, insert: m[1] === " " ? "[x]" : "[ ]" },
      });
    });
    return box;
  }
  override ignoreEvent(): boolean {
    return true;
  }
}

/** Inline image for `![alt](target)` when its line is inactive. Relative
 *  targets ride the same ticketed /raw/ URLs as the reading view (memoized in
 *  files.ts, so decoration rebuilds keep the src stable — no flash). */
class ImageWidget extends WidgetType {
  constructor(
    readonly target: string,
    readonly alt: string,
    readonly remote: boolean,
  ) {
    super();
  }
  override eq(other: ImageWidget): boolean {
    return other.target === this.target && other.alt === this.alt;
  }
  toDOM(view: EditorView): HTMLElement {
    const img = document.createElement("img");
    img.className = "lp-image";
    img.alt = this.alt;
    // The image height is unknown until load: re-measure so following lines
    // don't overlap while the editor still assumes the estimated height.
    img.addEventListener("load", () => view.requestMeasure());
    if (this.remote) {
      img.src = this.target;
    } else {
      void rawTicketUrl(this.target).then(
        (url) => {
          if (img.isConnected) img.src = url;
        },
        () => {
          // missing/unreadable target: the alt text shows
        },
      );
    }
    return img;
  }
  override get estimatedHeight(): number {
    return 120;
  }
}

/** The copy affordance on fenced code blocks (the reading view's `.md-copy`
 *  language, sized down to sit inline on the fence line). The payload is
 *  resolved from the live syntax tree at click time. */
class FenceCopyWidget extends WidgetType {
  eq(): boolean {
    return true;
  }
  toDOM(view: EditorView): HTMLElement {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "lp-copy";
    btn.setAttribute("aria-label", "copy code");
    btn.title = "copy";
    btn.innerHTML = COPY_BUTTON_SVG; // shared constant markup, never document content
    btn.addEventListener("mousedown", (e) => e.preventDefault());
    btn.addEventListener("click", (e) => {
      e.preventDefault();
      const pos = view.posAtDOM(btn);
      let n: SyntaxNode | null = syntaxTree(view.state).resolveInner(pos, -1);
      while (n !== null && n.name !== "FencedCode") n = n.parent;
      const code = n?.getChild("CodeText") ?? null;
      const payload =
        code === null
          ? ""
          : view.state.doc.sliceString(code.from, code.to).replace(/\s+$/, "");
      if (payload.length === 0) return;
      void copyText(payload).then((ok) => {
        if (!ok || !btn.isConnected) return;
        btn.classList.add("copied");
        btn.setAttribute("aria-label", "copied");
        setTimeout(() => {
          if (!btn.isConnected) return;
          btn.classList.remove("copied");
          btn.setAttribute("aria-label", "copy code");
        }, 1400);
      });
    });
    return btn;
  }
  override ignoreEvent(): boolean {
    return true;
  }
}

const bulletWidget = new BulletWidget();
const ruleWidget = new RuleWidget();
const fenceCopyWidget = new FenceCopyWidget();
const checkedWidget = new CheckboxWidget(true);
const uncheckedWidget = new CheckboxWidget(false);
const hidden = Decoration.replace({});

// --- decoration build --------------------------------------------------------

/** End offset of a leading YAML frontmatter block (0 = none). The markdown
 *  parser has no frontmatter notion — without this, `---` fences would render
 *  as rules and `title:`-then-`---` as a setext heading. Bounded scan. */
function frontmatterEnd(state: EditorState): number {
  const doc = state.doc;
  if (doc.lines < 2 || doc.line(1).text.trim() !== "---") return 0;
  const cap = Math.min(doc.lines, 200);
  for (let i = 2; i <= cap; i++) {
    const t = doc.line(i).text.trim();
    if (t === "---" || t === "...") return doc.line(i).to;
  }
  return 0;
}

function safeDecode(s: string): string {
  try {
    return decodeURI(s);
  } catch {
    return s; // the server rejects what it can't canonicalize
  }
}

function buildDecorations(view: EditorView, path: string): DecorationSet {
  const state = view.state;
  const doc = state.doc;
  const deco: ReturnType<Decoration["range"]>[] = [];
  const lineClasses = new Map<number, Set<string>>();

  // Selection, extended to whole lines: the reveal granularity.
  const sel = state.selection.ranges.map((r) => ({
    from: doc.lineAt(r.from).from,
    to: doc.lineAt(r.to).to,
  }));
  const active = (from: number, to: number): boolean =>
    sel.some((s) => s.from <= to && s.to >= from);
  const lineActive = (pos: number): boolean => {
    const l = doc.lineAt(pos);
    return active(l.from, l.to);
  };

  const addLineClass = (lineFrom: number, cls: string): void => {
    let set = lineClasses.get(lineFrom);
    if (set === undefined) {
      set = new Set();
      lineClasses.set(lineFrom, set);
    }
    set.add(cls);
  };
  const eachLine = (from: number, to: number, f: (line: Line) => void): void => {
    for (let pos = from; pos <= to; ) {
      const line = doc.lineAt(pos);
      f(line);
      if (line.to >= to) break;
      pos = line.to + 1;
    }
  };
  /** Hide [from,to], swallowing one adjacent space on the given side. */
  const hide = (from: number, to: number, spaceAfter = false, spaceBefore = false): void => {
    let f = from;
    let t = to;
    if (spaceAfter && doc.sliceString(t, t + 1) === " ") t += 1;
    if (spaceBefore && doc.sliceString(f - 1, f) === " ") f -= 1;
    if (t > f) deco.push(hidden.range(f, t));
  };

  const fmEnd = frontmatterEnd(state);
  if (fmEnd > 0) eachLine(0, fmEnd, (l) => addLineClass(l.from, "lp-frontmatter"));

  const enter = (node: SyntaxNodeRef): boolean | void => {
    const name = node.name;
    if (name === "Document") return;
    if (node.from < fmEnd) return false;

    if (name.startsWith("ATXHeading")) {
      const from = doc.lineAt(node.from).from;
      addLineClass(from, "lp-heading");
      addLineClass(from, `lp-h${name.slice(10)}`);
      return;
    }
    if (name === "SetextHeading1" || name === "SetextHeading2") {
      const mark = node.node.getChild("HeaderMark");
      const underFrom = mark === null ? -1 : doc.lineAt(mark.from).from;
      eachLine(node.from, node.to, (l) => {
        if (l.from === underFrom) return; // the ===/--- line stays small chrome
        addLineClass(l.from, "lp-heading");
        addLineClass(l.from, name === "SetextHeading1" ? "lp-h1" : "lp-h2");
      });
      return;
    }
    if (name === "HeaderMark") {
      const parent = node.node.parent;
      if (parent !== null && parent.name.startsWith("ATXHeading")) {
        if (!lineActive(node.from)) {
          const line = doc.lineAt(node.from);
          const opener = /^\s*$/.test(doc.sliceString(line.from, node.from));
          hide(node.from, node.to, opener, !opener);
        }
      } else {
        deco.push(Decoration.mark({ class: "lp-mark" }).range(node.from, node.to));
      }
      return;
    }
    if (name === "EmphasisMark" || name === "StrikethroughMark") {
      if (!lineActive(node.from)) hide(node.from, node.to);
      return;
    }
    if (name === "Strikethrough") {
      deco.push(Decoration.mark({ class: "lp-strike" }).range(node.from, node.to));
      return;
    }
    if (name === "InlineCode") {
      deco.push(Decoration.mark({ class: "lp-inline-code" }).range(node.from, node.to));
      return;
    }
    if (name === "CodeMark") {
      const parent = node.node.parent;
      if (parent !== null && parent.name === "InlineCode") {
        if (!lineActive(node.from)) hide(node.from, node.to);
      } else {
        deco.push(Decoration.mark({ class: "lp-fence-chrome" }).range(node.from, node.to));
      }
      return;
    }
    if (name === "CodeInfo") {
      deco.push(Decoration.mark({ class: "lp-fence-chrome" }).range(node.from, node.to));
      return;
    }
    if (name === "FencedCode" || name === "CodeBlock") {
      const firstLine = doc.lineAt(node.from);
      const lastFrom = doc.lineAt(node.to).from;
      eachLine(node.from, node.to, (l) => {
        addLineClass(l.from, "lp-codeblock");
        if (l.from === firstLine.from) addLineClass(l.from, "lp-codeblock-first");
        if (l.from === lastFrom) addLineClass(l.from, "lp-codeblock-last");
      });
      if (name === "FencedCode")
        deco.push(Decoration.widget({ widget: fenceCopyWidget, side: 1 }).range(firstLine.to));
      return;
    }
    if (name === "Blockquote") {
      let outermost = true;
      for (let p = node.node.parent; p !== null; p = p.parent)
        if (p.name === "Blockquote") {
          outermost = false;
          break;
        }
      const lastFrom = doc.lineAt(node.to).from;
      eachLine(node.from, node.to, (l) => {
        addLineClass(l.from, "lp-quote");
        if (outermost && l.from === doc.lineAt(node.from).from) addLineClass(l.from, "lp-quote-first");
        if (outermost && l.from === lastFrom) addLineClass(l.from, "lp-quote-last");
      });
      return;
    }
    if (name === "QuoteMark") {
      if (!lineActive(node.from)) hide(node.from, node.to, true);
      return;
    }
    if (name === "ListMark") {
      if (lineActive(node.from)) return;
      const item = node.node.parent;
      if (item === null) return;
      if (item.getChild("Task") !== null) {
        hide(node.from, node.to, true); // the checkbox stands in for `- [ ]`
      } else if (item.parent?.name === "BulletList" && node.to - node.from === 1) {
        deco.push(Decoration.replace({ widget: bulletWidget }).range(node.from, node.to));
      }
      return;
    }
    if (name === "Task") {
      const marker = node.node.getChild("TaskMarker");
      if (marker !== null && /x/i.test(doc.sliceString(marker.from, marker.to))) {
        const from = Math.min(marker.to + 1, node.to);
        if (node.to > from)
          deco.push(Decoration.mark({ class: "lp-task-done" }).range(from, node.to));
      }
      return;
    }
    if (name === "TaskMarker") {
      if (!lineActive(node.from)) {
        const checked = /x/i.test(doc.sliceString(node.from, node.to));
        deco.push(
          Decoration.replace({ widget: checked ? checkedWidget : uncheckedWidget }).range(
            node.from,
            node.to,
          ),
        );
      }
      return;
    }
    if (name === "HorizontalRule") {
      if (!lineActive(node.from))
        deco.push(Decoration.replace({ widget: ruleWidget }).range(node.from, node.to));
      return;
    }
    if (name === "Image") {
      const lineFrom = doc.lineAt(node.from).from;
      const lineTo = doc.lineAt(node.to).to;
      if (active(lineFrom, lineTo)) return false; // show source while editing it
      const urlNode = node.node.getChild("URL");
      if (urlNode === null) return false; // reference-style: leave as source
      const url = doc.sliceString(urlNode.from, urlNode.to);
      if (url.length === 0) return false;
      const marks = node.node.getChildren("LinkMark");
      const alt =
        marks.length >= 2 ? doc.sliceString(marks[0].to, marks[1].from) : "";
      const remote = /^[a-z][a-z0-9+.-]*:/i.test(url);
      const target = remote ? url : resolveDocPath(path, safeDecode(url));
      deco.push(
        Decoration.replace({ widget: new ImageWidget(target, alt, remote) }).range(
          node.from,
          node.to,
        ),
      );
      return false;
    }
    if (name === "Link") {
      const marks = node.node.getChildren("LinkMark");
      if (marks.length >= 2 && marks[1].from > marks[0].to) {
        const urlNode = node.node.getChild("URL");
        const url = urlNode === null ? "" : doc.sliceString(urlNode.from, urlNode.to);
        deco.push(
          Decoration.mark({
            class: "lp-link",
            attributes: url.length > 0 ? { title: url } : undefined,
          }).range(marks[0].to, marks[1].from),
        );
      }
      return;
    }
    if (name === "LinkMark") {
      const p = node.node.parent?.name;
      if ((p === "Link" || p === "Autolink") && !lineActive(node.from))
        hide(node.from, node.to);
      return;
    }
    if (name === "URL" || name === "LinkTitle") {
      const p = node.node.parent?.name;
      if (p === "Link" && !lineActive(node.from)) hide(node.from, node.to);
      return;
    }
    if (name === "Table") {
      eachLine(node.from, node.to, (l) => addLineClass(l.from, "lp-table"));
      return false; // pipes align in mono; the full grid lives in reading mode
    }
    if (name === "HTMLBlock" || name === "CommentBlock") {
      eachLine(node.from, node.to, (l) => addLineClass(l.from, "lp-html"));
      return false; // raw HTML stays visible source — never rendered here
    }
  };

  for (const range of view.visibleRanges)
    syntaxTree(state).iterate({ from: range.from, to: range.to, enter });

  for (const [pos, cls] of lineClasses)
    deco.push(Decoration.line({ class: [...cls].join(" ") }).range(pos));

  return Decoration.set(deco, true);
}

function livePlugin(path: string): Extension {
  return ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(view: EditorView) {
        this.decorations = buildDecorations(view, path);
      }
      update(u: ViewUpdate): void {
        // The tree comparison catches the incremental parser finishing regions
        // after the viewport painted (large documents parse in the background).
        if (
          u.docChanged ||
          u.selectionSet ||
          u.viewportChanged ||
          syntaxTree(u.state) !== syntaxTree(u.startState)
        )
          this.decorations = buildDecorations(u.view, path);
      }
    },
    { decorations: (v) => v.decorations },
  );
}

// --- links -------------------------------------------------------------------

function linkUrlAt(state: EditorState, pos: number): string | null {
  for (
    let n: SyntaxNode | null = syntaxTree(state).resolveInner(pos, 1);
    n !== null;
    n = n.parent
  ) {
    if (n.name === "URL") return state.doc.sliceString(n.from, n.to);
    if (n.name === "Link" || n.name === "Autolink") {
      const u = n.getChild("URL");
      return u === null ? null : state.doc.sliceString(u.from, u.to);
    }
  }
  return null;
}

/** Mod+click follows a link (plain click has to place the cursor — this is an
 *  editor). Routed like the reading view: a live local app opens in a browser
 *  pane, anything else in the user's real browser; relative/in-repo hrefs are
 *  swallowed rather than navigating the workbench to a 404. */
const linkClicks = EditorView.domEventHandlers({
  click: (e, view) => {
    if (!e.metaKey && !e.ctrlKey) return false;
    const pos = view.posAtCoords({ x: e.clientX, y: e.clientY });
    if (pos === null) return false;
    let url = linkUrlAt(view.state, pos);
    if (url === null) return false;
    if (/^www\./i.test(url)) url = `http://${url}`; // GFM bare autolink
    if (!isWebUrl(url)) return false;
    e.preventDefault();
    activateUrl(url, false);
    return true;
  },
});

// --- theme -------------------------------------------------------------------

/** Every selector is scoped on the extra `cm-md-live` root class so these
 *  rules out-rank the shared settings theme (same properties, e.g. .cm-line
 *  padding) by specificity rather than by fragile injection order. Sizes are
 *  in em off the content font so A−/A+ scales the document uniformly,
 *  mirroring the reading view's .md-body. */
const liveTheme = (fontSize: number, lineHeight: number): Extension =>
  EditorView.theme({
    "&.cm-md-live .cm-scroller": {
      fontFamily: "var(--ui-font)",
      lineHeight: `${lineHeight}`,
    },
    "&.cm-md-live .cm-content": {
      flex: "0 1 auto",
      width: "100%",
      maxWidth: "70ch",
      margin: "0 auto",
      boxSizing: "border-box",
      padding: "2.2rem 2rem 3.5rem",
      fontSize: `${fontSize}px`,
      overflowWrap: "break-word",
    },
    "&.cm-md-live .cm-line": { padding: "0" },
    "&.cm-md-live .cm-gutters": { display: "none" },

    "&.cm-md-live .lp-heading": { lineHeight: "1.25", letterSpacing: "-0.01em" },
    "&.cm-md-live .lp-h1": {
      fontSize: "1.576em",
      paddingTop: "0.45em",
      paddingBottom: "0.35em",
      borderBottom: "1px solid var(--edge)",
    },
    "&.cm-md-live .lp-h2": {
      fontSize: "1.25em",
      paddingTop: "0.55em",
      paddingBottom: "0.25em",
      borderBottom: "1px solid var(--edge)",
    },
    "&.cm-md-live .lp-h3": { fontSize: "1.087em", paddingTop: "0.5em" },
    "&.cm-md-live .lp-h4, &.cm-md-live .lp-h5, &.cm-md-live .lp-h6": {
      paddingTop: "0.4em",
    },
    "&.cm-md-live .lp-setext-under, &.cm-md-live .lp-mark": {
      color: "var(--muted)",
      opacity: "0.6",
    },

    "&.cm-md-live .lp-quote": {
      borderLeft: "3px solid color-mix(in srgb, var(--accent) 60%, transparent)",
      background:
        "linear-gradient(to right, color-mix(in srgb, var(--accent) 5%, transparent), color-mix(in srgb, var(--fg) 3%, transparent) 55%)",
      padding: "0 1em",
      color: "color-mix(in srgb, var(--fg) 45%, var(--muted))",
    },
    "&.cm-md-live .lp-quote-first": {
      borderTopRightRadius: "8px",
      paddingTop: "0.45em",
    },
    "&.cm-md-live .lp-quote-last": {
      borderBottomRightRadius: "8px",
      paddingBottom: "0.45em",
    },

    "&.cm-md-live .lp-codeblock": {
      background: "color-mix(in srgb, var(--fg) 4.5%, transparent)",
      borderLeft: "1px solid var(--edge)",
      borderRight: "1px solid var(--edge)",
      fontFamily: "var(--mono)",
      fontSize: "0.848em",
      lineHeight: "1.5",
      padding: "0 1em",
    },
    "&.cm-md-live .lp-codeblock-first": {
      borderTop: "1px solid var(--edge)",
      borderRadius: "8px 8px 0 0",
      paddingTop: "0.5em",
    },
    "&.cm-md-live .lp-codeblock-last": {
      borderBottom: "1px solid var(--edge)",
      borderRadius: "0 0 8px 8px",
      paddingBottom: "0.5em",
    },
    "&.cm-md-live .lp-codeblock-first.lp-codeblock-last": { borderRadius: "8px" },
    "&.cm-md-live .lp-fence-chrome": { color: "var(--muted)", opacity: "0.75" },

    "&.cm-md-live .lp-inline-code": {
      fontFamily: "var(--mono)",
      fontSize: "0.82em",
      background: "color-mix(in srgb, var(--fg) 6%, transparent)",
      borderRadius: "4px",
      padding: "0.12em 0.2em",
    },
    "&.cm-md-live .lp-link": { color: "var(--accent)" },
    "&.cm-md-live .lp-link:hover": { textDecoration: "underline" },
    "&.cm-md-live .lp-strike": { textDecoration: "line-through" },
    "&.cm-md-live .lp-task-done": {
      color: "var(--muted)",
      textDecoration: "line-through",
    },
    "&.cm-md-live .lp-table, &.cm-md-live .lp-html, &.cm-md-live .lp-frontmatter": {
      fontFamily: "var(--mono)",
      fontSize: "0.848em",
    },
    "&.cm-md-live .lp-frontmatter": { color: "var(--muted)" },

    "&.cm-md-live .lp-bullet": {
      color: "color-mix(in srgb, var(--accent) 70%, var(--muted))",
    },
    "&.cm-md-live .lp-hr": {
      display: "inline-block",
      width: "100%",
      borderTop: "1px solid var(--edge)",
      verticalAlign: "middle",
    },
    "&.cm-md-live .lp-image": {
      maxWidth: "100%",
      borderRadius: "4px",
      verticalAlign: "bottom",
    },
    "&.cm-md-live .lp-task": {
      accentColor: "var(--accent)",
      margin: "0 0.4em 0 0",
      verticalAlign: "middle",
    },
    "&.cm-md-live .lp-copy": {
      appearance: "none",
      border: "none",
      background: "none",
      display: "inline-flex",
      alignItems: "center",
      padding: "2px 4px",
      marginLeft: "0.75em",
      borderRadius: "4px",
      color: "var(--muted)",
      opacity: "0.5",
      cursor: "pointer",
      verticalAlign: "middle",
    },
    "&.cm-md-live .lp-copy:hover, &.cm-md-live .lp-copy.copied": {
      opacity: "1",
      color: "var(--accent)",
    },
    "&.cm-md-live .lp-copy .ic-check, &.cm-md-live .lp-copy.copied .ic-copy": {
      display: "none",
    },
    "&.cm-md-live .lp-copy.copied .ic-check": { display: "block" },
  });

/**
 * The full live-preview extension set for CodeView's `extra` slot. GFM base
 * (tables, task lists, strikethrough, autolinks) with nested fenced-code
 * highlighting via the shared language registry.
 */
export function markdownLive(opts: MarkdownLiveOptions): Extension {
  return [
    markdown({ base: markdownLanguage, codeLanguages: languages }),
    EditorView.lineWrapping,
    EditorView.editorAttributes.of({ class: "cm-md-live" }),
    liveTheme(opts.fontSize, opts.lineHeight),
    livePlugin(opts.path),
    linkClicks,
  ];
}
