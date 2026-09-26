/**
 * Live mode as the reading view with one block revealed (Typora's model):
 * every top-level block the selection does not touch is replaced by the
 * reading renderer's own DOM for it (`doc/render.ts`, one block widget per
 * block from a state field), and only the blocks you are editing show as
 * source — styled by `mdLive.ts`'s inline decorations. The two views draw
 * a block with the same code and the same CSS (`.md-doc`, MarkdownView), so
 * live looks exactly like reading, except where you type.
 *
 * What makes that hold together:
 *
 * - **Segments** (`doc/live.ts`): the document on whole lines, a block plus
 *   the blank lines after it, so revealing a block only ever adds lines
 *   below the text you clicked. Nothing reveals while the editor is
 *   unfocused, so a document opens looking exactly like reading.
 * - **Spacing**: CodeMirror measures a widget's border box, never margins,
 *   and adjacent widgets do not collapse margins with each other. Each
 *   widget is a margin-free `display: flow-root` box that starts with a
 *   zero-height "ghost" of the previous block's trailing edge (its tag and
 *   classes, `doc/live.ts edgeChain`): inside the box the ghost's bottom
 *   margin and the block's own top margin collapse exactly as the two
 *   blocks' margins do in the reading article, from the same CSS. A
 *   revealed block gets the same gap as a small widget above its lines, so
 *   its first line sits where its rendered text sat.
 * - **Heights**: a widget's estimated height comes from a cache of measured
 *   heights keyed by what it renders and the layout it rendered at, so a
 *   block re-rendered when the cursor leaves it (or scrolled back into
 *   view) is exact before it paints and nothing below it moves.
 * - **Anchoring**: when a transaction changes which blocks are revealed,
 *   the line being acted on (the clicked text, the block an arrow key
 *   entered, the line being typed on) keeps its place on screen and the
 *   blocks around it absorb the change (CodeMirror's own scroll anchor,
 *   pointed at that line).
 * - **Entering a block**: a press on rendered text lands the cursor on the
 *   character pressed (its block's `data-sourcepos` lines, then the
 *   rendered prefix aligned against the source minus its syntax); arrow
 *   keys step into the adjacent block; a selection across blocks reveals
 *   each. Mod+press follows a link; task boxes toggle the source.
 */
import {
  BlockType,
  Decoration,
  EditorView,
  ViewPlugin,
  WidgetType,
  keymap,
  type BlockInfo,
  type DecorationSet,
  type ViewUpdate,
} from "@codemirror/view";
import { syntaxTree } from "@codemirror/language";
import {
  Annotation,
  EditorSelection,
  EditorState,
  Facet,
  Prec,
  StateEffect,
  StateField,
  Transaction,
  type Extension,
  type Text as CmText,
} from "@codemirror/state";
import type { SyntaxNode } from "@lezer/common";
import { contextOf, depsOf, footnotesOf, frontmatterOf, TextLines, type DocContext } from "./doc/model";
import { renderFootnotes, renderRun } from "./doc/render";
import { Hydrator, markTasks } from "./doc/reader";
import {
  HeightCache,
  hashString,
  nodesIn,
  revealedOf,
  runEdges,
  segmentAt,
  segmentsOf,
  shapeKey,
  sourceOffset,
  type Segment,
  type Shape,
} from "./doc/live";
import { parseFrontmatter, parseSourcepos, stripFences } from "./mdDoc";
import { completeRow, linkDestination, tableModel, type RowModel, type TableModel } from "./mdTable";
import { revealFlashRanges } from "./cm";
import { copyPayload, decorateCopyTargets } from "../shared/copyDecor";
import { copyText } from "../shared/clipboard";
import { requestReveal } from "../shared/reveal";
import { isWebUrl, urlMenuEntries, webUrl } from "../shared/urlOpen";
import { contextMenu } from "../shared/contextMenu.svelte";
import {
  followDocHref,
  isFollowable,
  revealAnchorInSource,
  showLinkHint,
  type DocLinkHost,
  type LinkContext,
} from "./docLinks";

// --- what the host provides ----------------------------------------------------------

/** The document's path (relative images and links resolve beside it). */
export const docPath = Facet.define<string, string>({ combine: (v) => v[0] ?? "" });

export const noLinkContext = (): LinkContext => ({ wsRoot: null, workspaceId: null });
/** The host's link context, read at click time (a getter, so the extension
 *  set stays stable while the workspace it resolves against can move). */
export const linkContext = Facet.define<() => LinkContext, () => LinkContext>({
  combine: (v) => v[0] ?? noLinkContext,
});

/** What the live view reads from its host at draw time. */
export interface LiveHost {
  /** The theme diagrams lay out in. */
  theme(): "light" | "dark";
}
const defaultHost: LiveHost = { theme: () => "light" };
export const liveHost = Facet.define<LiveHost, LiveHost>({ combine: (v) => v[0] ?? defaultHost });

/**
 * End offset of a leading YAML frontmatter block (0 = none), by the reading
 * view's rule (`doc/model.ts frontmatterOf`): an unindented `---` first
 * line, a closing line exactly `---` within 200 lines, and a `key:` line
 * between. Reads only those first 200 lines.
 */
export function frontmatterEnd(state: EditorState): number {
  const doc = state.doc;
  if (doc.lines < 2 || doc.line(1).text !== "---") return 0;
  const head = doc.sliceString(0, doc.line(Math.min(doc.lines, 201)).to);
  return frontmatterOf(head)?.end ?? 0;
}

// --- shared press helpers -------------------------------------------------------------

/** Whether a press sits on a scroller's own bar: it targets the scroller
 *  element itself AND lies in the bar's band along the bottom edge. A
 *  classic bar has a measurable band; an overlay bar (the macOS/WKWebView
 *  default) reserves none, so a 12px strip stands in — which is why the
 *  target matters too: content covering the scroller never loses its
 *  bottom to the strip. A fitting box has no bar to press. */
export function onScrollbarBand(el: Element | null, e: MouseEvent): boolean {
  if (!(el instanceof HTMLElement) || e.target !== el || el.scrollWidth <= el.clientWidth) return false;
  const band = Math.max(el.offsetHeight - el.clientHeight, 12);
  return e.clientY >= el.getBoundingClientRect().bottom - band;
}

/** The table whose range holds `pos`, modelled from the CURRENT tree. */
export function tableAt(state: EditorState, pos: number): TableModel | null {
  for (let n: SyntaxNode | null = syntaxTree(state).resolveInner(pos, 1); n !== null; n = n.parent) {
    if (n.name === "Table") return tableModel(n, state.doc);
  }
  return null;
}

/** Follow a link out of the live document exactly as the reading view does
 *  (docLinks.ts): a web URL opens as there, a file link opens in the
 *  workbench — Shift beside, since Mod is the follow gesture here — and a
 *  same-document anchor or `#L` fragment reveals its line in this editor. */
export function followLiveLink(
  view: EditorView,
  href: string,
  e: { clientX: number; clientY: number; shiftKey: boolean },
  byName = false,
): void {
  const doc = view.state.facet(docPath);
  const ctx = view.state.facet(linkContext)();
  const box = view.dom.closest<HTMLElement>(".md-content") ?? view.dom;
  const host: DocLinkHost = {
    docPath: doc,
    wsRoot: ctx.wsRoot,
    workspaceId: ctx.workspaceId,
    toAnchor: (anchor) => revealAnchorInSource(doc, anchor, view.state.doc.toString()),
    toLines: (r) => requestReveal(doc, r),
    hint: (text) => showLinkHint(box, e.clientX, e.clientY, text),
  };
  // A web URL keeps live mode's routing (pane or browser, never forced
  // into a split by the Shift a file link reads).
  void followDocHref(href, webUrl(href) === null && e.shiftKey, host, { byName });
}

// --- the state ----------------------------------------------------------------------

/** The width and type size blocks render at: the height cache's key. */
interface LiveLayout {
  key: string;
  line: number;
  char: number;
  width: number;
}
const DEFAULT_LAYOUT: LiveLayout = { key: "", line: 27, char: 8.6, width: 620 };

const focusEffect = StateEffect.define<boolean>();
const layoutEffect = StateEffect.define<LiveLayout>();
const propsEffect = StateEffect.define<boolean>();

interface Edges {
  head: Shape[];
  tail: Shape[];
}

interface LiveState {
  segs: readonly Segment[];
  /** Per segment: what it renders (its identity), "" when it renders
   *  nothing. */
  keys: readonly string[];
  edges: readonly Edges[];
  /** Per segment: shown as source. The same array while nothing changes. */
  revealed: readonly boolean[];
  fmEnd: number;
  /** The footnote section's identity, "" when there is none. */
  notes: string;
  /** Document-wide facts for rendering (heading ids, footnotes, refs). */
  cx: DocContext;
  focused: boolean;
  /** The properties panel is folded. */
  collapsed: boolean;
  layout: LiveLayout;
  flash: DecorationSet;
  deco: DecorationSet;
  /** Widget → the segment it draws (-1: the footnote section). */
  owner: Map<WidgetType, number>;
  /** Widgets by identity, kept across states so an unchanged block's
   *  widget is the same object (CodeMirror's comparison is then free). */
  pool: Map<string, LiveWidget>;
}

/** Heights measured in this session (every live view shares them). */
const heights = new HeightCache();
/** A block's edges by its identity (they read only its text). */
const edgeCache = new Map<string, Edges>();

function edgesFor(key: string, state: EditorState, seg: Segment): Edges {
  let e = edgeCache.get(key);
  if (e === undefined) {
    e = runEdges(nodesIn(syntaxTree(state), seg.blockFrom, seg.blockTo), state.doc);
    if (edgeCache.size > 4000) edgeCache.clear();
    edgeCache.set(key, e);
  }
  return e;
}

/** The reading panel's own gap below the properties (`.after-props`). */
const PROPS_TAIL: Shape[] = [{ tag: "div", cls: "lp-props-gap" }];

const PROPS_KEY = "chimaera.markdownPropsCollapsed";
function readCollapsed(): boolean {
  try {
    return localStorage.getItem(PROPS_KEY) === "1";
  } catch {
    return false;
  }
}
function writeCollapsed(on: boolean): void {
  try {
    localStorage.setItem(PROPS_KEY, on ? "1" : "0");
  } catch {
    // storage unavailable: the choice lasts for this view
  }
}

/** Segments, identities and edges of the document as it stands. */
function structure(state: EditorState): Pick<LiveState, "segs" | "keys" | "edges" | "fmEnd" | "notes" | "cx"> {
  const doc = state.doc;
  const tree = syntaxTree(state);
  const fmEnd = frontmatterEnd(state);
  const segs = segmentsOf(tree, doc, fmEnd);
  const cx = contextOf(doc, tree, new TextLines(doc), fmEnd);
  const deps = depsOf(cx);
  const keys: string[] = [];
  const edges: Edges[] = [];
  for (const s of segs) {
    if (s.kind === "block") {
      const src = doc.sliceString(s.blockFrom, s.blockTo);
      const key = `${s.names}\u0000${src}\u0000${deps(s.blockFrom, s.blockTo, src)}`;
      keys.push(key);
      edges.push(edgesFor(key, state, s));
    } else if (s.kind === "front") {
      keys.push(`front\u0000${doc.sliceString(0, s.blockTo)}`);
      edges.push({ head: [], tail: PROPS_TAIL });
    } else {
      keys.push("");
      edges.push({ head: [], tail: [] });
    }
  }
  let notes = "";
  if (cx.footnotes.length > 0) {
    notes =
      "\u0001footnotes" +
      footnotesOf(cx)
        .map((f) => {
          const src = doc.sliceString(f.from, f.to);
          return `\u0000${f.label}\u0000${f.n}\u0000${f.refs}\u0000${doc.lineAt(f.from).number}\u0000${src}\u0000${deps(f.from, f.to, src)}`;
        })
        .join("\u0001");
  }
  return { segs, keys, edges, fmEnd, notes, cx };
}

function sameFlags(a: readonly boolean[], b: readonly boolean[]): boolean {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}

/** A rough first guess for a block never measured at this layout: its
 *  source lines wrapped at the column's width. */
function guessHeight(layout: LiveLayout, doc: CmText, from: number, to: number, names: string): number {
  const perLine = Math.max(20, Math.floor(layout.width / layout.char));
  const first = doc.lineAt(from).number;
  const last = doc.lineAt(to).number;
  let rows = 0;
  for (let l = first; l <= Math.min(last, first + 400); l++) {
    const len = doc.line(l).length;
    if (len > 0) rows += Math.max(1, Math.ceil(len / perLine));
  }
  rows += Math.max(0, last - first - 400);
  let h = (rows + 0.6) * layout.line;
  if (names.includes("Heading")) h *= 1.4;
  return Math.round(h);
}

function heightKey(layout: LiveLayout, hid: string): string {
  return `${layout.key}\u0000${hid}`;
}

/** The decorations for `st`: a block widget for every rendered segment, a
 *  gap widget above every revealed one, the footnote section at the end. */
function decorate(st: LiveState, state: EditorState): { deco: DecorationSet; owner: Map<WidgetType, number>; pool: Map<string, LiveWidget> } {
  const ranges: ReturnType<Decoration["range"]>[] = [];
  const owner = new Map<WidgetType, number>();
  const pool = new Map<string, LiveWidget>();
  const doc = state.doc;
  const get = <W extends LiveWidget>(id: string, make: (hid: string, est: (guess: () => number) => number) => W): W => {
    const hit = st.pool.get(id) ?? pool.get(id);
    if (hit !== undefined) {
      pool.set(id, hit);
      return hit as W;
    }
    const hid = hashString(id);
    const w = make(hid, (guess) => heights.get(heightKey(st.layout, hid)) ?? guess());
    pool.set(id, w);
    return w;
  };
  const flashed = (from: number, to: number): boolean => {
    let hit = false;
    st.flash.between(from, to, () => {
      hit = true;
      return false;
    });
    return hit;
  };
  let prev: readonly Shape[] = [];
  let prevKey = "";
  st.segs.forEach((s, i) => {
    const key = st.keys[i];
    if (s.kind === "front") {
      if (!st.revealed[i]) {
        const flash = flashed(s.from, s.to);
        const id = `P\u0000${key}\u0000${st.collapsed}\u0000${flash}`;
        const w = get(id, (hid, est) => new PropsWidget(id, hid, est(() => (st.collapsed ? 40 : 160)), key.slice(6), st.collapsed, flash));
        owner.set(w, i);
        ranges.push(Decoration.replace({ widget: w, block: true }).range(s.from, s.to));
      }
      prev = PROPS_TAIL;
      prevKey = "props";
      return;
    }
    if (s.kind !== "block") return;
    const edges = st.edges[i];
    if (!st.revealed[i]) {
      const flash = flashed(s.from, s.to);
      const id = `B\u0000${key}\u0000${prevKey}\u0000${flash}`;
      const tail = prev;
      const w = get(id, (hid, est) =>
        new BlockWidget(id, hid, est(() => guessHeight(st.layout, doc, s.blockFrom, s.blockTo, s.names)), key, tail, flash),
      );
      owner.set(w, i);
      ranges.push(Decoration.replace({ widget: w, block: true }).range(s.from, s.to));
    } else {
      const id = `G\u0000${prevKey}\u0000${shapeKey(edges.head)}`;
      const tail = prev;
      const w = get(id, (hid, est) => new GapWidget(id, hid, est(() => Math.round(st.layout.line * 0.45)), tail, edges.head));
      owner.set(w, i);
      ranges.push(Decoration.widget({ widget: w, block: true, side: -1 }).range(s.from));
    }
    if (edges.tail.length > 0) {
      prev = edges.tail;
      prevKey = shapeKey(edges.tail);
    }
  });
  if (st.notes !== "") {
    const id = `N\u0000${st.notes}\u0000${prevKey}`;
    const tail = prev;
    const w = get(id, (hid, est) => new NotesWidget(id, hid, est(() => st.layout.line * 3), tail));
    owner.set(w, -1);
    ranges.push(Decoration.widget({ widget: w, block: true, side: 1 }).range(doc.length));
  }
  return { deco: Decoration.set(ranges, true), owner, pool };
}

export const liveField = StateField.define<LiveState>({
  create(state) {
    const base = structure(state);
    const st: LiveState = {
      ...base,
      revealed: revealedOf(base.segs, []),
      focused: false,
      collapsed: readCollapsed(),
      layout: DEFAULT_LAYOUT,
      flash: revealFlashRanges(state),
      deco: Decoration.none,
      owner: new Map(),
      pool: new Map(),
    };
    return { ...st, ...decorate(st, state) };
  },
  update(v, tr) {
    let focused = v.focused;
    let collapsed = v.collapsed;
    let layout = v.layout;
    for (const e of tr.effects) {
      if (e.is(focusEffect)) focused = e.value;
      else if (e.is(layoutEffect)) layout = e.value;
      else if (e.is(propsEffect)) collapsed = e.value;
    }
    const structural = tr.docChanged || syntaxTree(tr.state) !== syntaxTree(tr.startState);
    const flash = revealFlashRanges(tr.state);
    if (
      !structural &&
      tr.selection === undefined &&
      focused === v.focused &&
      collapsed === v.collapsed &&
      layout === v.layout &&
      flash === v.flash
    )
      return v;
    const base = structural ? structure(tr.state) : v;
    let revealed: readonly boolean[] = revealedOf(base.segs, focused ? tr.state.selection.ranges : []);
    const sameSegs = !structural;
    if (sameSegs && sameFlags(revealed, v.revealed)) revealed = v.revealed;
    const st: LiveState = {
      ...v,
      segs: base.segs,
      keys: base.keys,
      edges: base.edges,
      fmEnd: base.fmEnd,
      notes: base.notes,
      cx: base.cx,
      revealed,
      focused,
      collapsed,
      layout,
      flash,
    };
    if (!structural && revealed === v.revealed && collapsed === v.collapsed && flash === v.flash) return st;
    return { ...st, ...decorate(st, tr.state) };
  },
  provide: (f) => [
    EditorView.decorations.from(f, (v) => v.deco),
    EditorView.contentAttributes.compute([f], (state) =>
      state.field(f).fmEnd > 0 ? { class: "lp-has-props" } : ({} as Record<string, string>),
    ),
  ],
});

/** The segments that show as source in `state`, for the inline decorator:
 *  what is revealed, never renders, or lies past the parse. */
export function sourceRanges(state: EditorState, from: number, to: number): { from: number; to: number }[] {
  const st = state.field(liveField, false);
  if (st === undefined) return [{ from, to }];
  const out: { from: number; to: number }[] = [];
  let i = segmentAt(st.segs, from);
  if (i < 0) return st.segs.length === 0 ? [{ from, to }] : [];
  for (; i < st.segs.length && st.segs[i].from <= to; i++) {
    if (!st.revealed[i]) continue;
    const s = st.segs[i];
    const f = Math.max(from, s.from);
    const t = Math.min(to, s.to);
    const last = out[out.length - 1];
    if (last !== undefined && last.to + 1 >= f) last.to = t;
    else if (t >= f) out.push({ from: f, to: t });
  }
  return out;
}

/** Whether `[from, to]` lies in a segment drawn as rendered DOM. */
export function renderedAt(state: EditorState, from: number): boolean {
  const st = state.field(liveField, false);
  if (st === undefined) return false;
  const i = segmentAt(st.segs, from);
  return i >= 0 && !st.revealed[i];
}

/** Fold or unfold the properties panel in a live view (the reading panel's
 *  toggle keeps the two in step). */
export function setLivePropsCollapsed(view: EditorView, collapsed: boolean): void {
  const st = view.state.field(liveField, false);
  if (st !== undefined && st.collapsed !== collapsed) view.dispatch({ effects: propsEffect.of(collapsed) });
}

// --- the widgets ----------------------------------------------------------------------

/** Per-view page hooks for rendered blocks (images, fences, diagrams,
 *  equations), made on first use. */
const hydrators = new WeakMap<EditorView, Hydrator>();

function hydratorFor(view: EditorView): Hydrator {
  let h = hydrators.get(view);
  if (h === undefined) {
    h = new Hydrator({
      docPath: view.state.facet(docPath),
      links: view.state.facet(linkContext),
      theme: view.state.facet(liveHost).theme(),
      onLayout: () => view.requestMeasure(),
    });
    hydrators.set(view, h);
  }
  return h;
}

/** Re-lay out a live view's diagrams for a theme change. */
export function setLiveTheme(view: EditorView, theme: "light" | "dark"): void {
  hydrators.get(view)?.setTheme(theme, view.contentDOM);
}

/** Which content a widget DOM shows (updateDOM reuses it when only the
 *  ghost or the flash changed). */
const drawn = new WeakMap<HTMLElement, string>();

/** A zero-height copy of an edge chain: its margins take part in collapsing
 *  (a `tail` ghost lends its bottom margins, a `head` ghost its top). */
function ghost(chain: readonly Shape[], edge: "tail" | "head"): HTMLElement | null {
  let root: HTMLElement | null = null;
  let parent: HTMLElement | null = null;
  for (const s of chain) {
    const el = document.createElement(s.tag);
    if (s.cls !== "") el.className = s.cls;
    if (parent === null) root = el;
    else parent.append(el);
    parent = el;
  }
  if (root === null) return null;
  root.classList.add(edge === "tail" ? "lp-ghost-tail" : "lp-ghost-head");
  root.setAttribute("aria-hidden", "true");
  return root;
}

function setGhost(root: HTMLElement, chain: readonly Shape[]): void {
  const old = root.firstElementChild;
  if (old !== null && old.classList.contains("lp-ghost-tail")) old.remove();
  const g = ghost(chain, "tail");
  if (g !== null) root.prepend(g);
}

const MOUSE_EVENTS = new Set(["mousedown", "mouseup", "click", "dblclick", "auxclick", "contextmenu"]);

abstract class LiveWidget extends WidgetType {
  constructor(
    /** Identity: equal ids draw equal DOM. */
    readonly id: string,
    /** The id's hash, for the height cache. */
    readonly hid: string,
    readonly est: number,
  ) {
    super();
  }
  override eq(other: WidgetType): boolean {
    return other instanceof LiveWidget && other.id === this.id;
  }
  override get estimatedHeight(): number {
    return this.est;
  }
  /** Mouse events go to the editor (its handlers and the mouse selection
   *  style below place the cursor); a press on a scroller's own bar is
   *  left to scroll. */
  override ignoreEvent(e: Event): boolean {
    if (!MOUSE_EVENTS.has(e.type)) return true;
    if (e.type === "mousedown" && e.target instanceof Element) {
      const scroller = e.target.closest(".md-doc table, .md-doc pre > code, .md-math-display, .md-mermaid-svg");
      if (onScrollbarBand(scroller, e as MouseEvent)) return true;
    }
    return false;
  }
}

/** Draw the document nodes of `seg` into `root` as the reader does. */
function drawRun(view: EditorView, root: HTMLElement, seg: Segment): void {
  const st = view.state.field(liveField);
  const h = hydratorFor(view);
  renderRun(root, nodesIn(syntaxTree(view.state), seg.blockFrom, seg.blockTo), { t: h.target, cx: st.cx });
}

/** What every rendered widget does once drawn: the reader's decorations,
 *  and a re-measure whenever something inside loads late. */
function finish(view: EditorView, root: HTMLElement): void {
  markTasks(root);
  decorateCopyTargets(root);
  for (const img of root.querySelectorAll("img")) img.addEventListener("load", () => view.requestMeasure());
  const h = hydratorFor(view);
  // Fences paint and equations typeset once the node is in the page (a
  // detached node is skipped), before CodeMirror measures it.
  queueMicrotask(() => {
    if (root.isConnected) h.settle([root]);
  });
}

class BlockWidget extends LiveWidget {
  constructor(
    id: string,
    hid: string,
    est: number,
    /** The block's own identity (its text and what it reads). */
    readonly block: string,
    readonly prev: readonly Shape[],
    readonly flash: boolean,
  ) {
    super(id, hid, est);
  }
  toDOM(view: EditorView): HTMLElement {
    const root = document.createElement("div");
    root.className = this.flash ? "lp-block md-doc md-flash" : "lp-block md-doc";
    const st = view.state.field(liveField);
    const seg = st.segs[st.owner.get(this) ?? -1];
    if (seg === undefined) return root;
    root.dataset.lpLine = String(view.state.doc.lineAt(seg.blockFrom).number);
    const g = ghost(this.prev, "tail");
    if (g !== null) root.append(g);
    drawRun(view, root, seg);
    finish(view, root);
    drawn.set(root, this.block);
    return root;
  }
  /** Same block with another neighbour above, or a flash: only the ghost
   *  and the class change. */
  override updateDOM(dom: HTMLElement): boolean {
    if (drawn.get(dom) !== this.block) return false;
    setGhost(dom, this.prev);
    dom.classList.toggle("md-flash", this.flash);
    return true;
  }
}

/** The gap above a revealed block: the same collapsed margin its rendered
 *  form would have had, so its first source line keeps its place. */
class GapWidget extends LiveWidget {
  constructor(
    id: string,
    hid: string,
    est: number,
    readonly prev: readonly Shape[],
    readonly head: readonly Shape[],
  ) {
    super(id, hid, est);
  }
  toDOM(): HTMLElement {
    const root = document.createElement("div");
    root.className = "lp-gap md-doc";
    root.setAttribute("aria-hidden", "true");
    const t = ghost(this.prev, "tail");
    const h = ghost(this.head, "head");
    if (t !== null) root.append(t);
    if (h !== null) root.append(h);
    return root;
  }
}

/** The footnote section comrak appends, at the end of the document. */
class NotesWidget extends LiveWidget {
  constructor(
    id: string,
    hid: string,
    est: number,
    readonly prev: readonly Shape[],
  ) {
    super(id, hid, est);
  }
  toDOM(view: EditorView): HTMLElement {
    const root = document.createElement("div");
    root.className = "lp-block lp-notes md-doc";
    root.dataset.lpLine = "1";
    const g = ghost(this.prev, "tail");
    if (g !== null) root.append(g);
    const st = view.state.field(liveField);
    renderFootnotes(root, footnotesOf(st.cx), { t: hydratorFor(view).target, cx: st.cx });
    finish(view, root);
    return root;
  }
}

const CHEVRON =
  '<svg viewBox="0 0 16 16" width="10" height="10" aria-hidden="true"><path d="M6 4l4 4-4 4" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>';

/** The frontmatter as the reading view's properties panel (the same
 *  markup and CSS; every value is text). A press on a property edits its
 *  line; the header folds the panel. */
class PropsWidget extends LiveWidget {
  constructor(
    id: string,
    hid: string,
    est: number,
    readonly raw: string,
    readonly collapsed: boolean,
    readonly flash: boolean,
  ) {
    super(id, hid, est);
  }
  toDOM(): HTMLElement {
    const fm = frontmatterOf(this.raw);
    const yaml = fm?.raw ?? "";
    const entries = parseFrontmatter(yaml);
    const root = document.createElement("section");
    root.className = this.collapsed ? "md-props lp-props collapsed" : "md-props lp-props";
    if (this.flash) root.classList.add("md-flash");
    root.setAttribute("aria-label", "properties");
    const head = document.createElement("button");
    head.type = "button";
    head.className = "md-props-head";
    head.setAttribute("aria-expanded", String(!this.collapsed));
    const chev = document.createElement("span");
    chev.className = this.collapsed ? "lp-chev" : "lp-chev open";
    chev.innerHTML = CHEVRON; // a constant glyph
    const label = document.createElement("span");
    label.textContent = "properties";
    head.append(chev, label);
    if (entries !== null) {
      const count = document.createElement("span");
      count.className = "md-props-count";
      count.textContent = String(entries.length);
      head.append(count);
    }
    root.append(head);
    if (this.collapsed) return root;
    if (entries !== null && entries.length > 0) {
      const dl = document.createElement("dl");
      dl.className = "md-props-list";
      for (const prop of entries) {
        const dt = document.createElement("dt");
        dt.textContent = prop.key;
        const dd = document.createElement("dd");
        dd.dataset.key = prop.key;
        const v = prop.value;
        const span = (cls: string, text: string): HTMLElement => {
          const s = document.createElement("span");
          s.className = cls;
          s.textContent = text;
          return s;
        };
        if (v.kind === "list") {
          if (v.items.length === 0) dd.append(span("md-prop-empty", "—"));
          for (const item of v.items) dd.append(span("md-prop-chip", item));
        } else if (v.kind === "bool") {
          const box = span("md-task", "");
          box.dataset.task = v.value ? "done" : "todo";
          box.setAttribute("role", "img");
          box.setAttribute("aria-label", v.value ? "true" : "false");
          dd.append(box);
        } else if (v.kind === "raw") {
          const pre = document.createElement("pre");
          pre.className = "md-prop-raw";
          pre.textContent = v.text;
          dd.append(pre);
        } else {
          dd.append(v.text === "" ? span("md-prop-empty", "—") : span("md-prop-text", v.text));
        }
        dl.append(dt, dd);
      }
      root.append(dl);
    } else {
      const pre = document.createElement("pre");
      pre.className = "md-props-raw";
      pre.textContent = stripFences(yaml);
      root.append(pre);
    }
    return root;
  }
}

// --- presses on rendered blocks ---------------------------------------------------------

/** The rendered widget a DOM node sits in, and the segment it draws. */
function hitOf(view: EditorView, target: EventTarget | null): { root: HTMLElement; seg: number } | null {
  if (!(target instanceof Element)) return null;
  const root = target.closest<HTMLElement>(".lp-block, .lp-props");
  if (root === null || !view.contentDOM.contains(root)) return null;
  const st = view.state.field(liveField, false);
  if (st === undefined) return null;
  if (root.classList.contains("lp-notes")) return { root, seg: -1 };
  // The widget's position, resolved from its DOM now (edits elsewhere may
  // have moved it since it was drawn).
  let pos: number;
  try {
    pos = view.posAtDOM(root);
  } catch {
    return null;
  }
  const i = segmentAt(st.segs, pos);
  return i < 0 ? null : { root, seg: i };
}

type Caret = { node: Node; offset: number };

function caretAt(x: number, y: number): Caret | null {
  const d = document as Document & {
    caretPositionFromPoint?(x: number, y: number): { offsetNode: Node; offset: number } | null;
    caretRangeFromPoint?(x: number, y: number): Range | null;
  };
  if (typeof d.caretPositionFromPoint === "function") {
    const p = d.caretPositionFromPoint(x, y);
    if (p !== null) return { node: p.offsetNode, offset: p.offset };
  }
  if (typeof d.caretRangeFromPoint === "function") {
    const r = d.caretRangeFromPoint(x, y);
    if (r !== null) return { node: r.startContainer, offset: r.startOffset };
  }
  return null;
}

/** Elements whose text is not the source's: an equation reads as its TeX
 *  (MathML's annotation), a laid-out diagram as nothing. */
function atomText(el: Element): string | null {
  if (el.classList.contains("md-math")) {
    return el.querySelector('annotation[encoding="application/x-tex"]')?.textContent ?? el.textContent ?? "";
  }
  if (el.classList.contains("md-mermaid-svg") || el.classList.contains("lp-ghost-tail")) return "";
  if (el.tagName === "BUTTON") return "";
  return null;
}

/** The rendered text of `root` before the caret. */
function renderedPrefix(root: Element, caret: Caret): string {
  let out = "";
  let done = false;
  const walk = (n: Node): void => {
    if (done) return;
    if (n === caret.node) {
      if (n instanceof Text) out += n.data.slice(0, caret.offset);
      else for (let i = 0; i < caret.offset && i < n.childNodes.length; i++) walk(n.childNodes[i]);
      done = true;
      return;
    }
    if (n instanceof Text) {
      out += n.data;
      return;
    }
    if (n instanceof Element) {
      const atom = atomText(n);
      if (atom !== null) {
        if (n.contains(caret.node)) done = true;
        else out += atom;
        return;
      }
      for (const c of Array.from(n.childNodes)) {
        walk(c);
        if (done) return;
      }
    }
  };
  walk(root);
  return out;
}

/** The top of the line box a caret sits in, in document coordinates. */
function caretTop(view: EditorView, caret: Caret | null, fallbackY: number): number {
  let top = fallbackY - view.defaultLineHeight / 2;
  if (caret !== null) {
    try {
      const r = document.createRange();
      r.setStart(caret.node, caret.offset);
      r.collapse(true);
      const rect = r.getBoundingClientRect();
      if (rect.height > 0) top = rect.top - Math.max(0, (view.defaultLineHeight - rect.height) / 2);
    } catch {
      // a caret the range API refuses: the pointer stands in
    }
  }
  return top - view.documentTop;
}

/** Where a press on rendered content lands in the source. */
function pressPos(view: EditorView, hit: { root: HTMLElement; seg: number }, e: MouseEvent): number {
  const state = view.state;
  const st = state.field(liveField);
  const doc = state.doc;
  const seg = st.segs[hit.seg];
  const fallback = seg?.blockFrom ?? doc.length;
  const caret = caretAt(e.clientX, e.clientY);
  const at: Element | null =
    caret === null
      ? e.target instanceof Element
        ? e.target
        : null
      : caret.node instanceof Element
        ? caret.node
        : caret.node.parentElement;
  if (at === null || !hit.root.contains(at)) return fallback;
  if (hit.root.classList.contains("lp-props")) return propsPos(state, at, seg);
  const cell = at.closest<HTMLTableCellElement>("th, td");
  if (cell !== null && seg !== undefined) {
    const p = cellPos(state, seg, cell, caret);
    if (p !== null) return p;
  }
  const block = at.closest<HTMLElement>("[data-sourcepos]");
  const range = block === null || !hit.root.contains(block) ? null : parseSourcepos(block.getAttribute("data-sourcepos"));
  if (range === null) return fallback;
  const shift = seg === undefined ? 0 : doc.lineAt(seg.blockFrom).number - Number(hit.root.dataset.lpLine ?? "1");
  const first = Math.min(Math.max(1, range.start + shift), doc.lines);
  const last = Math.min(Math.max(first, range.end + shift), doc.lines);
  const from = doc.line(first).from;
  const to = doc.line(last).to;
  if (caret === null || block === null) return from;
  return sourceOffset(syntaxTree(state), doc, from, to, renderedPrefix(block, caret));
}

/** A press in a rendered table cell: that cell's text, at the character. */
function cellPos(state: EditorState, seg: Segment, cell: HTMLTableCellElement, caret: Caret | null): number | null {
  const table = tableAt(state, seg.blockFrom);
  const tr = cell.parentElement as HTMLTableRowElement | null;
  if (table === null || tr === null) return null;
  const row: RowModel | undefined =
    tr.parentElement?.tagName === "THEAD" ? table.header : table.rows[tr.sectionRowIndex];
  const c = row?.cells[cell.cellIndex];
  if (row === undefined || c === undefined) return null;
  const next = row.cells[cell.cellIndex + 1];
  const end = next === undefined ? row.end : Math.max(c.from, next.from - 1);
  if (caret === null || cell.cellIndex >= row.present) return c.from;
  return sourceOffset(syntaxTree(state), state.doc, c.from, end, renderedPrefix(cell, caret));
}

/** A press on a property: after its key on its line. */
function propsPos(state: EditorState, at: Element, seg: Segment | undefined): number {
  const doc = state.doc;
  const second = doc.lines >= 2 ? doc.line(2).from : 0;
  const dd = at.closest<HTMLElement>("dd") ?? (at.closest("dt")?.nextElementSibling as HTMLElement | null);
  const key = dd?.dataset.key;
  if (key === undefined || seg === undefined) return second;
  const last = doc.lineAt(seg.blockTo).number;
  for (let l = 2; l < last; l++) {
    const line = doc.line(l);
    if (line.text.startsWith(`${key}:`)) {
      const m = /^[^:]*:[ \t]*/.exec(line.text);
      return line.from + (m?.[0].length ?? 0);
    }
  }
  return second;
}

/** The last press that entered a rendered block (a double-click's second
 *  press lands on the source it revealed). */
let lastEntry: { view: EditorView; at: number } | null = null;

/** Where the next transaction from a state should keep its place: set by a
 *  press or an arrow key just before it dispatches. `text` anchors put the
 *  line holding `pos` at `top` (a press: the text under the pointer stays
 *  there); otherwise `top` is where the block starting at `pos` began. */
interface Anchor {
  pos: number;
  top: number;
  text: boolean;
  focus: boolean;
}
const pendingAnchor = new WeakMap<EditorState, Anchor>();

/** A press on rendered text starts CodeMirror's own mouse selection with
 *  positions mapped through the rendering: a click lands at the character,
 *  a drag extends over source and rendered blocks alike, Shift extends. */
const renderedSelection = EditorView.mouseSelectionStyle.of((view, event) => {
  if (event.button !== 0) return null;
  const hit = hitOf(view, event.target);
  if (hit === null) return null;
  const st = view.state.field(liveField);
  const start = pressPos(view, hit, event);
  pendingAnchor.set(view.state, {
    pos: start,
    top: caretTop(view, caretAt(event.clientX, event.clientY), event.clientY),
    text: true,
    focus: !st.focused,
  });
  lastEntry = { view, at: Date.now() };
  let anchor = start;
  let startSel = view.state.selection;
  const posOf = (e: MouseEvent): number => {
    const h = hitOf(view, e.target);
    if (h !== null) return pressPos(view, h, e);
    return view.posAtCoords({ x: e.clientX, y: e.clientY }, false);
  };
  return {
    update(u: ViewUpdate) {
      if (u.docChanged) {
        anchor = u.changes.mapPos(anchor);
        startSel = startSel.map(u.changes);
      }
    },
    get(cur: MouseEvent, extend: boolean, multiple: boolean) {
      const head = cur === event ? start : posOf(cur);
      if (extend) return startSel.replaceRange(startSel.main.extend(head));
      if (multiple) return startSel.addRange(EditorSelection.cursor(head));
      return EditorSelection.single(anchor, head);
    },
  };
});

/** The source line a rendered task box stands for, toggled in place. */
function toggleTask(view: EditorView, hit: { root: HTMLElement; seg: number }, box: HTMLElement): void {
  const state = view.state;
  if (state.readOnly) return;
  const item = box.closest<HTMLElement>("[data-sourcepos]");
  const range = item === null ? null : parseSourcepos(item.getAttribute("data-sourcepos"));
  if (range === null) return;
  const st = state.field(liveField);
  const seg = st.segs[hit.seg];
  const shift = seg === undefined ? 0 : state.doc.lineAt(seg.blockFrom).number - Number(hit.root.dataset.lpLine ?? "1");
  const n = range.start + shift;
  if (n < 1 || n > state.doc.lines) return;
  const line = state.doc.line(n);
  let marker: { from: number; to: number } | null = null;
  syntaxTree(state).iterate({
    from: line.from,
    to: line.to,
    enter: (node) => {
      if (marker === null && node.name === "TaskMarker") marker = { from: node.from, to: node.to };
      return marker === null;
    },
  });
  const m = marker as { from: number; to: number } | null;
  if (m === null) return;
  const done = /x/i.test(state.doc.sliceString(m.from, m.to));
  view.dispatch({
    changes: { from: m.from, to: m.to, insert: done ? "[ ]" : "[x]" },
    userEvent: "input.toggle",
  });
}

/** A padded (empty, pipe-less) cell of a short row: pressing it writes the
 *  missing pipes first, so typing lands in that column. */
function fillCell(view: EditorView, hit: { root: HTMLElement; seg: number }, cell: HTMLTableCellElement): boolean {
  const st = view.state.field(liveField);
  const seg = st.segs[hit.seg];
  if (seg === undefined || view.state.readOnly) return false;
  const table = tableAt(view.state, seg.blockFrom);
  const tr = cell.parentElement as HTMLTableRowElement | null;
  if (table === null || tr === null) return false;
  const row = tr.parentElement?.tagName === "THEAD" ? table.header : table.rows[tr.sectionRowIndex];
  if (row === undefined || cell.cellIndex < row.present) return false;
  const fill = completeRow(row, table.header.cells.length, cell.cellIndex);
  if (fill === null) return false;
  pendingAnchor.set(view.state, {
    pos: fill.anchor,
    top: cell.getBoundingClientRect().top - view.documentTop,
    text: true,
    focus: !st.focused,
  });
  view.dispatch({
    changes: { from: fill.from, to: fill.to, insert: fill.insert },
    selection: { anchor: fill.anchor },
    userEvent: "select.pointer",
  });
  view.focus();
  return true;
}

/** Presses on rendered content that are not cursor placement: a task box,
 *  a copy button, the properties fold, Mod+press on a link, a short row's
 *  missing cell, a link's own click (never a navigation), its menu. */
const renderedEvents = EditorView.domEventHandlers({
  mousedown(e, view) {
    if (e.button !== 0) return false;
    const hit = hitOf(view, e.target);
    if (hit === null || !(e.target instanceof Element)) return false;
    const t = e.target;
    if (t.closest("button.md-copy") !== null) return true; // copies on click
    const box = t.closest<HTMLElement>(".md-task");
    if (box !== null && box.closest(".lp-props") === null) {
      toggleTask(view, hit, box);
      return true;
    }
    if (t.closest(".md-props-head") !== null) {
      const st = view.state.field(liveField);
      writeCollapsed(!st.collapsed);
      view.dispatch({ effects: propsEffect.of(!st.collapsed) });
      return true;
    }
    const a = t.closest<HTMLAnchorElement>("a[href]");
    if (a !== null && (e.metaKey || e.ctrlKey)) {
      const href = a.getAttribute("href") ?? "";
      if (isFollowable(href)) followLiveLink(view, href, e, a.hasAttribute("data-wikilink"));
      return true;
    }
    const cell = t.closest<HTMLTableCellElement>("th, td");
    if (cell !== null && fillCell(view, hit, cell)) return true;
    return false;
  },
  click(e, view) {
    const hit = hitOf(view, e.target);
    if (hit === null || !(e.target instanceof Element)) return false;
    const copy = e.target.closest<HTMLElement>("button.md-copy");
    if (copy !== null) {
      const payload = copyPayload(copy);
      if (payload.length > 0)
        void copyText(payload).then((ok) => {
          if (!ok || !copy.isConnected) return;
          copy.classList.add("copied");
          setTimeout(() => copy.classList.remove("copied"), 1400);
        });
      return true;
    }
    // A link in the rendering is text you edit: its click must never
    // navigate the workbench (Mod+press already followed it).
    return e.target.closest("a[href]") !== null;
  },
  auxclick(e, view) {
    if (e.button !== 1 || hitOf(view, e.target) === null || !(e.target instanceof Element)) return false;
    const a = e.target.closest<HTMLAnchorElement>("a[href]");
    const href = a?.getAttribute("href") ?? "";
    if (a === null || isWebUrl(href)) return false;
    if (isFollowable(href))
      followLiveLink(view, href, { clientX: e.clientX, clientY: e.clientY, shiftKey: true }, a.hasAttribute("data-wikilink"));
    return true;
  },
  contextmenu(e, view) {
    if (hitOf(view, e.target) === null || !(e.target instanceof Element)) return false;
    const href = e.target.closest("a[href]")?.getAttribute("href") ?? "";
    const url = href === "" ? null : webUrl(linkDestination(href));
    if (url === null) return false;
    contextMenu.openAt(e, urlMenuEntries(url));
    return true;
  },
  dblclick(_e, view) {
    // The first press revealed the block; the second landed on its source
    // as a first click. Select the word, as a double-click would have.
    const entry = lastEntry;
    if (entry === null || entry.view !== view || Date.now() - entry.at > 700) return false;
    lastEntry = null;
    const sel = view.state.selection.main;
    if (!sel.empty) return false;
    const word = view.state.wordAt(sel.head);
    if (word === null) return false;
    view.dispatch({ selection: { anchor: word.from, head: word.to }, userEvent: "select.pointer" });
    return true;
  },
});

// --- arrow keys into rendered blocks --------------------------------------------------------

/** ArrowUp/ArrowDown off the edge of the current text line into a rendered
 *  block: land on the block's first (or last) line at the same column and
 *  reveal it, keeping the block's edge where it was on screen. */
function stepInto(forward: boolean, extend: boolean) {
  return (view: EditorView): boolean => {
    const st = view.state.field(liveField, false);
    if (st === undefined) return false;
    const doc = view.state.doc;
    const sel = view.state.selection.main;
    const line = doc.lineAt(sel.head);
    // Still inside this line's wrapped rows: the default motion.
    const moved = view.moveVertically(sel, forward);
    if (moved.head >= line.from && moved.head <= line.to && moved.head !== sel.head) return false;
    const probe = forward ? line.to + 1 : line.from - 1;
    if (probe < 0 || probe > doc.length) return false;
    const i = segmentAt(st.segs, probe);
    const seg = st.segs[i];
    if (seg === undefined || st.revealed[i] || (seg.kind !== "block" && seg.kind !== "front")) return false;
    const target = forward ? doc.lineAt(seg.blockFrom) : doc.lineAt(Math.max(seg.blockFrom, seg.blockTo));
    const col = sel.goalColumn ?? sel.head - line.from;
    const pos = Math.min(target.from + col, target.to);
    // Forward the block's top stays put; backward the boundary below it.
    const edge = forward || i + 1 >= st.segs.length ? seg.from : st.segs[i + 1].from;
    pendingAnchor.set(view.state, { pos: edge, top: view.lineBlockAt(edge).top, text: false, focus: !st.focused });
    view.dispatch({
      selection: extend ? EditorSelection.range(sel.anchor, pos) : EditorSelection.cursor(pos, 0, undefined, col),
      userEvent: "select",
    });
    return true;
  };
}

const arrowKeys = Prec.high(
  keymap.of([
    { key: "ArrowDown", run: stepInto(true, false), shift: stepInto(true, true) },
    { key: "ArrowUp", run: stepInto(false, false), shift: stepInto(false, true) },
  ]),
);

// --- keeping the acted-on line in place ----------------------------------------------------

/** What an anchor may choose from, read before the transaction applies. */
interface Before {
  /** An explicit anchor (a press, an arrow key), old coordinates. */
  exact: Anchor | null;
  /** The first change's line, and the new cursor's line, in the old
   *  height map. */
  changePos: number;
  changeTop: number;
  headPos: number;
  headTop: number;
  /** The visible band, document coordinates. */
  viewTop: number;
  viewBottom: number;
}
const beforeAnn = Annotation.define<Before>();

/** Live views by their current state (the extender below finds its view). */
const liveViews = new Set<EditorView>();

const readBefore = EditorState.transactionExtender.of((tr) => {
  const pend = pendingAnchor.get(tr.startState);
  if (!tr.docChanged && tr.selection === undefined && pend === undefined) return null;
  let view: EditorView | null = null;
  for (const v of liveViews) if (v.state === tr.startState) view = v;
  if (view === null) return null;
  if (pend !== undefined) pendingAnchor.delete(tr.startState);
  let changePos = -1;
  tr.changes.iterChangedRanges((fromA) => {
    if (changePos < 0) changePos = fromA;
  });
  const head = tr.newSelection.main.head;
  const headPos = tr.docChanged ? tr.changes.invertedDesc.mapPos(head, -1) : head;
  const rect = view.scrollDOM.getBoundingClientRect();
  const docTop = view.documentTop;
  const before: Before = {
    exact: pend ?? null,
    changePos,
    changeTop: changePos < 0 ? 0 : view.lineBlockAt(changePos).top,
    headPos,
    headTop: view.lineBlockAt(headPos).top,
    viewTop: rect.top - docTop,
    viewBottom: rect.bottom - docTop,
  };
  const effects = pend?.focus === true ? [focusEffect.of(true)] : [];
  return { annotations: beforeAnn.of(before), effects };
});

/** Point CodeMirror's scroll anchor (the block it keeps in place across a
 *  re-measure) at `pos`, whose line box sat at `top` before. Its public
 *  alternative — a scroll target — is resolved by coordinates, which a
 *  block swap has just invalidated. Guarded: a CodeMirror without these
 *  fields keeps its own anchoring. */
function anchorAt(view: EditorView, pos: number, top: number, dropTarget: boolean): void {
  const vs = (view as unknown as { viewState?: Record<string, unknown> }).viewState;
  if (vs === undefined || typeof vs.scrollAnchorHeight !== "number" || typeof vs.scrollAnchorPos !== "number") return;
  vs.scrollAnchorPos = pos;
  vs.scrollAnchorHeight = top;
  if (dropTarget && "scrollTarget" in vs) vs.scrollTarget = null;
}

// --- the view side ----------------------------------------------------------------------------

/** Focus, layout, heights and anchoring for one live view. */
const liveView = ViewPlugin.fromClass(
  class {
    private readonly onFocus = (): void => queueMicrotask(() => this.syncFocus());
    private readonly heightsKey = {};
    constructor(readonly view: EditorView) {
      liveViews.add(view);
      view.dom.addEventListener("focusin", this.onFocus);
      view.dom.addEventListener("focusout", this.onFocus);
      this.onFocus();
      view.requestMeasure({ key: this.heightsKey, read: (v) => this.record(v) });
    }
    update(u: ViewUpdate): void {
      const st = u.state.field(liveField);
      const was = u.startState.field(liveField);
      const before = u.transactions[0]?.annotation(beforeAnn);
      if (before !== undefined) this.keepPlace(u, before, st.revealed !== was.revealed || st.segs !== was.segs);
      if (u.geometryChanged || u.heightChanged || u.viewportChanged || u.docChanged)
        this.view.requestMeasure({ key: this.heightsKey, read: (v) => this.record(v) });
    }
    destroy(): void {
      liveViews.delete(this.view);
      this.view.dom.removeEventListener("focusin", this.onFocus);
      this.view.dom.removeEventListener("focusout", this.onFocus);
      hydrators.get(this.view)?.destroy();
      hydrators.delete(this.view);
    }
    private syncFocus(): void {
      const view = this.view;
      if (!liveViews.has(view)) return;
      const focused = view.dom.contains(document.activeElement);
      if (view.state.field(liveField).focused !== focused) view.dispatch({ effects: focusEffect.of(focused) });
    }
    /** The line being acted on keeps its place when blocks swap (or an
     *  outside write lands above the cursor). */
    private keepPlace(u: ViewUpdate, b: Before, revealChanged: boolean): void {
      const outside =
        u.docChanged && !u.transactions.some((tr) => tr.annotation(Transaction.userEvent) !== undefined);
      let pos: number;
      let top: number;
      let text = false;
      if (b.exact !== null) {
        pos = u.changes.mapPos(b.exact.pos);
        top = b.exact.top;
        text = b.exact.text;
      } else if (outside) {
        // Someone else's write: the cursor's line stays where you were typing.
        pos = u.state.selection.main.head;
        top = b.headTop;
      } else if (u.docChanged && b.changePos >= 0) {
        // Your own edit that swaps blocks (a new paragraph renders the one
        // above it): the line being edited stays.
        pos = u.changes.mapPos(b.changePos);
        top = b.changeTop;
      } else if (revealChanged) {
        pos = u.state.selection.main.head;
        top = b.headTop;
      } else {
        return;
      }
      if (top < b.viewTop - 4 || top > b.viewBottom) return; // off screen: the default holds
      if (text) {
        // CodeMirror anchors whole line blocks, and a revealed block's
        // first line carries its gap widget above it: aim the block so its
        // text line lands at `top`.
        const block = this.view.lineBlockAt(pos);
        if (Array.isArray(block.type))
          for (const part of block.type as readonly BlockInfo[])
            if (part.type === BlockType.Text && part.from <= pos && pos <= part.to) top -= part.top - block.top;
      }
      // The cursor's own scroll-into-view would undo the anchoring; it is
      // only needed when the cursor is near the bottom edge.
      const line = this.view.defaultLineHeight;
      const drop = b.headTop >= b.viewTop && b.headTop <= b.viewBottom - 3 * line;
      anchorAt(this.view, pos, top, drop);
    }
    /** Record measured widget heights, and the layout they were measured
     *  at (a changed layout goes into the state for new estimates). */
    private record(view: EditorView): null {
      const width = view.contentDOM.clientWidth;
      if (width <= 0) return null;
      const key = `${Math.round(width)}:${view.defaultLineHeight.toFixed(1)}:${view.defaultCharacterWidth.toFixed(2)}`;
      const st = view.state.field(liveField);
      const layout = st.layout.key === key ? st.layout : { key, line: view.defaultLineHeight, char: view.defaultCharacterWidth, width };
      const note = (b: BlockInfo): void => {
        const w = b.widget;
        if (w instanceof LiveWidget && b.height > 0) heights.set(heightKey(layout, w.hid), b.height);
      };
      for (const b of view.viewportLineBlocks) {
        if (Array.isArray(b.type)) for (const part of b.type as readonly BlockInfo[]) note(part);
        else note(b);
      }
      if (layout !== st.layout)
        queueMicrotask(() => {
          if (liveViews.has(view) && view.state.field(liveField).layout.key !== key)
            view.dispatch({ effects: layoutEffect.of(layout) });
        });
      return null;
    }
  },
);

/** The rendered-document behaviour for live mode (paired with mdLive's
 *  inline decorations for the revealed blocks). */
export function renderedDocument(host: LiveHost): Extension {
  return [liveHost.of(host), liveField, liveView, readBefore, renderedSelection, renderedEvents, arrowKeys];
}
