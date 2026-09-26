/**
 * The reading view's engine: a document's current text in, the article's
 * DOM kept in step with it — incrementally. Each top-level block is keyed by
 * its source text plus the document-wide facts its rendering reads (heading
 * ids, footnote numbers, reference definitions); a block whose key survives
 * an update keeps its DOM nodes (only its `data-sourcepos` lines shift), so
 * an agent rewriting one paragraph never re-flows, re-decodes or re-typesets
 * anything else. The parse is incremental too (lezer fragments), so a
 * keystroke-sized edit costs a keystroke-sized parse.
 *
 * It also owns what only a page can do for the DOM target: images resolve
 * beside the document through short-lived `/raw/` tickets (memoized, so a
 * re-rendered block keeps its src and never flashes), `![[embeds]]` by name
 * through `/fs/validate`, fences highlight through the lezer highlighter
 * live mode uses (`codeHighlight`, lazy per language — only that block
 * repaints when its grammar arrives), mermaid lays out per theme, and
 * equations typeset under the shared KaTeX policy, time-sliced.
 */
import type { Parser, Tree } from "@lezer/common";
import { LanguageDescription } from "@codemirror/language";
import { languages } from "@codemirror/language-data";
import { highlightCode } from "@lezer/highlight";
import { codeHighlight } from "../cm";
import { dirname, fsValidate, lastRawTicketUrl, rawTicketUrl, resolveDocPath, safeDecodeUri } from "../files";
import { loadMath, mathNow } from "../mathLoad";
import { renderMermaid } from "../../shared/mermaid";
import type { LinkContext } from "../docLinks";
import {
  bodyText,
  depsOf,
  htmlOfNode,
  htmlRuns,
  documentContext,
  footnotesOf,
  type OutlineEntry,
} from "./model";
import { DomTarget, renderFootnotes, renderRun, topLevel, type DomHooks } from "./render";

export type ReaderOptions = HydratorOptions;

export interface ReadResult {
  /** The leading YAML block's text (the properties panel), or null. */
  frontmatter: string | null;
  /** The body's headings, with their 1-based source lines. */
  outline: (OutlineEntry & { line: number })[];
  /** Top-level nodes drawn fresh this update (the rest kept theirs). */
  fresh: Node[];
}

interface Unit {
  key: string;
  nodes: Node[];
  /** The source line the unit's nodes were drawn (or last shifted) for. */
  line: number;
}

// --- images -----------------------------------------------------------------------

/** `![[name]]` resolutions kept (a document rarely names more). */
const NAMED_MAX = 256;

/** Point `img` at `path`'s `/raw/` URL: at once with the last answer, so a
 *  block re-rendered for an edit next to its image keeps its src (no
 *  flash), then with the daemon's current one when that differs — the
 *  image changed on disk since (a new version is a new ticket). */
function pointAt(img: HTMLImageElement, path: string, stamp: string): void {
  const last = lastRawTicketUrl(path);
  if (last !== null) img.src = last;
  rawTicketUrl(path).then(
    (url) => {
      if (img.dataset.mdSrc === stamp && url !== last) img.src = url;
    },
    () => {
      // missing/unreadable target: the alt text shows
    },
  );
}

/** `![[name]]` targets by name, per folder and workspace (the daemon's
 *  resolver: beside the document, then a unique match in the workspace). */
const named = new Map<string, Promise<string | null>>();

function resolveNamed(name: string, dir: string, ws: string | null): Promise<string | null> {
  const key = `${ws ?? ""}\u0000${dir}\u0000${name}`;
  let p = named.get(key);
  if (p === undefined) {
    p = fsValidate([name], dir, ws).then(
      (res) => {
        const hit = res.valid[name];
        return hit !== undefined && hit.kind === "file" ? hit.path : null;
      },
      () => {
        named.delete(key); // transient: ask again next render
        return null;
      },
    );
    named.set(key, p);
    if (named.size > NAMED_MAX) {
      const oldest = named.keys().next().value;
      if (oldest !== undefined) named.delete(oldest);
    }
  }
  return p;
}

// --- code highlighting ----------------------------------------------------------------

/** Past this a fence paints as plain text: highlighting it would stall. */
const HIGHLIGHT_MAX = 100_000;
let highlightStyled = false;

/** The highlight style's class rules, installed once for the page (the
 *  editors mount the same module; a duplicate rule set is harmless). */
function ensureHighlightStyle(): void {
  if (highlightStyled) return;
  highlightStyled = true;
  const rules = codeHighlight.module?.getRules();
  if (rules === undefined || rules === "") return;
  const style = document.createElement("style");
  style.dataset.mdHighlight = "";
  style.textContent = rules;
  document.head.append(style);
}

function paint(code: HTMLElement, parser: Parser, text: string): void {
  let tree: Tree;
  try {
    tree = parser.parse(text);
  } catch {
    return; // a grammar that chokes leaves the plain text
  }
  ensureHighlightStyle();
  const frag = document.createDocumentFragment();
  highlightCode(
    text,
    tree,
    codeHighlight,
    (t, classes) => {
      if (classes === "") {
        frag.append(t);
      } else {
        const span = document.createElement("span");
        span.className = classes;
        span.textContent = t;
        frag.append(span);
      }
    },
    () => frag.append("\n"),
  );
  code.replaceChildren(frag);
}

interface Fence {
  code: HTMLElement;
  lang: string;
  text: string;
}

/** Paints `fence` if its grammar is loaded; otherwise loads it (once for
 *  the page) and hands the fence to `later` — so a first document's fences
 *  go back through the slices rather than all painting in the load's task. */
function highlight(fence: Fence, later: (fence: Fence) => void): void {
  const { code, lang, text } = fence;
  if (text.length > HIGHLIGHT_MAX || !code.isConnected) return;
  const desc = LanguageDescription.matchLanguageName(languages, lang, true);
  if (desc === null) return;
  if (desc.support !== undefined) {
    paint(code, desc.support.language.parser, text);
    return;
  }
  desc.load().then(
    () => later(fence),
    () => {
      // grammar failed to load: plain text is fine
    },
  );
}

// --- math -----------------------------------------------------------------------------

/** Past this a "source" is not an equation but a document (an unclosed
 *  ```math fence runs to the end of the file): it stays readable text. */
const MAX_MATH_SOURCE = 16 * 1024;

type MathModule = typeof import("../../shared/math");

function typesetSpan(span: HTMLElement, math: MathModule): void {
  if (span.classList.contains("md-math")) return;
  const display = span.dataset.mathStyle === "display";
  const source = span.textContent ?? "";
  span.classList.add("md-math");
  // `$$ $$` has nothing to typeset, as in live.
  if (source.trim().length === 0 || source.length > MAX_MATH_SOURCE) return;
  if (display) span.classList.add("md-math-display");
  span.innerHTML = math.safeMathHtml(source, display);
}

/**
 * Work over a document's blocks, time-sliced: the first 8 ms synchronously
 * (a document's first screen, an edited block), the rest at idle, so notes
 * with thousands of equations or fences never stall the workbench. One
 * queue: items added while a pass is pending join it, in order. Stopping
 * is final (the view is going away).
 */
class Slicer<T> {
  private queue: T[] = [];
  private next = 0;
  private handle: number | null = null;
  private idle = false;
  private stopped = false;

  constructor(
    private readonly run: (item: T) => void,
    private readonly onSlice: () => void = () => {},
  ) {}

  add(items: readonly T[]): void {
    if (items.length === 0 || this.stopped) return;
    // A loop, not a spread: a spread of tens of thousands of items throws.
    for (const item of items) this.queue.push(item);
    if (this.handle === null) this.slice();
  }

  stop(): void {
    this.stopped = true;
    if (this.handle !== null) {
      if (this.idle) cancelIdleCallback(this.handle);
      else clearTimeout(this.handle);
    }
    this.handle = null;
    this.queue = [];
    this.next = 0;
  }

  private slice(): void {
    this.handle = null;
    const deadline = performance.now() + 8;
    while (this.next < this.queue.length && performance.now() < deadline) this.run(this.queue[this.next++]);
    this.onSlice();
    if (this.next >= this.queue.length) {
      this.queue = [];
      this.next = 0;
      return;
    }
    // WKWebView (the native app) has no requestIdleCallback: a short
    // timeout stands in.
    if (typeof requestIdleCallback === "function") {
      this.idle = true;
      this.handle = requestIdleCallback(() => this.slice(), { timeout: 500 });
    } else {
      this.idle = false;
      this.handle = window.setTimeout(() => this.slice(), 16);
    }
  }
}

/**
 * Typesets equations (`span[data-math-style]`, the one seam every equation
 * arrives through — the server fallback's too) under the shared KaTeX
 * policy, loaded on demand, in slices. A span that left the page before its
 * turn is skipped.
 */
export class MathTypesetter {
  private pending: HTMLElement[] = [];
  private slicer: Slicer<HTMLElement> | null = null;
  private loading = false;
  private stopped = false;

  constructor(private readonly onSlice: () => void) {}

  add(spans: readonly HTMLElement[]): void {
    if (spans.length === 0 || this.stopped) return;
    if (this.slicer !== null) {
      this.slicer.add(spans);
      return;
    }
    for (const s of spans) this.pending.push(s);
    if (this.loading) return;
    const math = mathNow();
    if (math !== null) {
      this.start(math);
      return;
    }
    this.loading = true;
    loadMath().then(
      (m) => {
        this.loading = false;
        if (!this.stopped) this.start(m);
      },
      () => {
        // KaTeX failed to load: the LaTeX literals stay readable as text.
        this.loading = false;
        this.pending = [];
      },
    );
  }

  stop(): void {
    this.stopped = true;
    this.slicer?.stop();
    this.pending = [];
  }

  private start(math: MathModule): void {
    this.slicer = new Slicer((s) => {
      if (s.isConnected) typesetSpan(s, math);
    }, this.onSlice);
    const spans = this.pending;
    this.pending = [];
    this.slicer.add(spans);
  }
}

/** The untypeset equations in `nodes` (and the nodes themselves). */
export function mathSpans(nodes: readonly Node[]): HTMLElement[] {
  const out: HTMLElement[] = [];
  for (const n of nodes) {
    if (!(n instanceof HTMLElement)) continue;
    if (n.matches("span[data-math-style]:not(.md-math)")) out.push(n);
    for (const s of n.querySelectorAll<HTMLElement>("span[data-math-style]:not(.md-math)")) out.push(s);
  }
  return out;
}

// --- the reader -----------------------------------------------------------------------

/** `L:C-L:C` with its lines moved by `delta`. */
function shiftPos(pos: string, delta: number): string {
  const m = /^(\d+):(\d+)-(\d+):(\d+)$/.exec(pos);
  return m === null ? pos : `${Number(m[1]) + delta}:${m[2]}-${Number(m[3]) + delta}:${m[4]}`;
}

function shiftSourcepos(nodes: readonly Node[], delta: number): void {
  for (const n of nodes) {
    if (!(n instanceof Element)) continue;
    const pos = n.getAttribute("data-sourcepos");
    if (pos !== null) n.setAttribute("data-sourcepos", shiftPos(pos, delta));
    for (const el of n.querySelectorAll("[data-sourcepos]"))
      el.setAttribute("data-sourcepos", shiftPos(el.getAttribute("data-sourcepos") ?? "", delta));
  }
}

const mermaidSource = new WeakMap<HTMLElement, string>();

export interface HydratorOptions {
  /** The document's absolute path: relative images resolve beside it. */
  docPath: string;
  /** Read when used: the workspace an `![[embed]]` resolves in. */
  links: () => LinkContext;
  theme: "light" | "dark";
  /** Drawn content changed size after the fact (an equation typeset, a
   *  diagram laid out). */
  onLayout?: () => void;
}

/**
 * What only a page can do for rendered blocks, shared by the reading
 * article and the live editor's block widgets so the two draw a block
 * identically: the DOM target with its hooks — images beside the document
 * (tickets, `![[name]]`), fences through the lezer highlighter, mermaid per
 * theme — and the time-sliced passes that paint fences and typeset
 * equations once the nodes are in the page.
 *
 * The image hook is where an image becomes something richer: returning a
 * node from `DomHooks.image` puts it in the image's place in both views.
 */
export class Hydrator {
  readonly target: DomTarget;
  private theme: "light" | "dark";
  private readonly math: MathTypesetter;
  /** Fences drawn since the last `settle`: painted once they are in the
   *  page, in slices (a warm grammar would otherwise paint every fence of a
   *  first render synchronously). */
  private fences: Fence[] = [];
  /** Fences whose grammar just loaded, requeued as one batch: a grammar's
   *  load settles every fence waiting on it in the same microtask run. */
  private loaded: Fence[] = [];
  private readonly highlighter = new Slicer<Fence>((f) =>
    highlight(f, (g) => {
      if (this.loaded.push(g) === 1) queueMicrotask(() => this.highlighter.add(this.loaded.splice(0)));
    }),
  );

  constructor(private readonly opts: HydratorOptions) {
    this.theme = opts.theme;
    const hooks: DomHooks = {
      image: (img, src, wikilink) => this.image(img, src, wikilink),
      code: (code, lang, text) => {
        this.fences.push({ code, lang, text });
      },
      mermaid: (box, source) => {
        mermaidSource.set(box, source);
        this.drawMermaid(box, source);
      },
    };
    this.target = new DomTarget(hooks);
    this.math = new MathTypesetter(() => this.opts.onLayout?.());
  }

  /** `fresh` just went into the page: paint the fences drawn since the
   *  last call and typeset the equations in it. */
  settle(fresh: readonly Node[]): void {
    this.highlighter.add(this.fences.splice(0));
    this.math.add(mathSpans(fresh));
  }

  /** Re-lay out every diagram under `root` for a theme change. */
  setTheme(theme: "light" | "dark", root: ParentNode): void {
    if (theme === this.theme) return;
    this.theme = theme;
    for (const box of root.querySelectorAll<HTMLElement>(".md-mermaid")) {
      const source = mermaidSource.get(box);
      if (source !== undefined) this.drawMermaid(box, source);
    }
  }

  destroy(): void {
    this.math.stop();
    this.highlighter.stop();
    this.fences = [];
    this.loaded = [];
  }

  private image(img: HTMLImageElement, src: string, wikilink: string | null): void {
    const { docPath } = this.opts;
    if (wikilink !== null) {
      img.dataset.mdSrc = wikilink;
      void resolveNamed(wikilink, dirname(docPath), this.opts.links().workspaceId).then((path) => {
        if (path !== null && img.dataset.mdSrc === wikilink) pointAt(img, path, wikilink);
      });
      return;
    }
    if (/^([a-z][a-z0-9+.-]*:|\/\/)/i.test(src)) {
      img.src = src;
      return;
    }
    img.dataset.mdSrc = src;
    pointAt(img, resolveDocPath(docPath, safeDecodeUri(src)), src);
  }

  private drawMermaid(box: HTMLElement, source: string): void {
    const theme = this.theme;
    box.dataset.theme = theme;
    renderMermaid(source, theme).then(
      (svg) => {
        if (box.dataset.theme !== theme) return; // a newer theme's render owns it
        const holder = document.createElement("div");
        holder.className = "md-mermaid-svg";
        holder.innerHTML = svg; // sanitized by shared/mermaid (strict mode + DOMPurify)
        box.classList.remove("md-mermaid-error");
        box.replaceChildren(holder);
        this.opts.onLayout?.();
      },
      (err: unknown) => {
        if (box.dataset.theme !== theme) return;
        const note = document.createElement("p");
        note.className = "md-mermaid-note";
        note.textContent = `diagram error: ${err instanceof Error ? err.message : String(err)}`;
        const pre = document.createElement("pre");
        const code = document.createElement("code");
        code.textContent = source;
        pre.append(code);
        box.classList.add("md-mermaid-error");
        box.replaceChildren(note, pre);
        this.opts.onLayout?.();
      },
    );
  }
}

/** Block elements a done task's text stops at (its nested blocks keep
 *  their own ink). */
const TASK_BLOCK_TAGS = new Set(["UL", "OL", "P", "DIV", "PRE", "BLOCKQUOTE", "TABLE"]);

/**
 * Task items (`- [x]`) arrive as `span.md-task[data-task]` at the head of
 * their item. The item drops its bullet (the box stands in for it), and a
 * done item's own text — up to its first nested block, so a sub-list keeps
 * its ink — is wrapped to read muted and struck through. A CSS `:has()`
 * would do the first half, but its invalidation cost on a long document in
 * WebKit is exactly the restyle churn the pane parking fights; a class set
 * once is free. Idempotent: a fresh render brings fresh spans. Both views
 * run it on what they draw (the server fallback too).
 */
export function markTasks(root: ParentNode): void {
  for (const box of root.querySelectorAll<HTMLElement>("span.md-task[data-task]")) {
    const item = box.closest("li");
    if (item !== null && !item.classList.contains("md-task-item")) {
      item.classList.add("md-task-item");
    }
    if (box.dataset.task !== "done") continue;
    if (box.nextElementSibling?.classList.contains("md-task-text")) continue;
    const wrap = document.createElement("span");
    wrap.className = "md-task-text";
    let n = box.nextSibling;
    while (n !== null && !(n instanceof HTMLElement && TASK_BLOCK_TAGS.has(n.tagName))) {
      const next = n.nextSibling;
      wrap.append(n);
      n = next;
    }
    // The space after the box stays outside, or the strike starts on it.
    const head = wrap.firstChild;
    const lead = head instanceof Text ? /^\s+/.exec(head.data) : null;
    if (head instanceof Text && lead !== null) head.data = head.data.slice(lead[0].length);
    box.after(wrap);
    if (lead !== null) box.after(document.createTextNode(lead[0]));
  }
}

export class DocReader {
  private units: Unit[] = [];
  private prev: { text: string; tree: Tree } | null = null;
  private readonly hydrator: Hydrator;

  constructor(
    private readonly root: HTMLElement,
    opts: ReaderOptions,
  ) {
    this.hydrator = new Hydrator(opts);
  }

  /** Bring the article in step with `source` (the document's full text). */
  update(source: string): ReadResult {
    const { text, frontmatter } = bodyText(source);
    const cx = documentContext(text, this.prev);
    this.prev = { text, tree: cx.tree };
    const env = { t: this.hydrator.target, cx };

    const pool = new Map<string, Unit[]>();
    for (const u of this.units) {
      const list = pool.get(u.key);
      if (list === undefined) pool.set(u.key, [u]);
      else list.push(u);
    }
    const deps = depsOf(cx);
    const next: Unit[] = [];
    const fresh: Node[] = [];
    const place = (key: string, line: number, draw: (into: DocumentFragment) => void): void => {
      const reuse = pool.get(key)?.shift();
      if (reuse !== undefined) {
        if (reuse.line !== line) shiftSourcepos(reuse.nodes, line - reuse.line);
        next.push({ key, nodes: reuse.nodes, line });
        return;
      }
      const frag = document.createDocumentFragment();
      draw(frag);
      const nodes = Array.from(frag.childNodes);
      fresh.push(...nodes);
      next.push({ key, nodes, line });
    };
    for (const run of htmlRuns(topLevel(cx), (n) => htmlOfNode(n, cx))) {
      const from = run[0].from;
      const to = run[run.length - 1].to;
      const line = cx.lines.lineOf(from);
      const src = text.slice(cx.lines.lineStart(line), to);
      const key = `${run.map((n) => n.name).join(",")}\u0000${src}\u0000${deps(from, to, src)}`;
      place(key, line, (into) => renderRun(into, run, env));
    }
    // The footnote section: one unit, drawn from definitions that may sit
    // anywhere in the document — so it keys on each one's line too (a shift
    // between two of them can't be applied to the unit as a whole).
    const notes = footnotesOf(cx);
    if (notes.length > 0) {
      const key =
        "\u0001footnotes" +
        notes
          .map((f) => {
            const src = text.slice(f.from, f.to);
            const at = cx.lines.lineOf(f.from);
            return `\u0000${f.label}\u0000${f.n}\u0000${f.refs}\u0000${at}\u0000${src}\u0000${deps(f.from, f.to, src)}`;
          })
          .join("\u0001");
      place(key, cx.lines.lineOf(notes[0].from), (into) => renderFootnotes(into, notes, env));
    }

    // Drop what no longer renders, then put every kept and fresh node in
    // document order, touching only the nodes that moved.
    for (const units of pool.values()) for (const u of units) for (const n of u.nodes) n.parentNode?.removeChild(n);
    let cursor: ChildNode | null = this.root.firstChild;
    for (const u of next)
      for (const n of u.nodes) {
        if (n === cursor) cursor = cursor.nextSibling;
        else this.root.insertBefore(n, cursor);
      }
    while (cursor !== null) {
      const after: ChildNode | null = cursor.nextSibling;
      this.root.removeChild(cursor);
      cursor = after;
    }
    this.units = next;

    this.hydrator.settle(fresh);
    return {
      frontmatter: frontmatter?.raw ?? null,
      outline: cx.outline.map((h) => ({ ...h, line: cx.lines.lineOf(h.from) })),
      fresh,
    };
  }

  /** Re-lay out every diagram for a theme change. */
  setTheme(theme: "light" | "dark"): void {
    this.hydrator.setTheme(theme, this.root);
  }

  destroy(): void {
    this.hydrator.destroy();
    this.units = [];
    this.prev = null;
  }
}
