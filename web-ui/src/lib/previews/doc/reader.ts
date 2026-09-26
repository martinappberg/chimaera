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
import { dirname, fsValidate, rawTicketUrl, resolveDocPath, safeDecodeUri } from "../files";
import { loadMath, mathNow } from "../mathLoad";
import { renderMermaid } from "../../shared/mermaid";
import type { LinkContext } from "../docLinks";
import {
  bodyText,
  htmlOfNode,
  htmlRuns,
  documentContext,
  footnotesOf,
  type DocContext,
  type OutlineEntry,
} from "./model";
import { DomTarget, renderFootnotes, renderRun, topLevel, type DomHooks } from "./render";

export interface ReaderOptions {
  /** The document's absolute path: relative images resolve beside it. */
  docPath: string;
  /** Read when used: the workspace an `![[embed]]` resolves in. */
  links: () => LinkContext;
  theme: "light" | "dark";
  /** After equations typeset (a new wide one may need a scroll region). */
  onLayout?: () => void;
}

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

/** Ticket URLs by path, read synchronously so a block re-rendered for an
 *  edit next to its image keeps the same src (no request, no flash).
 *  Tickets live ~10 min on the daemon; entries retire before that. */
const TICKET_MS = 8 * 60 * 1000;
const tickets = new Map<string, { url: string; at: number }>();
const TICKETS_MAX = 256;

function ticketNow(path: string): string | null {
  const hit = tickets.get(path);
  return hit !== undefined && Date.now() - hit.at < TICKET_MS ? hit.url : null;
}

function rememberTicket(path: string, url: string): void {
  tickets.delete(path);
  tickets.set(path, { url, at: Date.now() });
  if (tickets.size > TICKETS_MAX) {
    const oldest = tickets.keys().next().value;
    if (oldest !== undefined) tickets.delete(oldest);
  }
}

function pointAt(img: HTMLImageElement, path: string, stamp: string): void {
  const now = ticketNow(path);
  if (now !== null) {
    img.src = now;
    return;
  }
  rawTicketUrl(path).then(
    (url) => {
      rememberTicket(path, url);
      if (img.dataset.mdSrc === stamp) img.src = url;
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
    if (named.size > TICKETS_MAX) {
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

function highlight(code: HTMLElement, lang: string, text: string): void {
  if (text.length > HIGHLIGHT_MAX) return;
  const desc = LanguageDescription.matchLanguageName(languages, lang, true);
  if (desc === null) return;
  if (desc.support !== undefined) {
    paint(code, desc.support.language.parser, text);
    return;
  }
  // The grammar loads once for the page; only this block repaints — and
  // only if it still holds the text it was drawn with.
  desc.load().then(
    (support) => {
      if (code.textContent === text) paint(code, support.language.parser, text);
    },
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
 * Typesets equations (`span[data-math-style]`, the one seam every equation
 * arrives through — the server fallback's too) under the shared KaTeX
 * policy, loaded on demand. Time-sliced: the first 8 ms synchronously (a
 * document's first screen, an edited block), the rest at idle, so lecture
 * notes with thousands of equations never stall the workbench. One queue
 * per view: spans added while a pass runs join it; a span that left the
 * page before its turn is skipped.
 */
export class MathTypesetter {
  private queue: HTMLElement[] = [];
  private next = 0;
  private handle: number | null = null;
  private idle = false;
  private loading = false;
  private stopped = false;

  constructor(private readonly onSlice: () => void) {}

  add(spans: readonly HTMLElement[]): void {
    if (spans.length === 0 || this.stopped) return;
    this.queue.push(...spans);
    if (this.handle !== null || this.loading) return;
    const math = mathNow();
    if (math !== null) {
      this.slice(math);
      return;
    }
    this.loading = true;
    loadMath().then(
      (m) => {
        this.loading = false;
        if (!this.stopped) this.slice(m);
      },
      () => {
        // KaTeX failed to load: the LaTeX literals stay readable as text.
        this.loading = false;
        this.queue = [];
        this.next = 0;
      },
    );
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

  private slice(math: MathModule): void {
    this.handle = null;
    const deadline = performance.now() + 8;
    while (this.next < this.queue.length && performance.now() < deadline) {
      const s = this.queue[this.next++];
      if (s.isConnected) typesetSpan(s, math);
    }
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
      this.handle = requestIdleCallback(() => this.slice(math), { timeout: 500 });
    } else {
      this.idle = false;
      this.handle = window.setTimeout(() => this.slice(math), 16);
    }
  }
}

/** The untypeset equations in `nodes` (and the nodes themselves). */
export function mathSpans(nodes: readonly Node[]): HTMLElement[] {
  const out: HTMLElement[] = [];
  for (const n of nodes) {
    if (!(n instanceof HTMLElement)) continue;
    if (n.matches("span[data-math-style]:not(.md-math)")) out.push(n);
    out.push(...n.querySelectorAll<HTMLElement>("span[data-math-style]:not(.md-math)"));
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

export class DocReader {
  private units: Unit[] = [];
  private prev: { text: string; tree: Tree } | null = null;
  private readonly target: DomTarget;
  private theme: "light" | "dark";
  private readonly math: MathTypesetter;

  constructor(
    private readonly root: HTMLElement,
    private readonly opts: ReaderOptions,
  ) {
    this.theme = opts.theme;
    const hooks: DomHooks = {
      image: (img, src, wikilink) => this.image(img, src, wikilink),
      code: (code, lang, text) => highlight(code, lang, text),
      mermaid: (box, source) => {
        mermaidSource.set(box, source);
        this.drawMermaid(box, source);
      },
    };
    this.target = new DomTarget(hooks);
    this.math = new MathTypesetter(() => this.opts.onLayout?.());
  }

  /** Bring the article in step with `source` (the document's full text). */
  update(source: string): ReadResult {
    const { text, frontmatter } = bodyText(source);
    const cx = documentContext(text, this.prev);
    this.prev = { text, tree: cx.tree };
    const env = { t: this.target, cx };

    const pool = new Map<string, Unit[]>();
    for (const u of this.units) {
      const list = pool.get(u.key);
      if (list === undefined) pool.set(u.key, [u]);
      else list.push(u);
    }
    const deps = this.depsFor(cx);
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

    this.math.add(mathSpans(fresh));
    return {
      frontmatter: frontmatter?.raw ?? null,
      outline: cx.outline.map((h) => ({ ...h, line: cx.lines.lineOf(h.from) })),
      fresh,
    };
  }

  /** Re-lay out every diagram for a theme change. */
  setTheme(theme: "light" | "dark"): void {
    if (theme === this.theme) return;
    this.theme = theme;
    for (const box of this.root.querySelectorAll<HTMLElement>(".md-mermaid")) {
      const source = mermaidSource.get(box);
      if (source !== undefined) this.drawMermaid(box, source);
    }
  }

  destroy(): void {
    this.math.stop();
    this.units = [];
    this.prev = null;
  }

  /**
   * What a stretch of the document's rendering reads beyond its own text:
   * the ids of the headings in it, the numbers of the footnote references
   * in it, and — when it could hold a reference link — every definition.
   */
  private depsFor(cx: DocContext): (from: number, to: number, src: string) => string {
    const headings = [...cx.headingIds.entries()].sort((a, b) => a[0] - b[0]);
    const refs = [...cx.footnoteRefs.entries()].sort((a, b) => a[0] - b[0]);
    const defs =
      cx.refs.size === 0
        ? ""
        : [...cx.refs.entries()].map(([k, d]) => `${k}\u0000${d.url}\u0000${d.title ?? ""}`).join("\u0001");
    /** The first entry at or after `pos`. */
    const lowerBound = (list: readonly [number, unknown][], pos: number): number => {
      let lo = 0;
      let hi = list.length;
      while (lo < hi) {
        const mid = (lo + hi) >> 1;
        if (list[mid][0] < pos) lo = mid + 1;
        else hi = mid;
      }
      return lo;
    };
    return (from, to, src) => {
      let out = "";
      for (let k = lowerBound(headings, from); k < headings.length && headings[k][0] < to; k++)
        out += `#${headings[k][1]}`;
      for (let k = lowerBound(refs, from); k < refs.length && refs[k][0] < to; k++) {
        const f = refs[k][1];
        out += `^${f.label}:${f.n}:${f.nth}`;
      }
      if (defs !== "" && src.includes("[")) out += `\u0001${defs}`;
      return out;
    };
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
      },
    );
  }
}


