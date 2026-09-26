/**
 * The markdown renderer: the document model (`model.ts`) drawn through one
 * small builder interface with two targets —
 *
 * - `DomTarget`, the reading view: real elements via createElement and
 *   textContent. The only markup ever parsed is raw HTML the document
 *   carries, and it goes through DOMPurify (chat's policy, widened to what
 *   the daemon's ammonia allows: `sanitizeHtml` below); equations and
 *   diagrams go through their own sanitized helpers later (the reader
 *   hydrates them), code through the lezer highlighter.
 * - `HtmlTarget`, an HTML string for the parity corpus (Vitest has no DOM).
 *   It draws the same tree in its un-hydrated state — an equation as its
 *   `span[data-math-style]` literal, a diagram as its fence — which is also
 *   exactly the daemon's markup. Raw HTML passes through it unsanitized:
 *   the string never reaches a page.
 *
 * The markup matches the daemon's (`fs.rs markdown_to_html` + ammonia), so
 * the view's CSS and its link, anchor, task and line-mapping logic work on
 * either: `data-sourcepos` on every block element (1-based lines of the
 * original file), `user-content-` heading and footnote ids, GitHub alert
 * cards, `span.md-task` boxes, comrak's footnote section. Hrefs are
 * escaped the way comrak escapes them, and a URL whose scheme ammonia would
 * strip loses its href (or src) here too.
 */
import DOMPurify from "dompurify";
import type { SyntaxNode } from "@lezer/common";
import type { Inline } from "../mdTable";
import { anchorIds } from "../mdDoc";
import {
  bodyText,
  blockOf,
  documentContext,
  footnotesOf,
  githubSlug,
  htmlOfNode,
  htmlRuns,
  type Block,
  type DocContext,
  type Footnote,
} from "./model";

export type Attrs = Record<string, string>;

/** What a renderer needs from an output. */
export interface Target<N> {
  el(tag: string, attrs?: Attrs): N;
  text(text: string): N;
  add(parent: N, child: N): void;
  attr(node: N, name: string, value: string): void;
  /** The document's own raw HTML (a block, or an inline run holding tags),
   *  appended to `parent` — sanitized by a target that reaches a page. */
  raw(parent: N, html: string, sourcepos?: string): void;
  /** Constructs a target draws its own way. */
  math(source: string, display: boolean): N;
  code(lang: string, text: string): N;
  mermaid(source: string): N;
  /** `src` is already escaped and scheme-checked ("" = none); `wikilink`
   *  names an `![[embed]]`'s target, resolved by name. */
  image(src: string, alt: string, title: string | null, wikilink: string | null): N;
}

// --- URLs ---------------------------------------------------------------------

/** The schemes ammonia (the daemon's sanitizer) keeps; anything else loses
 *  its href/src. A scheme-less URL is relative and passes. */
const URL_SCHEMES = new Set([
  "bitcoin", "ftp", "ftps", "geo", "http", "https", "im", "irc", "ircs", "magnet", "mailto",
  "mms", "mx", "news", "nntp", "openpgp4fpr", "sip", "sms", "smsto", "ssh", "tel", "url",
  "webcal", "wtai", "xmpp",
]);

export function urlAllowed(url: string): boolean {
  const m = /^([a-z][a-z0-9+.-]*):/i.exec(url.trim());
  return m === null || URL_SCHEMES.has(m[1].toLowerCase());
}

const HREF_SAFE = /[A-Za-z0-9\-_.+!*(),#@?=;:/$~]/;
const utf8 = new TextEncoder();

/** comrak's `escape_href`: every byte outside its safe set percent-encoded,
 *  an existing `%XX` kept, a stray `%` as `%25`. */
export function escapeHref(url: string): string {
  let out = "";
  for (let i = 0; i < url.length; i++) {
    const ch = url[i];
    if (HREF_SAFE.test(ch) || ch === "&" || ch === "'") {
      out += ch;
    } else if (ch === "%") {
      out += /^[0-9a-fA-F]{2}$/.test(url.slice(i + 1, i + 3)) ? "%" : "%25";
    } else {
      const cp = url.codePointAt(i) ?? 0;
      const s = String.fromCodePoint(cp);
      if (s.length === 2) i++;
      for (const b of utf8.encode(cp === 0 ? "�" : s)) out += `%${b.toString(16).toUpperCase().padStart(2, "0")}`;
    }
  }
  return out;
}

/** A link destination as an href: escaped, or null when its scheme is one
 *  the daemon's sanitizer strips. */
export function hrefFor(url: string): string | null {
  return urlAllowed(url) ? escapeHref(url) : null;
}

const IMAGE_EXT = /\.(png|jpe?g|gif|webp|svg|bmp|ico|avif)$/i;

/** Where a wikilink points: the target file (`.md` added to an
 *  extension-less name, as Obsidian reads `[[note]]`) plus the heading's
 *  GitHub slug — an ordinary document link from here on (`docLinks.ts`
 *  resolves it against the document's folder, then by name). */
export function wikilinkHref(target: string, heading: string | null): string {
  const fragment = heading === null ? "" : `#${githubSlug(heading)}`;
  if (target === "") return fragment === "" ? "#" : fragment;
  const hasExt = /\.[A-Za-z0-9]*[A-Za-z][A-Za-z0-9]*$/.test(target.split("/").pop() ?? "");
  return escapeHref(hasExt ? target : `${target}.md`) + fragment;
}

export function isImageTarget(target: string): boolean {
  return IMAGE_EXT.test(target);
}

// --- rendering ------------------------------------------------------------------

export interface Env<N> {
  t: Target<N>;
  cx: DocContext;
}

function pos<N>(env: Env<N>, from: number, to: number, attrs: Attrs = {}): Attrs {
  attrs["data-sourcepos"] = env.cx.lines.sourcepos(from, to);
  return attrs;
}

/** Tags turned into task boxes before any sanitizer sees raw HTML (the
 *  daemon's `tasks_to_spans`): a checkbox is a `span.md-task`, never a
 *  form control. */
export function tasksToSpans(html: string): string {
  if (!/<input/i.test(html)) return html;
  return html.replace(/<input\b[^>]*>/gi, (tag) => {
    if (!/\stype\s*=\s*["']?checkbox\b/i.test(tag)) return tag;
    const done = /\schecked\b/i.test(tag);
    return `<span class="md-task" data-task="${done ? "done" : "todo"}"></span>`;
  });
}

/** An inline run to append. A run holding raw HTML tags is drawn to a
 *  string first and handed to the target whole — a tag only means
 *  something with its partner and whatever sits between them. */
function renderInline<N>(parent: N, inline: readonly Inline[], env: Env<N>): void {
  if (inline.some((i) => i.kind === "html")) {
    const html = new HtmlTarget();
    const box = html.el("div");
    renderInlineNodes(box, inline, { t: html, cx: env.cx });
    env.t.raw(parent, html.serializeChildren(box));
    return;
  }
  renderInlineNodes(parent, inline, env);
}

function renderInlineNodes<N>(parent: N, inline: readonly Inline[], env: Env<N>): void {
  const { t } = env;
  for (const i of inline) {
    switch (i.kind) {
      case "text":
        t.add(parent, t.text(i.text));
        break;
      case "strong":
      case "em":
      case "code": {
        const el = t.el(i.kind);
        renderInlineNodes(el, i.children, env);
        t.add(parent, el);
        break;
      }
      case "strike": {
        const el = t.el("del");
        renderInlineNodes(el, i.children, env);
        t.add(parent, el);
        break;
      }
      case "link": {
        const href = hrefFor(i.url);
        const attrs: Attrs = {};
        if (href !== null) attrs.href = href;
        if (i.title !== null) attrs.title = i.title;
        if (href !== null && /^https?:/i.test(href)) {
          attrs.target = "_blank";
          attrs.rel = "noopener noreferrer";
        }
        const a = t.el("a", attrs);
        renderInlineNodes(a, i.children, env);
        t.add(parent, a);
        break;
      }
      case "image":
        t.add(parent, t.image(hrefFor(i.url) ?? "", i.alt, i.title, null));
        break;
      case "math":
        t.add(parent, t.math(i.source, i.display));
        break;
      case "break":
        t.add(parent, t.el("br"));
        t.add(parent, t.text("\n"));
        break;
      case "html":
        // Only ever drawn into the string a run with tags is built as
        // (renderInline), which the target then sanitizes whole.
        t.raw(parent, i.source);
        break;
      case "footnote": {
        const ref = env.cx.footnoteRefs.get(i.at);
        if (ref === undefined) {
          t.add(parent, t.text(i.source));
          break;
        }
        const name = escapeHref(ref.label);
        const sup = t.el("sup", { class: "footnote-ref" });
        const a = t.el("a", {
          href: `#fn-${name}`,
          id: `user-content-fnref-${name}${ref.nth > 1 ? `-${ref.nth}` : ""}`,
        });
        t.add(a, t.text(String(ref.n)));
        t.add(sup, a);
        t.add(parent, sup);
        break;
      }
      case "wikilink": {
        const shown = i.alias ?? i.source.replace(/^!?\[\[|\]\]$/g, "");
        // A target that reads as a URL scheme gets no href: a name is a
        // file, never `javascript:`.
        const href = wikilinkHref(i.target, i.heading);
        if (i.embed && isImageTarget(i.target)) {
          const src = urlAllowed(i.target) ? escapeHref(i.target) : "";
          t.add(parent, t.image(src, i.alias ?? i.target, null, src === "" ? null : i.target));
          break;
        }
        const attrs: Attrs = {
          class: i.embed ? "wikilink wikilink-embed" : "wikilink",
          "data-wikilink": i.target,
        };
        if (urlAllowed(href) && !/^[a-z][a-z0-9+.-]*:/i.test(href)) attrs.href = href;
        const a = t.el("a", attrs);
        t.add(a, t.text(shown));
        t.add(parent, a);
        break;
      }
    }
  }
}

/** Append sibling blocks, an HTML-opened run (`htmlRuns`) as one island. */
export function renderBlocks<N>(parent: N, blocks: readonly Block[], env: Env<N>, tight = false): void {
  for (const run of htmlRuns(blocks, (b) => (b.kind === "html" ? b.source : null))) {
    if (run.length === 1) renderBlock(parent, run[0], env, tight);
    else renderHtmlRun(parent, run, env);
  }
}

/** A run an HTML block opened, drawn as one stretch of HTML — its raw
 *  pieces as written, the markdown between them as this renderer's markup
 *  — so the target sanitizes (and nests) it whole, as the daemon does. */
export function renderHtmlRun<N>(parent: N, run: readonly Block[], env: Env<N>): void {
  const html = new HtmlTarget();
  const box = html.el("div");
  const inner: Env<HNode> = { t: html, cx: env.cx };
  for (const b of run) renderBlock(box, b, inner);
  env.t.raw(parent, html.serializeChildren(box), env.cx.lines.sourcepos(run[0].from, run[run.length - 1].to));
}

/** Append `b` to `parent`. `tight` is its list's: a tight item's
 *  paragraphs are bare text, as comrak renders them. */
export function renderBlock<N>(parent: N, b: Block, env: Env<N>, tight = false): void {
  const { t } = env;
  switch (b.kind) {
    case "paragraph": {
      if (tight) {
        renderInline(parent, b.inline, env);
        return;
      }
      const p = t.el("p", pos(env, b.from, b.to));
      renderInline(p, b.inline, env);
      t.add(parent, p);
      return;
    }
    case "heading": {
      const h = t.el(`h${b.level}`, pos(env, b.from, b.to, { id: `user-content-${b.id}` }));
      renderInline(h, b.inline, env);
      // The daemon's empty self-link; kept out of the tab order and the
      // accessibility tree (it has no text).
      t.add(h, t.el("a", { href: `#${b.id}`, class: "anchor", "aria-hidden": "true", tabindex: "-1" }));
      t.add(parent, h);
      return;
    }
    case "rule":
      t.add(parent, t.el("hr", pos(env, b.from, b.to)));
      return;
    case "quote": {
      const q = t.el("blockquote", pos(env, b.from, b.to));
      renderBlocks(q, b.children, env);
      t.add(parent, q);
      return;
    }
    case "alert": {
      const d = t.el("div", pos(env, b.from, b.to, { class: `markdown-alert markdown-alert-${b.type}` }));
      const title = t.el("p", { class: "markdown-alert-title" });
      t.add(title, t.text(b.title));
      t.add(d, title);
      renderBlocks(d, b.children, env);
      t.add(parent, d);
      return;
    }
    case "list": {
      const attrs = pos(env, b.from, b.to);
      if (b.items.some((i) => i.task !== null)) attrs.class = "contains-task-list";
      if (b.ordered && b.start !== 1) attrs.start = String(b.start);
      const list = t.el(b.ordered ? "ol" : "ul", attrs);
      for (const item of b.items) {
        const li = t.el("li", pos(env, item.from, item.to));
        if (item.task !== null) {
          t.attr(li, "class", "task-list-item");
          t.add(li, t.el("span", { class: "md-task", "data-task": item.task }));
          if (b.tight && item.children.length > 0) t.add(li, t.text(" "));
        }
        renderBlocks(li, item.children, env, b.tight);
        t.add(list, li);
      }
      t.add(parent, list);
      return;
    }
    case "code": {
      const lang = b.lang.split(/\s/)[0] ?? "";
      const node = lang === "mermaid" ? t.mermaid(b.text) : t.code(lang, b.text);
      t.attr(node, "data-sourcepos", env.cx.lines.sourcepos(b.from, b.to));
      t.add(parent, node);
      return;
    }
    case "math": {
      const p = t.el("p", pos(env, b.from, b.to));
      t.add(p, t.math(b.source, true));
      t.add(parent, p);
      return;
    }
    case "table": {
      const table = t.el("table", pos(env, b.from, b.to));
      const { header, rows, align } = b.table;
      const row = (tag: "th" | "td", r: typeof header): N => {
        const rowFrom = r.cells[0]?.from ?? r.end;
        const tr = t.el("tr", pos(env, rowFrom, r.to));
        r.cells.forEach((c, col) => {
          const a = align[col];
          // A cell maps to its row's line (a reveal or a selection lands there).
          const cell = t.el(tag, pos(env, rowFrom, r.to, a === null || a === undefined ? {} : { align: a }));
          renderInline(cell, c.inline, env);
          t.add(tr, cell);
        });
        return tr;
      };
      const thead = t.el("thead");
      t.add(thead, row("th", header));
      t.add(table, thead);
      if (rows.length > 0) {
        const tbody = t.el("tbody");
        for (const r of rows) t.add(tbody, row("td", r));
        t.add(table, tbody);
      }
      t.add(parent, table);
      return;
    }
    case "html":
      t.raw(parent, b.source, env.cx.lines.sourcepos(b.from, b.to));
      return;
  }
}

/** The footnote section comrak appends: each rendered definition with a
 *  back-reference per reference (inside its last paragraph, when it ends
 *  in one). */
export function renderFootnotes<N>(parent: N, notes: readonly Footnote[], env: Env<N>): void {
  if (notes.length === 0) return;
  const { t } = env;
  const section = t.el("section", { class: "footnotes" });
  const ol = t.el("ol");
  for (const f of notes) {
    const name = escapeHref(f.label);
    const li = t.el("li", pos(env, f.from, f.to, { id: `user-content-fn-${name}` }));
    const backrefs = (into: N): void => {
      for (let k = 1; k <= f.refs; k++) {
        if (k > 1) t.add(into, t.text(" "));
        const a = t.el("a", {
          href: `#fnref-${name}${k > 1 ? `-${k}` : ""}`,
          class: "footnote-backref",
          "aria-label": `back to reference ${f.n}${k > 1 ? `-${k}` : ""}`,
        });
        t.add(a, t.text("↩"));
        if (k > 1) {
          const sup = t.el("sup", { class: "footnote-ref" });
          t.add(sup, t.text(String(k)));
          t.add(a, sup);
        }
        t.add(into, a);
      }
    };
    const last = f.children[f.children.length - 1];
    const endsInParagraph = last !== undefined && last.kind === "paragraph";
    renderBlocks(li, endsInParagraph ? f.children.slice(0, -1) : f.children, env);
    if (last !== undefined && last.kind === "paragraph") {
      const p = t.el("p", pos(env, last.from, last.to));
      renderInline(p, last.inline, env);
      t.add(p, t.text(" "));
      backrefs(p);
      t.add(li, p);
    } else {
      backrefs(li);
    }
    t.add(ol, li);
  }
  t.add(section, ol);
  t.add(parent, section);
}

// --- the string target (parity corpus) ---------------------------------------------

type HNode = { raw: string } | { tag: string; attrs: Attrs; children: HNode[] };

const VOID = new Set(["br", "hr", "img", "input", "wbr", "col", "area"]);

export function escapeText(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function escapeAttr(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/"/g, "&quot;");
}

/** HTML text. Raw HTML passes UNSANITIZED — this target feeds the parity
 *  tests, never a page. */
export class HtmlTarget implements Target<HNode> {
  el(tag: string, attrs: Attrs = {}): HNode {
    return { tag, attrs: { ...attrs }, children: [] };
  }
  text(text: string): HNode {
    return { raw: escapeText(text) };
  }
  add(parent: HNode, child: HNode): void {
    if ("children" in parent) parent.children.push(child);
  }
  attr(node: HNode, name: string, value: string): void {
    if ("attrs" in node) node.attrs[name] = value;
  }
  raw(parent: HNode, html: string): void {
    this.add(parent, { raw: tasksToSpans(html) });
  }
  math(source: string, display: boolean): HNode {
    const span = this.el("span", { "data-math-style": display ? "display" : "inline" });
    this.add(span, this.text(source));
    return span;
  }
  code(lang: string, text: string): HNode {
    const pre = this.el("pre");
    const code = this.el("code", lang === "" ? {} : { "data-lang": lang });
    this.add(code, this.text(text));
    this.add(pre, code);
    return pre;
  }
  mermaid(source: string): HNode {
    return this.code("mermaid", source);
  }
  image(src: string, alt: string, title: string | null): HNode {
    const attrs: Attrs = {};
    if (src !== "") attrs.src = src;
    attrs.alt = alt;
    if (title !== null) attrs.title = title;
    return this.el("img", attrs);
  }
  serialize(node: HNode): string {
    if ("raw" in node) return node.raw;
    const attrs = Object.entries(node.attrs)
      .map(([k, v]) => ` ${k}="${escapeAttr(v)}"`)
      .join("");
    if (VOID.has(node.tag)) return `<${node.tag}${attrs}>`;
    return `<${node.tag}${attrs}>${this.serializeChildren(node)}</${node.tag}>`;
  }
  serializeChildren(node: HNode): string {
    return "children" in node ? node.children.map((c) => this.serialize(c)).join("") : "";
  }
}

/** The document's top-level block nodes, in order. */
export function topLevel(cx: DocContext): SyntaxNode[] {
  const out: SyntaxNode[] = [];
  for (let c = cx.tree.topNode.firstChild; c !== null; c = c.nextSibling) out.push(c);
  return out;
}

/** One top-level unit: a block node, or an HTML-opened run of them. */
export function renderRun<N>(parent: N, run: readonly SyntaxNode[], env: Env<N>): void {
  const blocks: Block[] = [];
  for (const n of run) {
    const b = blockOf(n, env.cx);
    if (b !== null) blocks.push(b);
  }
  if (run.length === 1) {
    if (blocks.length === 1) renderBlock(parent, blocks[0], env);
  } else if (blocks.length > 0) {
    renderHtmlRun(parent, blocks, env);
  }
}

/** A whole document as HTML (the parity corpus's client half): its body
 *  and footnotes; frontmatter is set aside, as the reading view's panel
 *  shows it instead. */
export function renderHtml(source: string): { html: string; frontmatter: string | null } {
  const { text, frontmatter } = bodyText(source);
  const cx = documentContext(text);
  const t = new HtmlTarget();
  const root = t.el("div");
  const env: Env<HNode> = { t, cx };
  for (const run of htmlRuns(topLevel(cx), (n) => htmlOfNode(n, cx))) renderRun(root, run, env);
  renderFootnotes(root, footnotesOf(cx), env);
  return { html: t.serializeChildren(root), frontmatter: frontmatter?.raw ?? null };
}

/**
 * The source line an in-document anchor names — a heading's slug, a
 * footnote (`fn-x`) or a reference to one (`fnref-x`) — by the ids this
 * renderer gives them, for the editor modes, which have no rendered ids.
 * Null when nothing carries it.
 */
export function anchorSourceLine(source: string, anchor: string): number | null {
  const { text } = bodyText(source);
  const cx = documentContext(text);
  const wanted = new Set(anchorIds(anchor));
  for (const h of cx.outline) if (wanted.has(`user-content-${h.id}`)) return cx.lines.lineOf(h.from);
  for (const f of cx.footnotes) {
    if (wanted.has(`user-content-fn-${escapeHref(f.label)}`)) return cx.lines.lineOf(f.node.from);
  }
  for (const [at, ref] of cx.footnoteRefs) {
    const id = `user-content-fnref-${escapeHref(ref.label)}${ref.nth > 1 ? `-${ref.nth}` : ""}`;
    if (wanted.has(id)) return cx.lines.lineOf(at);
  }
  return null;
}

// --- the DOM target ---------------------------------------------------------------

/** What only a page can do, supplied by the reader: fetch image bytes,
 *  lay out diagrams, load highlighting grammars. */
export interface DomHooks {
  image(img: HTMLImageElement, src: string, wikilink: string | null): void;
  code(code: HTMLElement, lang: string, text: string): void;
  mermaid(box: HTMLElement, source: string): void;
}

export class DomTarget implements Target<Node> {
  constructor(private readonly hooks: DomHooks) {}
  el(tag: string, attrs: Attrs = {}): Node {
    const e = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
    return e;
  }
  text(text: string): Node {
    return document.createTextNode(text);
  }
  add(parent: Node, child: Node): void {
    parent.appendChild(child);
  }
  attr(node: Node, name: string, value: string): void {
    if (node instanceof Element) node.setAttribute(name, value);
  }
  raw(parent: Node, html: string, sourcepos?: string): void {
    const frag = sanitizeHtml(html);
    // What the island's markdown (a run an HTML block opened) drew as
    // markup gets the same hydration as everywhere else.
    for (const img of frag.querySelectorAll<HTMLImageElement>("img[data-md-src]"))
      this.hooks.image(img, img.dataset.mdSrc ?? "", null);
    for (const code of frag.querySelectorAll<HTMLElement>("pre > code[data-lang]")) {
      const lang = code.dataset.lang ?? "";
      const text = code.textContent ?? "";
      if (lang === "mermaid") code.parentElement?.replaceWith(this.mermaid(text));
      else this.hooks.code(code, lang, text);
    }
    // Mapped like every other block, so a reveal or a selection inside
    // raw HTML still names its lines (comrak gives HTML no position).
    if (sourcepos !== undefined)
      for (const el of Array.from(frag.children))
        if (!el.hasAttribute("data-sourcepos")) el.setAttribute("data-sourcepos", sourcepos);
    parent.appendChild(frag);
  }
  math(source: string, display: boolean): Node {
    // Typeset after insertion, time-sliced (the reader's pass): a document
    // of a thousand equations must not stall its first paint.
    const span = document.createElement("span");
    span.dataset.mathStyle = display ? "display" : "inline";
    span.textContent = source;
    return span;
  }
  code(lang: string, text: string): Node {
    const pre = document.createElement("pre");
    const code = document.createElement("code");
    if (lang !== "") code.dataset.lang = lang;
    code.textContent = text;
    pre.append(code);
    if (lang !== "") this.hooks.code(code, lang, text);
    return pre;
  }
  mermaid(source: string): Node {
    const box = document.createElement("div");
    box.className = "md-mermaid";
    // The source shows (and copies) until the diagram is laid out.
    box.append(this.code("", source));
    this.hooks.mermaid(box, source);
    return box;
  }
  image(src: string, alt: string, title: string | null, wikilink: string | null): Node {
    const img = document.createElement("img");
    img.alt = alt;
    if (title !== null) img.title = title;
    if (src !== "" || wikilink !== null) this.hooks.image(img, src, wikilink);
    return img;
  }
}

// --- sanitizing raw HTML -----------------------------------------------------------

/** ammonia's default tags (the daemon's allowlist) plus the footnote
 *  `section`: what a document's own HTML may draw. */
const ALLOWED_TAGS = [
  "a", "abbr", "acronym", "area", "article", "aside", "b", "bdi", "bdo", "blockquote", "br",
  "caption", "center", "cite", "code", "col", "colgroup", "data", "dd", "del", "details", "dfn",
  "div", "dl", "dt", "em", "figcaption", "figure", "footer", "h1", "h2", "h3", "h4", "h5", "h6",
  "header", "hgroup", "hr", "i", "img", "ins", "kbd", "li", "map", "mark", "nav", "ol", "p", "pre",
  "q", "rp", "rt", "rtc", "ruby", "s", "samp", "small", "span", "strike", "strong", "sub",
  "summary", "sup", "table", "tbody", "td", "th", "thead", "time", "tr", "tt", "u", "ul", "var",
  "wbr", "section",
];

const ALLOWED_ATTR = [
  "lang", "title", "href", "hreflang", "dir", "cite", "align", "char", "charoff", "span",
  "datetime", "size", "width", "height", "alt", "src", "start", "summary", "colspan", "headers",
  "rowspan", "scope", "id", "class", "data-math-style", "data-task",
  // This renderer's own markers, inside a run an HTML block opened.
  "data-wikilink", "data-sourcepos", "data-lang", "aria-hidden", "aria-label",
];

/** ammonia's per-tag attributes; the rest of ALLOWED_ATTR is allowed on
 *  any tag (`lang`, `title`, and this renderer's markers). */
const TAG_ATTRS: Record<string, ReadonlySet<string>> = {
  A: new Set(["href", "hreflang"]),
  BDO: new Set(["dir"]),
  BLOCKQUOTE: new Set(["cite"]),
  COL: new Set(["align", "char", "charoff", "span"]),
  COLGROUP: new Set(["align", "char", "charoff", "span"]),
  DEL: new Set(["cite", "datetime"]),
  HR: new Set(["align", "size", "width"]),
  IMG: new Set(["align", "alt", "height", "src", "width"]),
  INS: new Set(["cite", "datetime"]),
  OL: new Set(["start"]),
  Q: new Set(["cite"]),
  TABLE: new Set(["align", "char", "charoff", "summary"]),
  TBODY: new Set(["align", "char", "charoff"]),
  TD: new Set(["align", "char", "charoff", "colspan", "headers", "rowspan"]),
  TH: new Set(["align", "char", "charoff", "colspan", "headers", "rowspan", "scope"]),
  THEAD: new Set(["align", "char", "charoff"]),
  TR: new Set(["align", "char", "charoff"]),
};
const TAG_ONLY_ATTRS = new Set(Object.values(TAG_ATTRS).flatMap((s) => [...s]));

/** Where the daemon lets an id through (namespaced `user-content-`). */
const ID_TAGS = new Set(["H1", "H2", "H3", "H4", "H5", "H6", "A", "LI"]);

/** The classes a rendered document may carry, per tag (the daemon's
 *  `MARKDOWN_CLASSES`): a document's own HTML can't borrow app chrome. */
const CLASSES: Record<string, ReadonlySet<string>> = {
  DIV: new Set([
    "markdown-alert",
    "markdown-alert-note",
    "markdown-alert-tip",
    "markdown-alert-important",
    "markdown-alert-warning",
    "markdown-alert-caution",
  ]),
  P: new Set(["markdown-alert-title"]),
  SPAN: new Set(["md-task"]),
  A: new Set(["anchor", "footnote-backref", "wikilink", "wikilink-embed"]),
  SUP: new Set(["footnote-ref"]),
  SECTION: new Set(["footnotes"]),
  UL: new Set(["contains-task-list"]),
  OL: new Set(["contains-task-list"]),
  LI: new Set(["task-list-item"]),
};

let purifier: ReturnType<typeof DOMPurify> | null = null;

function documentPurifier(): ReturnType<typeof DOMPurify> {
  if (purifier !== null) return purifier;
  // Its own instance: hooks are per instance, and chat's must not see
  // (or be seen by) the document policy.
  const p = DOMPurify(window);
  p.addHook("afterSanitizeAttributes", (node) => {
    if (!(node instanceof Element)) return;
    const tag = node.tagName;
    // ammonia's attributes are per tag (a `width` on an image, never on a
    // cell); DOMPurify's list is one set, so the rest go here.
    for (const name of node.getAttributeNames())
      if (TAG_ONLY_ATTRS.has(name) && TAG_ATTRS[tag]?.has(name) !== true) node.removeAttribute(name);
    const id = node.getAttribute("id");
    if (id !== null) {
      if (ID_TAGS.has(tag) && id !== "") {
        if (!id.startsWith("user-content-")) node.setAttribute("id", `user-content-${id}`);
      } else {
        node.removeAttribute("id");
      }
    }
    const cls = node.getAttribute("class");
    if (cls !== null) {
      const keep = cls.split(/\s+/).filter((c) => CLASSES[tag]?.has(c) === true);
      if (keep.length > 0) node.setAttribute("class", keep.join(" "));
      else node.removeAttribute("class");
    }
    for (const name of ["href", "src", "cite"]) {
      const v = node.getAttribute(name);
      if (v !== null && !urlAllowed(v)) node.removeAttribute(name);
    }
    if (tag === "A" && /^https?:/i.test(node.getAttribute("href") ?? "")) {
      node.setAttribute("target", "_blank");
      node.setAttribute("rel", "noopener noreferrer");
    }
    // A relative image would load from the app's origin (a 404): the
    // reader points it at the file beside the document instead.
    if (tag === "IMG") {
      const src = node.getAttribute("src");
      if (src !== null && !/^([a-z][a-z0-9+.-]*:|\/\/)/i.test(src)) {
        node.removeAttribute("src");
        node.setAttribute("data-md-src", src);
      }
    }
  });
  purifier = p;
  return p;
}

/** A document's raw HTML as sanitized nodes: chat's policy (no style tags
 *  or attributes; http(s) links open outside with no opener) narrowed to
 *  the daemon's allowlist, so the reading view and its server fallback
 *  agree on what survives. */
export function sanitizeHtml(html: string): DocumentFragment {
  return documentPurifier().sanitize(tasksToSpans(html), {
    ALLOWED_TAGS,
    ALLOWED_ATTR,
    ALLOW_DATA_ATTR: false,
    FORBID_TAGS: ["style"],
    FORBID_ATTR: ["style"],
    RETURN_DOM_FRAGMENT: true,
  });
}

