/**
 * Publishing, the page half (the pure pieces are `publish.ts`). Loaded on
 * first use from the toolbar's publish menu, never with the view.
 *
 * - `exportHtml`: the document through the reading view's renderer (the
 *   DOM target, then serialized) into ONE self-contained file — equations
 *   typeset (KaTeX's MathML), diagrams laid out as SVG, fences highlighted,
 *   pictures inlined as `data:` URLs under a byte cap, other embeds as a
 *   link card, relative links left relative, and a page policy that loads
 *   nothing and runs no script. Light, whatever the app's theme.
 * - `printDocument`: that same page in a hidden, script-free frame, handed
 *   to the browser's print dialog (save as PDF there).
 * - `exportBundle`: a zip of the document as saved and every file it names
 *   inside its workspace, laid out so its links still resolve.
 *
 * Everything is built in this tab from bytes the daemon already serves
 * (`/raw` tickets), so on a remote workspace it all crosses the tunnel once
 * and the daemon writes nothing; the result downloads through the browser.
 */
import type { Parser } from "@lezer/common";
import { LanguageDescription } from "@codemirror/language";
import { languages } from "@codemirror/language-data";
import { highlightCode } from "@lezer/highlight";
import { codeHighlight } from "../cm";
import { basename, dirname, humanSize, safeDecodeUri, tableHeaderRow } from "../files";
import { fetchRawBytes, saveBlob, TooLargeError } from "../rawBytes";
import { loadMath } from "../mathLoad";
import { renderMermaid } from "../../shared/mermaid";
import { cropLayout, embedKind, isMissing, parseSizeHint, splitTarget, type TargetInfo, type TargetResult } from "../../shared/embed/embed";
import { fragmentLabel, parseEmbedFragment } from "../../shared/embed/fragment";
import { activeTheme } from "../../settings/store.svelte";
import { defaultThemeFor } from "../../settings/themes";
import type { LinkContext } from "../docLinks";
import { bodyText, documentContext, footnotesOf, htmlOfNode, htmlRuns } from "./model";
import { DomTarget, escapeHref, renderFootnotes, renderRun, topLevel, type DomHooks, type EmbedRef, type EmbedSpec } from "./render";
import { markTasks } from "./reader";
import { DocEmbeds, refKey } from "./embeds";
import {
  applyEdits,
  BUNDLE_CAPS,
  docTargets,
  htmlPage,
  INLINE_CAP,
  isInside,
  localTarget,
  planBundle,
  planInline,
  relPath,
  stemOf,
  titleBlock,
  type DocTarget,
  type FoundTarget,
} from "./publish";
import { exportCss } from "./publishCss";

export interface PublishSource {
  /** The document's absolute path. */
  docPath: string;
  /** Its text as the view shows it, unsaved edits included. */
  text: string;
  links: LinkContext;
}

export interface PublishResult {
  /** The file handed to the browser, or null (print). */
  saved: string | null;
  /** What the reader should know: what was left out, and why. */
  notes: string[];
}

const HAS_SCHEME = /^([a-z][a-z0-9+.-]*:|\/\/)/i;
const plural = (n: number, one: string, many = `${one}s`): string => `${n} ${n === 1 ? one : many}`;

// --- code and equations ---------------------------------------------------------------

/** Past these a fence stays plain and an equation stays LaTeX (the reading
 *  view's limits). */
const HIGHLIGHT_MAX = 100_000;
const MATH_MAX = 16 * 1024;

interface Fence {
  code: HTMLElement;
  lang: string;
  text: string;
}

/** A fence painted with the reading view's highlighter (classes whose
 *  rules the page carries). */
function paint(code: HTMLElement, parser: Parser, text: string): void {
  let tree;
  try {
    tree = parser.parse(text);
  } catch {
    return;
  }
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

async function highlightFence(f: Fence): Promise<void> {
  if (f.text.length > HIGHLIGHT_MAX) return;
  const desc = LanguageDescription.matchLanguageName(languages, f.lang, true);
  if (desc === null) return;
  if (desc.support === undefined) {
    try {
      await desc.load();
    } catch {
      return; // plain text is fine
    }
  }
  if (desc.support !== undefined) paint(f.code, desc.support.language.parser, f.text);
}

type MathModule = Awaited<ReturnType<typeof loadMath>>;

function typeset(span: HTMLElement, math: MathModule): void {
  const display = span.dataset.mathStyle === "display";
  const source = span.textContent ?? "";
  span.classList.add("md-math");
  if (source.trim() === "" || source.length > MATH_MAX) return;
  if (display) span.classList.add("md-math-display");
  span.innerHTML = math.safeMathHtml(source, display); // sanitized by the shared math policy
}

// --- pictures -------------------------------------------------------------------------

/** A `Blob` as a `data:` URL, typed `mime` when the blob says nothing. */
function dataUrl(blob: Blob, mime: string): Promise<string> {
  const typed = blob.type === "" && mime !== "" ? new Blob([blob], { type: mime }) : blob;
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result));
    r.onerror = () => reject(r.error ?? new Error("unreadable"));
    r.readAsDataURL(typed);
  });
}

/** A resolved file's bytes: through its `/raw` ticket when the answer
 *  carries one, else a fresh one. */
async function fileBlob(info: TargetInfo, cap: number): Promise<Blob> {
  if (info.ticket === undefined) return new Blob([await fetchRawBytes(info.path, cap)]);
  const res = await fetch(`/raw/${info.ticket}`);
  if (!res.ok) throw new Error(`couldn't read ${basename(info.path)} (${res.status})`);
  const blob = await res.blob();
  if (blob.size > cap) throw new TooLargeError(blob.size, cap);
  return blob;
}

/** A picture on the web, if its host lets this page read it (CORS) and it
 *  fits what the cap has left. */
async function webBlob(url: string, cap: number): Promise<Blob | null> {
  try {
    const res = await fetch(url, { mode: "cors", credentials: "omit", signal: AbortSignal.timeout(8000) });
    if (!res.ok || !(res.headers.get("content-type") ?? "").startsWith("image/")) return null;
    const declared = Number(res.headers.get("content-length"));
    if (Number.isFinite(declared) && declared > cap) return null;
    const blob = await res.blob();
    return blob.size > cap ? null : blob;
  } catch {
    return null;
  }
}

/** A picture that is not in the page: a link to it, named by its alt text. */
function pictureLink(img: HTMLImageElement, href: string | null, name: string, why: string): void {
  const el = document.createElement(href === null ? "span" : "a");
  el.className = "md-image-link";
  if (el instanceof HTMLAnchorElement && href !== null) el.href = href;
  el.title = why;
  el.textContent = img.alt.trim() !== "" ? `${img.alt} (${name})` : name;
  img.replaceWith(el);
}

/** A picture cropped to a region (`#xywh=`), as the embed card draws it. */
function cropped(img: HTMLImageElement, info: TargetInfo, fragment: string | null, hint: number | null): HTMLElement | null {
  const region = parseEmbedFragment(fragment, info.path).at?.region;
  if (region === undefined || info.width === undefined || info.height === undefined) return null;
  const c = cropLayout({ w: info.width, h: info.height }, region);
  if (c === null) return null;
  const frame = document.createElement("span");
  frame.className = "md-crop";
  const natural = region.percent === true ? (region.w / 100) * info.width : region.w;
  frame.style.width = `${Math.round(Math.min(natural, hint ?? Infinity))}px`;
  frame.style.aspectRatio = String(c.aspect);
  img.removeAttribute("width");
  img.removeAttribute("height");
  img.style.width = `${c.width}%`;
  img.style.height = `${c.height}%`;
  img.style.left = `${c.left}%`;
  img.style.top = `${c.top}%`;
  img.replaceWith(frame);
  frame.append(img);
  return frame;
}

// --- file cards -------------------------------------------------------------------------

const SVG_NS = "http://www.w3.org/2000/svg";

function fileGlyph(): SVGSVGElement {
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  const path = document.createElementNS(SVG_NS, "path");
  path.setAttribute("d", "M14 3v4a1 1 0 0 0 1 1h4M17 21H7a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h7l5 5v11a2 2 0 0 1-2 2z");
  path.setAttribute("fill", "none");
  path.setAttribute("stroke", "currentColor");
  path.setAttribute("stroke-width", "1.6");
  path.setAttribute("stroke-linecap", "round");
  path.setAttribute("stroke-linejoin", "round");
  svg.append(path);
  return svg;
}

/** An embed that is no picture: a card linking to the file, relative to
 *  the document, with its name and the piece it showed. */
function fillCard(a: HTMLAnchorElement, spec: EmbedSpec, answer: TargetResult | null, docPath: string): void {
  const { path, fragment } = splitTarget(spec.target);
  const hit = answer !== null && !isMissing(answer) ? answer : null;
  const named = hit?.path ?? safeDecodeUri(path);
  // A by-name embed names a note, not a path: point at where it is.
  const href = spec.byName && hit !== null ? escapeHref(relPath(dirname(docPath), hit.path)) + (fragment !== null ? `#${fragment}` : "") : spec.target;
  a.href = href;
  if (hit === null) a.title = "not found when this page was made";
  const name = document.createElement("span");
  name.className = "md-file-name";
  name.textContent = basename(named) || named;
  a.append(fileGlyph(), name);
  const label = fragmentLabel(parseEmbedFragment(fragment, named), tableHeaderRow(named));
  if (label !== "") {
    const f = document.createElement("span");
    f.className = "md-file-frag";
    f.textContent = label;
    a.append(f);
  }
  const alt = parseSizeHint(spec.alt).alt.trim();
  if (alt !== "") {
    const c = document.createElement("span");
    c.className = "md-file-alt";
    c.textContent = alt;
    a.append(c);
  }
}

// --- the page ---------------------------------------------------------------------------

/** The light palette the page is drawn in: the app's own theme when it is
 *  a light one, else the default light theme. */
function lightTokens(): Record<string, string> {
  const t = activeTheme();
  return t.kind === "light" ? t.tokens : defaultThemeFor("light").tokens;
}

/** The document as one self-contained page, and what it left out. */
export async function buildPage(src: PublishSource): Promise<{ html: string; notes: string[] }> {
  const notes: string[] = [];
  const { text, frontmatter } = bodyText(src.text);
  const cx = documentContext(text);
  const fences: Fence[] = [];
  const diagrams: { box: HTMLElement; source: string }[] = [];
  const cards: { el: HTMLAnchorElement; spec: EmbedSpec }[] = [];
  const hooks: DomHooks = {
    image: (img, s, wikilink) => {
      img.setAttribute("data-publish-src", s);
      if (wikilink !== null) img.setAttribute("data-publish-name", "");
    },
    embed: (spec) => {
      if (spec.image) {
        // `![[x|400]]`: the alias is the size hint itself (the reader's rule).
        const hint = parseSizeHint(spec.byName && /^\d+(\s*x\s*\d+)?$/.test(spec.alt) ? `|${spec.alt}` : spec.alt);
        const img = document.createElement("img");
        img.alt = hint.alt;
        img.setAttribute("data-publish-src", spec.target);
        if (spec.byName) img.setAttribute("data-publish-name", "");
        if (hint.width !== null) img.setAttribute("data-publish-width", String(hint.width));
        return img;
      }
      const a = document.createElement("a");
      a.className = "md-file";
      cards.push({ el: a, spec });
      return a;
    },
    code: (code, lang, t) => {
      fences.push({ code, lang, text: t });
    },
    mermaid: (box, source) => {
      diagrams.push({ box, source });
    },
  };
  const article = document.createElement("article");
  const env = { t: new DomTarget(hooks), cx };
  for (const run of htmlRuns(topLevel(cx), (n) => htmlOfNode(n, cx))) renderRun(article, run, env);
  renderFootnotes(article, footnotesOf(cx), env);
  markTasks(article);

  const spans = [...article.querySelectorAll<HTMLElement>("span[data-math-style]")];
  if (spans.length > 0) {
    try {
      const math = await loadMath();
      for (const s of spans) typeset(s, math);
    } catch {
      notes.push("equations stay as LaTeX: the math renderer didn't load");
    }
  }
  await Promise.all(fences.map(highlightFence));
  for (const d of diagrams) {
    try {
      const svg = await renderMermaid(d.source, "light");
      const holder = document.createElement("div");
      holder.className = "md-mermaid-svg";
      holder.innerHTML = svg; // sanitized by shared/mermaid
      d.box.replaceChildren(holder);
    } catch (err) {
      const note = document.createElement("p");
      note.className = "md-mermaid-note";
      note.textContent = `diagram error: ${err instanceof Error ? err.message : String(err)}`;
      d.box.prepend(note);
    }
  }

  await placePictures(article, cards, src, notes);

  // The page stands alone: no line map, and ids as its own links name them
  // (`#results`, `#fn-1`) — the app's `user-content-` namespace is for
  // sharing a page with the workbench.
  for (const el of article.querySelectorAll("*")) {
    for (const name of el.getAttributeNames())
      if (name === "data-sourcepos" || name === "data-md-src" || name.startsWith("data-publish")) el.removeAttribute(name);
    const id = el.getAttribute("id");
    if (id !== null && id.startsWith("user-content-")) el.setAttribute("id", id.slice("user-content-".length));
  }

  // The block's title stands in for an opening `# Title` only when it
  // says something else.
  const first = cx.outline[0];
  const opening = first !== undefined && first.level === 1 && topLevel(cx)[0]?.from === first.from ? first.text : null;
  let block = titleBlock(frontmatter?.raw ?? null, opening, src.docPath);
  const h1 = opening === null ? null : article.querySelector(":scope > h1");
  if (h1 !== null && block.heading === null && (block.summary !== null || block.updated !== null)) {
    // The document's own title heads the page: the summary and date go
    // under it, not above.
    const meta = document.createElement("header");
    meta.className = "doc-meta";
    if (block.summary !== null) meta.append(Object.assign(document.createElement("p"), { className: "doc-summary", textContent: block.summary }));
    if (block.updated !== null) meta.append(Object.assign(document.createElement("p"), { className: "doc-updated", textContent: `Updated ${block.updated}` }));
    h1.after(meta);
    block = { ...block, summary: null, updated: null };
  }
  const css = exportCss(lightTokens(), codeHighlight.module?.getRules() ?? "");
  return { html: htmlPage(block, css, article.innerHTML), notes };
}

/** Every picture inlined (under the cap) or turned into a link, and every
 *  file card filled — from one resolve of the document's references. */
async function placePictures(
  article: HTMLElement,
  cards: readonly { el: HTMLAnchorElement; spec: EmbedSpec }[],
  src: PublishSource,
  notes: string[],
): Promise<void> {
  // A raw-HTML picture on the web kept its src; nothing may load from the
  // page itself, so it goes the same way as the rest.
  for (const img of article.querySelectorAll<HTMLImageElement>("img")) {
    const s = img.getAttribute("src");
    if (!img.hasAttribute("data-publish-src") && s !== null && /^https?:/i.test(s)) img.setAttribute("data-publish-src", s);
    img.removeAttribute("src");
  }
  const imgs = [...article.querySelectorAll<HTMLImageElement>("img[data-publish-src]")];
  const refOf = (target: string, byName: boolean): EmbedRef | null =>
    !byName && HAS_SCHEME.test(target) ? null : { target, byName };
  const embeds = new DocEmbeds(src.docPath, () => src.links);
  const answers = new Map<string, TargetResult | null>();
  try {
    const refs = [
      ...imgs.map((img) => refOf(img.getAttribute("data-publish-src") ?? "", img.hasAttribute("data-publish-name"))),
      ...cards.map((c) => ({ target: c.spec.target, byName: c.spec.byName })),
    ].filter((r): r is EmbedRef => r !== null);
    await Promise.all(refs.map(async (r) => answers.set(refKey(r), await embeds.ask(r))));
  } finally {
    embeds.dispose();
  }
  const docDir = dirname(src.docPath);
  const answerOf = (ref: EmbedRef | null): TargetResult | null => (ref === null ? null : (answers.get(refKey(ref)) ?? null));

  for (const c of cards) fillCard(c.el, c.spec, answerOf({ target: c.spec.target, byName: c.spec.byName }), src.docPath);

  interface Local {
    img: HTMLImageElement;
    info: TargetInfo;
    target: string;
    size: number;
  }
  const local: Local[] = [];
  const web: HTMLImageElement[] = [];
  let missing = 0;
  for (const img of imgs) {
    const target = img.getAttribute("data-publish-src") ?? "";
    const ref = refOf(target, img.hasAttribute("data-publish-name"));
    if (ref === null) {
      web.push(img);
      continue;
    }
    const a = answerOf(ref);
    const { path } = splitTarget(target);
    const name = basename(a !== null && !isMissing(a) ? a.path : safeDecodeUri(path));
    if (a === null || isMissing(a) || embedKind(a) !== "image") {
      missing++;
      pictureLink(img, target, name, "not found when this page was made");
      continue;
    }
    local.push({ img, info: a, target, size: a.size });
  }
  const plan = planInline(local, INLINE_CAP);
  let budget = INLINE_CAP - plan.bytes;
  let failed = 0;
  for (const p of plan.inline) {
    const { img, info, target } = p;
    const href = img.hasAttribute("data-publish-name") ? escapeHref(relPath(docDir, info.path)) : splitTarget(target).path;
    try {
      img.src = await dataUrl(await fileBlob(info, p.size + 1024 * 1024), info.mime);
    } catch {
      failed++;
      budget += p.size;
      pictureLink(img, href, basename(info.path), "couldn't be read when this page was made");
      continue;
    }
    const hint = Number(img.getAttribute("data-publish-width") ?? "");
    const width = Number.isFinite(hint) && hint > 0 ? hint : null;
    if (cropped(img, info, splitTarget(target).fragment, width) !== null) continue;
    // The box the picture takes, reserved before it decodes — unless the
    // document sized it itself (raw HTML's `width`).
    if (width !== null) {
      img.width = width;
    } else if (info.width !== undefined && info.height !== undefined && !img.hasAttribute("width") && !img.hasAttribute("height")) {
      img.width = info.width;
      img.height = info.height;
    }
  }
  for (const p of plan.over) {
    const href = p.img.hasAttribute("data-publish-name") ? escapeHref(relPath(docDir, p.info.path)) : splitTarget(p.target).path;
    pictureLink(p.img, href, basename(p.info.path), `${humanSize(p.size)}: past the ${humanSize(INLINE_CAP)} this page embeds`);
  }
  let webLinked = 0;
  for (const img of web) {
    const url = img.getAttribute("data-publish-src") ?? "";
    const blob = await webBlob(url, budget);
    if (blob !== null) {
      img.src = await dataUrl(blob, "");
      budget -= blob.size;
    } else {
      webLinked++;
      pictureLink(img, url, url.replace(/^https?:\/\//i, "").split(/[?#]/)[0] ?? url, "a web picture this page could not embed");
    }
  }
  if (plan.over.length > 0)
    notes.push(`${plural(plan.over.length, "picture")} past the ${humanSize(INLINE_CAP)} embed cap ${plan.over.length === 1 ? "is a link" : "are links"}`);
  if (missing > 0) notes.push(`${plural(missing, "picture")} not found: ${missing === 1 ? "its" : "their"} alt text links to the path`);
  if (failed > 0) notes.push(`${plural(failed, "picture")} couldn't be read: linked instead`);
  if (webLinked > 0) notes.push(`${plural(webLinked, "web picture")} couldn't be embedded (the host doesn't allow it): linked instead`);
}

// --- the three actions ---------------------------------------------------------------------

/** One self-contained HTML file, downloaded through the browser. */
export async function exportHtml(src: PublishSource): Promise<PublishResult> {
  const { html, notes } = await buildPage(src);
  const name = `${stemOf(src.docPath)}.html`;
  saveBlob(new Blob([html], { type: "text/html;charset=utf-8" }), name);
  return { saved: name, notes };
}

/**
 * The page in a one-off frame, printed: the browser's dialog saves it as
 * PDF. As the slides view prints: the frame must be same-origin to be told
 * to print, so it stays script-free (the page's own policy forbids scripts
 * too); the sandbox grants the print dialog only. In the native app
 * (WKWebView) `print()` goes through the system's print panel, where PDF
 * is under the panel's PDF menu.
 */
export async function printDocument(src: PublishSource): Promise<PublishResult> {
  const { html, notes } = await buildPage(src);
  await new Promise<void>((resolve) => {
    const frame = document.createElement("iframe");
    frame.setAttribute("sandbox", "allow-same-origin allow-modals");
    frame.setAttribute("aria-hidden", "true");
    frame.tabIndex = -1;
    frame.style.cssText = "position:fixed;right:0;bottom:0;width:0;height:0;border:0;opacity:0";
    frame.srcdoc = html;
    frame.addEventListener(
      "load",
      () => {
        const win = frame.contentWindow;
        // Pictures decode just after load; give them a beat.
        setTimeout(() => {
          try {
            win?.focus();
            win?.print();
          } finally {
            setTimeout(() => {
              frame.remove();
              resolve();
            }, 1000);
          }
        }, 300);
      },
      { once: true },
    );
    document.body.appendChild(frame);
  });
  return { saved: null, notes };
}

/** Already compressed: stored, not deflated again. */
const STORED = /\.(png|jpe?g|gif|webp|avif|pdf|zip|gz|tgz|bz2|xz|zst|7z|mp4|m4v|webm|mov|mp3|m4a|ogg|oga|ogv|flac|docx|xlsx|pptx|parquet|npz)$/i;

/** Past this the document itself is not bundled (it is never this big). */
const DOC_MAX = 16 * 1024 * 1024;

/**
 * A zip of the document as saved plus every file it links or embeds that
 * lies inside its workspace (the document's own folder when it has none),
 * at the places its links name — so they resolve unzipped, on GitHub and
 * in Obsidian. Targets resolve as document links do: beside the document,
 * a root-relative `/x` also against the workspace root, a wikilink by
 * name. Linked notes come along, not the files they link in turn.
 */
export async function exportBundle(src: PublishSource): Promise<PublishResult> {
  const notes: string[] = [];
  const bytes = new Uint8Array(await fetchRawBytes(src.docPath, DOC_MAX));
  const text = new TextDecoder("utf-8", { ignoreBOM: true }).decode(bytes);
  const plain = (s: string): string => s.replace(/^\ufeff/, "").replace(/\r\n?/g, "\n");
  if (plain(text) !== plain(src.text)) notes.push("the bundle has the file as saved; this view has unsaved edits");

  const locals = docTargets(text)
    .map((target) => ({ target, at: localTarget(target.href) }))
    .filter((x): x is { target: DocTarget; at: NonNullable<typeof x.at> } => x.at !== null);
  // Document-link rules: strictly beside the document (no workspace-root
  // fallback for a relative path), `/x` also under the root, wikilinks by
  // name.
  const embeds = new DocEmbeds(src.docPath, () => ({ wsRoot: null, workspaceId: src.links.workspaceId }));
  const answers = new Map<DocTarget, TargetResult | null>();
  try {
    await Promise.all(
      locals.map(async ({ target }) => answers.set(target, await embeds.ask({ target: target.href, byName: target.byName }))),
    );
  } finally {
    embeds.dispose();
  }
  const found: FoundTarget[] = [];
  const missing: DocTarget[] = [];
  const tickets = new Map<string, TargetInfo>();
  let unknown = 0;
  for (const { target, at } of locals) {
    const a = answers.get(target) ?? null;
    if (a === null) {
      unknown++;
      continue;
    }
    if (isMissing(a)) {
      missing.push(target);
      continue;
    }
    tickets.set(a.path, a);
    found.push({ target, written: at.path, fragment: at.fragment, real: a.path, kind: a.kind, size: a.size });
  }
  const ws = src.links.wsRoot;
  const boundary = ws !== null && ws !== "" && isInside(src.docPath, ws) ? ws : dirname(src.docPath);
  const plan = planBundle(src.docPath, boundary, found, missing, BUNDLE_CAPS);

  const { default: JSZip } = await import("jszip");
  const zip = new JSZip();
  zip.file(plan.doc, plan.rewrites.length === 0 ? bytes : new TextEncoder().encode(applyEdits(text, plan.rewrites)));
  let unread = 0;
  let total = bytes.length;
  // A few at a time: over a tunnel each read is a round trip or two.
  const queue = [...plan.files];
  const worker = async (): Promise<void> => {
    for (let f = queue.shift(); f !== undefined; f = queue.shift()) {
      const info = tickets.get(f.real);
      try {
        const cap = BUNDLE_CAPS.bytes - total;
        const blob = info !== undefined ? await fileBlob(info, cap) : new Blob([await fetchRawBytes(f.real, cap)]);
        total += blob.size;
        zip.file(f.entry, await blob.arrayBuffer(), { compression: STORED.test(f.entry) ? "STORE" : "DEFLATE" });
      } catch {
        unread++;
      }
    }
  };
  await Promise.all([worker(), worker(), worker(), worker()]);
  const blob = await zip.generateAsync({ type: "blob", compression: "DEFLATE", compressionOptions: { level: 6 } });
  const name = `${stemOf(src.docPath)}.zip`;
  saveBlob(blob, name);

  const count = (why: string): number => plan.skipped.filter((s) => s.why === why).length;
  const files = plan.files.length - unread;
  notes.unshift(`${plural(files + 1, "file")}, ${humanSize(total)}`);
  if (plan.rewrites.length > 0) notes.push(`${plural(plan.rewrites.length, "absolute path")} rewritten as relative in the copy`);
  if (count("missing") > 0) notes.push(`${plural(count("missing"), "link")} to nothing left as written`);
  if (count("outside") > 0) notes.push(`${plural(count("outside"), "file")} outside the workspace left out`);
  if (count("folder") > 0) notes.push(`${plural(count("folder"), "folder link")} left out (folders aren't bundled)`);
  if (count("cap") > 0) notes.push(`${plural(count("cap"), "file")} past the ${humanSize(BUNDLE_CAPS.bytes)} / ${BUNDLE_CAPS.files}-file cap left out`);
  if (unread > 0) notes.push(`${plural(unread, "file")} couldn't be read`);
  if (unknown > 0) notes.push(`${plural(unknown, "link")} couldn't be checked (daemon busy)`);
  return { saved: name, notes };
}
