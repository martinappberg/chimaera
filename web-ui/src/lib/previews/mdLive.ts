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
 * cursor always edits real text and nothing is ever atomic-trapped. A
 * construct the decorator can't render faithfully (a reference link, an image
 * whose syntax wraps lines) stays visible source rather than half-hidden.
 *
 * Replace decorations from a view plugin may not span line breaks (CodeMirror
 * throws and disables the plugin, degrading the whole document): `hide()` and
 * the image widget both enforce single-line ranges.
 *
 * Sanitization boundary: nothing from the document is ever injected as HTML.
 * Widgets are built via createElement/textContent; the only innerHTML is the
 * shared constant copy-icon SVG. Image widgets pass through only web
 * (http/https) and inline `data:image/` URLs — any other absolute scheme
 * stays visible source — and document-relative paths go through a ticketed
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
import { rawTicketUrl, resolveDocPath, safeDecodeUri } from "./files";
import { copyText } from "../shared/clipboard";
import { makeCopyButton } from "../shared/copyDecor";
import { activateUrl, hasUrlScheme, isWebUrl, urlMenuEntries } from "../shared/urlOpen";
import { contextMenu } from "../shared/contextMenu.svelte";

/**
 * The GFM markdown language (tables, task lists, strikethrough, autolinks;
 * nested fenced-code highlighting via the shared registry). A module
 * SINGLETON: the host keeps it active in both live and source modes, so a
 * mode flip reconfigures around the same Language instance and CodeMirror
 * never reparses the document.
 */
export const markdownLanguageExt: Extension = markdown({
  base: markdownLanguage,
  codeLanguages: languages,
});

// --- widgets -----------------------------------------------------------------

/** Base for stateless singleton widgets: any instance equals any other. */
abstract class StaticWidget extends WidgetType {
  override eq(): boolean {
    return true;
  }
}

/** `-`/`*`/`+` rendered as the reading view's tinted bullet. */
class BulletWidget extends StaticWidget {
  toDOM(): HTMLElement {
    const s = document.createElement("span");
    s.className = "lp-bullet";
    s.textContent = "•";
    return s;
  }
}

/** `---` rendered as a rule when its line is inactive. */
class RuleWidget extends StaticWidget {
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

/** Restore a copy button's idle state after the shared 1400ms flash. */
function flashCopied(btn: HTMLElement, idleLabel: string): void {
  btn.classList.add("copied");
  btn.setAttribute("aria-label", "copied");
  setTimeout(() => {
    if (!btn.isConnected) return;
    btn.classList.remove("copied");
    btn.setAttribute("aria-label", idleLabel);
  }, 1400);
}

/**
 * The copy affordance on fenced code blocks and quote cards (the reading
 * view's `.md-copy` language, sized down to sit inline on the block's first
 * line). The payload is resolved from the live syntax tree at click time.
 */
class BlockCopyWidget extends WidgetType {
  constructor(readonly kind: "fence" | "quote") {
    super();
  }
  override eq(other: BlockCopyWidget): boolean {
    return other.kind === this.kind;
  }
  private payloadAt(view: EditorView, pos: number): string {
    const doc = view.state.doc;
    let n: SyntaxNode | null = syntaxTree(view.state).resolveInner(pos, -1);
    if (this.kind === "fence") {
      while (n !== null && n.name !== "FencedCode") n = n.parent;
      if (n === null) return "";
      // One CodeText per LINE inside a blockquote (interleaved QuoteMarks) —
      // getChild would return only the first line of a quoted fence.
      return n
        .getChildren("CodeText")
        .map((c) => doc.sliceString(c.from, c.to))
        .join("")
        .replace(/\s+$/, "");
    }
    let quote: SyntaxNode | null = null;
    for (; n !== null; n = n.parent) if (n.name === "Blockquote") quote = n; // outermost wins
    if (quote === null) return "";
    // The rendered prose is the source minus its per-line quote markers.
    return doc
      .sliceString(quote.from, quote.to)
      .split("\n")
      .map((l) => l.replace(/^\s{0,3}(>\s?)+/, ""))
      .join("\n")
      .trim();
  }
  toDOM(view: EditorView): HTMLElement {
    const idleLabel = this.kind === "fence" ? "copy code" : "copy";
    const btn = makeCopyButton("lp-copy", idleLabel);
    btn.addEventListener("mousedown", (e) => e.preventDefault());
    btn.addEventListener("click", (e) => {
      e.preventDefault();
      const payload = this.payloadAt(view, view.posAtDOM(btn));
      if (payload.length === 0) return;
      void copyText(payload).then((ok) => {
        if (ok && btn.isConnected) flashCopied(btn, idleLabel);
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
const checkedWidget = new CheckboxWidget(true);
const uncheckedWidget = new CheckboxWidget(false);
const fenceCopyWidget = new BlockCopyWidget("fence");
const quoteCopyWidget = new BlockCopyWidget("quote");

// Invariant decoration specs, hoisted so a rebuild allocates none of them and
// RangeSet comparison stays on the fast reference-equality path.
const hidden = Decoration.replace({});
const markMuted = Decoration.mark({ class: "lp-mark" });
const markStrike = Decoration.mark({ class: "lp-strike" });
const markInlineCode = Decoration.mark({ class: "lp-inline-code" });
const markFenceChrome = Decoration.mark({ class: "lp-fence-chrome" });
const markTaskDone = Decoration.mark({ class: "lp-task-done" });
const replaceBullet = Decoration.replace({ widget: bulletWidget });
const replaceRule = Decoration.replace({ widget: ruleWidget });
const replaceChecked = Decoration.replace({ widget: checkedWidget });
const replaceUnchecked = Decoration.replace({ widget: uncheckedWidget });
const widgetFenceCopy = Decoration.widget({ widget: fenceCopyWidget, side: 1 });
const widgetQuoteCopy = Decoration.widget({ widget: quoteCopyWidget, side: 1 });

// --- decoration build --------------------------------------------------------

/**
 * End offset of a leading YAML frontmatter block (0 = none). The markdown
 * parser has no frontmatter notion — without this, `---` fences would render
 * as rules and `title:`-then-`---` as a setext heading. Strict shape: an
 * UNindented `---` first line, a closing `---`, and at least one `key:` line
 * between — a document that merely opens with a thematic break must not have
 * its head restyled as metadata. Bounded scan; recomputed only on doc change.
 */
function frontmatterEnd(state: EditorState): number {
  const doc = state.doc;
  if (doc.lines < 2 || doc.line(1).text !== "---") return 0;
  const cap = Math.min(doc.lines, 200);
  let sawKey = false;
  for (let i = 2; i <= cap; i++) {
    const t = doc.line(i).text;
    if (t.trimEnd() === "---") return sawKey ? doc.line(i).to : 0;
    if (/^[A-Za-z0-9_-]+\s*:/.test(t)) sawKey = true;
  }
  return 0;
}

interface Span {
  from: number;
  to: number;
}

/** The selection extended to whole lines — the reveal granularity. */
function selectionSpans(state: EditorState): Span[] {
  return state.selection.ranges.map((r) => ({
    from: state.doc.lineAt(r.from).from,
    to: state.doc.lineAt(r.to).to,
  }));
}

function spansEqual(a: Span[], b: Span[]): boolean {
  return a.length === b.length && a.every((s, i) => s.from === b[i].from && s.to === b[i].to);
}

function buildDecorations(
  view: EditorView,
  path: string,
  fmEnd: number,
  sel: Span[],
): DecorationSet {
  const state = view.state;
  const doc = state.doc;
  const deco: ReturnType<Decoration["range"]>[] = [];
  const lineClasses = new Map<number, Set<string>>();
  // Point/replace pushes that could repeat when a node overlaps two
  // visibleRanges entries (a ≥20k-char line splits the viewport with a line
  // gap) — without this a fence would render two stacked copy buttons.
  const pushed = new Set<string>();
  const once = (key: string): boolean => {
    if (pushed.has(key)) return false;
    pushed.add(key);
    return true;
  };

  // Spans are whole lines, so "the position's line intersects a span" reduces
  // to plain containment — no per-node doc.lineAt lookup needed.
  const active = (from: number, to: number): boolean =>
    sel.some((s) => s.from <= to && s.to >= from);
  const lineActive = (pos: number): boolean => sel.some((s) => s.from <= pos && s.to >= pos);

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
  // The current visibleRange being iterated: per-line walks clamp to it so a
  // huge node overlapping the viewport (a 10k-line pasted-log fence) costs
  // only its visible lines per rebuild, never the whole node.
  let rangeFrom = 0;
  let rangeTo = 0;
  const eachVisibleLine = (from: number, to: number, f: (line: Line) => void): void => {
    const f0 = Math.max(from, rangeFrom);
    const t0 = Math.min(to, rangeTo);
    if (f0 <= t0) eachLine(f0, t0, f);
  };
  /** Hide [from,to], swallowing one adjacent space on the given side. A range
   *  that would cross a line break is left visible (see the module header). */
  const hide = (from: number, to: number, spaceAfter = false, spaceBefore = false): void => {
    let f = from;
    let t = to;
    if (spaceAfter && doc.sliceString(t, t + 1) === " ") t += 1;
    if (spaceBefore && doc.sliceString(f - 1, f) === " ") f -= 1;
    if (t > f && doc.lineAt(f).to >= t) deco.push(hidden.range(f, t));
  };
  /** A quoted table/HTML block nests its per-line QuoteMarks INSIDE the
   *  skipped node; leaving them visible while the block's first line hides
   *  its own would shift that one line left of the rest. */
  const hideNestedQuoteMarks = (n: SyntaxNodeRef): void => {
    syntaxTree(state).iterate({
      from: Math.max(n.from, rangeFrom),
      to: Math.min(n.to, rangeTo),
      enter: (c) => {
        if (c.name === "QuoteMark" && !lineActive(c.from)) hide(c.from, c.to, true);
      },
    });
  };

  if (fmEnd > 0) eachLine(0, fmEnd, (l) => addLineClass(l.from, "lp-frontmatter"));

  const enter = (node: SyntaxNodeRef): boolean | void => {
    const name = node.name;
    if (name === "Document") return;
    if (node.from < fmEnd) return false;

    if (name.startsWith("ATXHeading")) {
      const line = doc.lineAt(node.from);
      addLineClass(line.from, "lp-heading");
      addLineClass(line.from, `lp-h${name.slice(10)}`);
      if (!active(line.from, line.to)) {
        const marks = node.node.getChildren("HeaderMark");
        if (marks.length > 0) {
          // Opener and (optional) closer are hidden HERE, together, so the
          // closer's backward space-swallow can never reach into the opener's
          // forward one — an empty heading (`# #`) would otherwise emit
          // partially overlapping replaces, which CodeMirror forbids.
          let openEnd = marks[0].to;
          if (doc.sliceString(openEnd, openEnd + 1) === " ") openEnd += 1;
          deco.push(hidden.range(marks[0].from, openEnd));
          if (marks.length > 1) {
            const closer = marks[marks.length - 1];
            let cf = closer.from;
            if (cf - 1 >= openEnd && doc.sliceString(cf - 1, cf) === " ") cf -= 1;
            if (closer.to > cf && cf >= openEnd) deco.push(hidden.range(cf, closer.to));
          }
        }
      }
      return;
    }
    if (name === "SetextHeading1" || name === "SetextHeading2") {
      const mark = node.node.getChild("HeaderMark");
      const underFrom = mark === null ? -1 : doc.lineAt(mark.from).from;
      eachVisibleLine(node.from, node.to, (l) => {
        if (l.from === underFrom) return; // the ===/--- line stays small chrome
        addLineClass(l.from, "lp-heading");
        addLineClass(l.from, name === "SetextHeading1" ? "lp-h1" : "lp-h2");
      });
      return;
    }
    if (name === "HeaderMark") {
      // ATX marks are handled by their heading; only setext underlines remain.
      if (node.node.parent?.name.startsWith("ATXHeading") !== true)
        deco.push(markMuted.range(node.from, node.to));
      return;
    }
    if (name === "EmphasisMark" || name === "StrikethroughMark") {
      if (!lineActive(node.from)) hide(node.from, node.to);
      return;
    }
    if (name === "Strikethrough") {
      deco.push(markStrike.range(node.from, node.to));
      return;
    }
    if (name === "InlineCode") {
      deco.push(markInlineCode.range(node.from, node.to));
      return;
    }
    if (name === "CodeMark") {
      const parent = node.node.parent;
      if (parent !== null && parent.name === "InlineCode") {
        if (!lineActive(node.from)) hide(node.from, node.to);
      } else {
        deco.push(markFenceChrome.range(node.from, node.to));
      }
      return;
    }
    if (name === "CodeInfo") {
      deco.push(markFenceChrome.range(node.from, node.to));
      return;
    }
    if (name === "FencedCode" || name === "CodeBlock") {
      const firstLine = doc.lineAt(node.from);
      const lastFrom = doc.lineAt(node.to).from;
      eachVisibleLine(node.from, node.to, (l) => {
        addLineClass(l.from, "lp-codeblock");
        if (l.from === firstLine.from) addLineClass(l.from, "lp-codeblock-first");
        if (l.from === lastFrom) addLineClass(l.from, "lp-codeblock-last");
      });
      if (name === "FencedCode" && once(`copy:${firstLine.to}`))
        deco.push(widgetFenceCopy.range(firstLine.to));
      return;
    }
    if (name === "Blockquote") {
      let outermost = true;
      for (let p = node.node.parent; p !== null; p = p.parent)
        if (p.name === "Blockquote") {
          outermost = false;
          break;
        }
      const firstLine = doc.lineAt(node.from);
      const lastFrom = doc.lineAt(node.to).from;
      eachVisibleLine(node.from, node.to, (l) => {
        addLineClass(l.from, "lp-quote");
        if (outermost && l.from === firstLine.from) addLineClass(l.from, "lp-quote-first");
        if (outermost && l.from === lastFrom) addLineClass(l.from, "lp-quote-last");
      });
      // The quote card carries the same copy affordance as the reading view.
      if (outermost && once(`copy:${firstLine.to}`))
        deco.push(widgetQuoteCopy.range(firstLine.to));
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
      const bullet = item.parent?.name === "BulletList";
      if (item.getChild("Task") !== null) {
        // The checkbox stands in for a BULLET task's `- `; an ordered task
        // keeps its number — hiding it would erase the item's ordering.
        if (bullet) hide(node.from, node.to, true);
      } else if (bullet && node.to - node.from === 1) {
        deco.push(replaceBullet.range(node.from, node.to));
      }
      return;
    }
    if (name === "Task") {
      const marker = node.node.getChild("TaskMarker");
      if (marker !== null && /x/i.test(doc.sliceString(marker.from, marker.to))) {
        const from = Math.min(marker.to + 1, node.to);
        if (node.to > from) deco.push(markTaskDone.range(from, node.to));
      }
      return;
    }
    if (name === "TaskMarker") {
      if (!lineActive(node.from)) {
        const checked = /x/i.test(doc.sliceString(node.from, node.to));
        deco.push((checked ? replaceChecked : replaceUnchecked).range(node.from, node.to));
      }
      return;
    }
    if (name === "HorizontalRule") {
      if (!lineActive(node.from) && once(`hr:${node.from}`))
        deco.push(replaceRule.range(node.from, node.to));
      return;
    }
    if (name === "Image") {
      const line = doc.lineAt(node.from);
      if (line.to < node.to) return false; // spans lines: replace is illegal from a plugin
      if (active(line.from, line.to)) return false; // show source while editing it
      const urlNode = node.node.getChild("URL");
      if (urlNode === null) return false; // reference-style: leave as source
      const url = doc.sliceString(urlNode.from, urlNode.to);
      if (url.length === 0) return false;
      const remote = hasUrlScheme(url);
      // Only web and inline-data images render; any other absolute scheme
      // (file:, chrome:, …) stays visible source — mirroring what the reading
      // view's server-side sanitizer lets through.
      if (remote && !/^(https?:|data:image\/)/i.test(url)) return false;
      const marks = node.node.getChildren("LinkMark");
      const alt = marks.length >= 2 ? doc.sliceString(marks[0].to, marks[1].from) : "";
      const target = remote ? url : resolveDocPath(path, safeDecodeUri(url));
      if (once(`img:${node.from}`))
        deco.push(
          Decoration.replace({ widget: new ImageWidget(target, alt, remote) }).range(
            node.from,
            node.to,
          ),
        );
      return false;
    }
    if (name === "Link") {
      const n = node.node;
      const urlNode = n.getChild("URL");
      const marks = n.getChildren("LinkMark");
      // Reference-style ([a][ref]) and label-less links have no URL or no
      // text to stand in for the syntax — leave them fully visible source
      // (hiding their marks rendered `a[ref]` mangles and `[]()` invisible).
      if (urlNode === null || marks.length < 2 || marks[1].from <= marks[0].to) return false;
      const url = doc.sliceString(urlNode.from, urlNode.to);
      deco.push(
        Decoration.mark({ class: "lp-link", attributes: { title: url } }).range(
          marks[0].to,
          marks[1].from,
        ),
      );
      if (!lineActive(node.from)) {
        hide(marks[0].from, marks[0].to);
        // ONE range from "]" to ")": the URL/title child nodes don't cover
        // their separator spaces, so piecemeal hides leaked stray gaps.
        hide(marks[1].from, node.to);
      }
      return; // children still decorate (nested emphasis in the label)
    }
    if (name === "LinkMark") {
      // Link/Image marks are handled by their parents; only autolink angle
      // brackets remain.
      if (node.node.parent?.name === "Autolink" && !lineActive(node.from))
        hide(node.from, node.to);
      return;
    }
    if (name === "Table") {
      eachVisibleLine(node.from, node.to, (l) => addLineClass(l.from, "lp-table"));
      hideNestedQuoteMarks(node);
      return false; // pipes align in mono; the full grid lives in reading mode
    }
    if (name === "HTMLBlock" || name === "CommentBlock") {
      eachVisibleLine(node.from, node.to, (l) => addLineClass(l.from, "lp-html"));
      hideNestedQuoteMarks(node);
      return false; // raw HTML stays visible source — never rendered here
    }
  };

  for (const range of view.visibleRanges) {
    rangeFrom = range.from;
    rangeTo = range.to;
    syntaxTree(state).iterate({ from: range.from, to: range.to, enter });
  }

  for (const [pos, cls] of lineClasses)
    deco.push(Decoration.line({ class: [...cls].join(" ") }).range(pos));

  return Decoration.set(deco, true);
}

function livePlugin(path: string): Extension {
  return ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      private fmEnd: number;
      private spans: Span[];
      constructor(view: EditorView) {
        this.fmEnd = frontmatterEnd(view.state);
        this.spans = selectionSpans(view.state);
        this.decorations = buildDecorations(view, path, this.fmEnd, this.spans);
      }
      update(u: ViewUpdate): void {
        if (u.docChanged) this.fmEnd = frontmatterEnd(u.state);
        // The tree comparison catches the incremental parser finishing regions
        // after the viewport painted (large documents parse in the background).
        const treeChanged = syntaxTree(u.state) !== syntaxTree(u.startState);
        if (!u.docChanged && !u.viewportChanged && !treeChanged) {
          if (!u.selectionSet) return;
          // Reveal granularity is whole lines: a cursor move WITHIN the
          // already-selected lines cannot change the output — skip the walk.
          const spans = selectionSpans(u.state);
          if (spansEqual(spans, this.spans)) return;
          this.spans = spans;
        } else {
          this.spans = selectionSpans(u.state);
        }
        this.decorations = buildDecorations(u.view, path, this.fmEnd, this.spans);
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

function linkUrlAtCoords(view: EditorView, x: number, y: number): string | null {
  const pos = view.posAtCoords({ x, y });
  if (pos === null) return null;
  let url = linkUrlAt(view.state, pos);
  if (url === null) return null;
  if (/^www\./i.test(url)) url = `http://${url}`; // GFM bare autolink
  return isWebUrl(url) ? url : null;
}

/** Mod+press follows a link (plain click has to place the cursor — this is an
 *  editor). Routed like the reading view: a live local app opens in a browser
 *  pane, anything else in the user's real browser; relative/in-repo hrefs are
 *  swallowed rather than navigating the workbench to a 404. */
const linkClicks = EditorView.domEventHandlers({
  // MOUSEDOWN, not click: CodeMirror places the cursor on mousedown, which
  // reveals the line's hidden marks and shifts the layout — a click-time
  // posAtCoords would resolve against the SHIFTED text (wrong or dead link).
  // Consuming the mousedown also keeps the mod-press from moving the cursor.
  mousedown: (e, view) => {
    if (e.button !== 0 || (!e.metaKey && !e.ctrlKey)) return false;
    const url = linkUrlAtCoords(view, e.clientX, e.clientY);
    if (url === null) return false;
    e.preventDefault();
    activateUrl(url, false);
    return true;
  },
  // Right-click parity with the reading view's URL menu — the editor renders
  // no <a> elements for the delegated handler to find. openAt suppresses the
  // native menu itself.
  contextmenu: (e, view) => {
    const url = linkUrlAtCoords(view, e.clientX, e.clientY);
    if (url === null) return false;
    contextMenu.openAt(e, urlMenuEntries(url));
    return true;
  },
});

// --- theme -------------------------------------------------------------------

/** Every selector is scoped on the extra `cm-md-live` root class so these
 *  rules out-rank the shared settings theme (same properties, e.g. .cm-line
 *  padding) by specificity rather than by fragile injection order. The prose
 *  size rides the host-set `--lp-font-size`/`--lp-line-height` variables — a
 *  STATIC theme, because CodeMirror mounts every new theme's StyleModule
 *  permanently (a per-resize theme would leak one stylesheet per A−/A+ step).
 *  Sizes below are in em off the content font so A−/A+ scales the whole
 *  document uniformly, mirroring the reading view's .md-body. */
const liveTheme: Extension = EditorView.theme({
  "&.cm-md-live .cm-scroller": {
    fontFamily: "var(--ui-font)",
    lineHeight: "var(--lp-line-height, 1.6)",
  },
  "&.cm-md-live .cm-content": {
    flex: "0 1 auto",
    width: "100%",
    maxWidth: "70ch",
    margin: "0 auto",
    boxSizing: "border-box",
    padding: "2.2rem 2rem 3.5rem",
    fontSize: "var(--lp-font-size, var(--text-lg))",
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
  "&.cm-md-live .lp-mark": {
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
 * The live-preview behavior set (decorations, link handling, wrapping, the
 * static prose theme) for CodeView's `extra` slot. Keyed only on the document
 * path so the host can memoize it; the prose size arrives via the host-set
 * `--lp-font-size`/`--lp-line-height` CSS variables, so an A−/A+ resize never
 * reconfigures the editor. Pair with the module's `markdownLanguageExt`.
 */
export function markdownLive(path: string): Extension {
  return [
    EditorView.lineWrapping,
    EditorView.editorAttributes.of({ class: "cm-md-live" }),
    liveTheme,
    livePlugin(path),
    linkClicks,
  ];
}
