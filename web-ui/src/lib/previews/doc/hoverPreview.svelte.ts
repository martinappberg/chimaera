/**
 * Hover previews on a markdown document's links, for the reading view and
 * live mode (one controller per view, on its content box):
 *
 * - **reading**: rest the pointer on a link for 400 ms;
 * - **live**: hold Mod (Cmd, or Ctrl) over a link — a plain hover is for
 *   editing there — in a rendered block or in the source being edited;
 * - **keyboard**: Mod+K on a focused link (reading) or with the cursor in
 *   a link (live) shows it; Mod+K again, Escape, or moving on hides it.
 *
 * A `.md` target (or a heading in this document) shows its section — or
 * its opening — drawn by the reading renderer (`Hydrator`: equations,
 * fences, diagrams and pictures as the reading view draws them); a
 * footnote reference shows the note; any other file shows the compact body
 * an embed card gives it, at the link's fragment (a PDF page, a picture's
 * region, a table's rows). Web links show nothing. Nothing loads until the
 * preview is due, and leaving first cancels it. The popover never takes
 * focus, its content is inert, and it goes on leave, Escape, a press
 * elsewhere, a scroll, or a mode switch.
 */
import { flushSync, mount, unmount } from "svelte";
import type { EditorView } from "@codemirror/view";
import { syntaxTree } from "@codemirror/language";
import type { SyntaxNode } from "@lezer/common";
import { basename, fsFile } from "../files";
import { isMac } from "../../shared/keys";
import { mountEmbed, type EmbedHandle } from "../../shared/embed/mount.svelte";
import { embedKind, isMissing, type TargetInfo } from "../../shared/embed/embed";
import { fragmentLabel, parseEmbedFragment } from "../../shared/embed/fragment";
import { anchorIds, decodeAnchor } from "../mdDoc";
import { destinationText, inlineOf } from "../mdTable";
import { isMarpSource } from "../marp";
import type { LinkContext } from "../docLinks";
import { bodyText, documentContext, footnotesOf, htmlOfNode, htmlRuns, type DocContext } from "./model";
import { escapeHref, hrefFor, renderBlocks, renderRun, topLevel, wikilinkHref } from "./render";
import { Hydrator, markTasks } from "./reader";
import type { DocEmbeds } from "./embeds";
import { hoverTarget, isLineFragment, placePopover, sectionBounds, type HoverTarget, type Placement, type Rect } from "./hover";
import HoverPreview from "./HoverPreview.svelte";

export interface HoverHost {
  /** The positioned content box every link lives in; the popover mounts here. */
  root: HTMLElement;
  docPath: () => string;
  mode: () => "reading" | "live" | "source";
  /** The document's current text (a same-document preview reads it). */
  text: () => string | null;
  links: () => LinkContext;
  /** The document's embed answers: a target resolves once for both. */
  embeds: () => DocEmbeds;
  theme: () => "light" | "dark";
  /** The body text size, px. */
  fontSize: () => number;
  /** Live mode's editor, when mounted. */
  editor: () => EditorView | null;
}

/** What the popover draws (reactive; the component reads it). */
export interface PreviewState {
  id: string;
  kind: "doc" | "card";
  visible: boolean;
  place: Placement;
  fontSize: number;
  path: string;
  name: string;
  label: string;
  status: "loading" | "ready" | "missing" | "error";
  message: string;
  content: HTMLElement;
  onEnter: () => void;
  onLeave: () => void;
}

/** A link the pointer or the keyboard is on. */
interface Candidate {
  /** Identity: the element, or a source link's href and offset. */
  key: unknown;
  href: string;
  byName: boolean;
  rect: DOMRect;
  via: "pointer" | "key";
}

const READING_DELAY = 400;
const LIVE_DELAY = 60;
/** Leaving the link: this long to reach the popover before it goes. */
const LEAVE_GRACE = 220;
/** A loading preview shows its frame after this (none flashes before). */
const REVEAL_AFTER = 250;
const WANT = { width: 440, height: 340 };
/** Blocks drawn at most (the popover clips; this bounds the work). */
const MAX_BLOCKS = 24;
const SOURCE_MAX = 1024 * 1024;
const CACHE_MAX = 4;
const CACHE_ENTRY_MAX = 256 * 1024;

let seq = 0;

/** The link destination at `pos` in the editor's source text, as an href
 *  (a wikilink's as the renderer gives it). */
function sourceLinkAt(view: EditorView, pos: number): { href: string; byName: boolean; from: number } | null {
  const doc = { sliceString: (a: number, b: number) => view.state.sliceDoc(a, b) };
  for (let n: SyntaxNode | null = syntaxTree(view.state).resolveInner(pos, 1); n !== null; n = n.parent) {
    if (n.name === "Link" || n.name === "Image") {
      const u = n.getChild("URL");
      const href = u === null ? null : hrefFor(destinationText(view.state.sliceDoc(u.from, u.to)));
      return href === null || href === "" ? null : { href, byName: false, from: n.from };
    }
    if (n.name === "Wikilink" && n.parent !== null) {
      for (const i of inlineOf(n.parent, n.from, n.to, doc)) {
        if (i.kind !== "wikilink" || i.target === "") continue;
        const href = i.embed
          ? `${wikilinkHref(i.target, null)}${i.heading === null ? "" : `#${escapeHref(i.heading)}`}`
          : wikilinkHref(i.target, i.heading);
        return { href, byName: true, from: n.from };
      }
      return null;
    }
  }
  return null;
}

/** The rect of `el` the point sits in (a wrapped link has several). */
function rectNear(el: Element, x: number | null, y: number | null): DOMRect {
  const rects = Array.from(el.getClientRects());
  if (x !== null && y !== null)
    for (const r of rects) if (x >= r.left - 1 && x <= r.right + 1 && y >= r.top - 1 && y <= r.bottom + 1) return r;
  return rects[0] ?? el.getBoundingClientRect();
}

export class HoverPreviews {
  private pending: { c: Candidate; timer: ReturnType<typeof setTimeout> } | null = null;
  private shown: { c: Candidate; state: PreviewState; component: ReturnType<typeof mount>; abort: AbortController; teardown: () => void } | null = null;
  private hideTimer: ReturnType<typeof setTimeout> | null = null;
  private pointer: { x: number; y: number; inside: boolean } = { x: 0, y: 0, inside: false };
  private readonly sources = new Map<string, string>();
  private readonly off: (() => void)[] = [];

  constructor(private readonly host: HoverHost) {
    const root = host.root;
    const on = <K extends keyof HTMLElementEventMap>(
      el: HTMLElement | Window,
      type: K,
      fn: (e: HTMLElementEventMap[K]) => void,
      opts?: AddEventListenerOptions,
    ): void => {
      el.addEventListener(type, fn as EventListener, opts);
      this.off.push(() => el.removeEventListener(type, fn as EventListener, opts));
    };
    on(root, "pointermove", (e) => this.move(e), { passive: true });
    on(root, "pointerleave", () => {
      this.pointer.inside = false;
      this.cancelPending();
      if (this.shown?.c.via === "pointer") this.scheduleHide();
    });
    on(root, "keydown", (e) => this.key(e));
    // Anything scrolling moves the link out from under the popover.
    on(root, "scroll", (e) => {
      if (this.shown !== null && !this.inPopover(e.target)) this.hide();
    }, { capture: true, passive: true });
    on(root, "focusout", (e) => {
      if (this.shown?.c.via === "key" && e.target === this.shown.c.key) this.hide();
    });
    on(window, "keydown", (e) => this.windowKey(e), { capture: true });
    on(window, "pointerdown", (e) => {
      if (this.shown !== null && !this.inPopover(e.target)) this.hide();
      this.cancelPending();
    }, { capture: true });
    on(window, "blur", () => {
      this.cancelPending();
      this.hide();
    });
  }

  destroy(): void {
    this.cancelPending();
    this.hide();
    for (const f of this.off) f();
    this.off.length = 0;
    this.sources.clear();
  }

  /** Close the popover (a mode switch, a new document). */
  hide(): void {
    if (this.hideTimer !== null) clearTimeout(this.hideTimer);
    this.hideTimer = null;
    const s = this.shown;
    if (s === null) return;
    this.shown = null;
    s.abort.abort();
    s.teardown();
    void unmount(s.component);
    if (s.c.key instanceof Element) s.c.key.removeAttribute("aria-describedby");
  }

  // --- pointer ---------------------------------------------------------------------

  private inPopover(t: EventTarget | null): boolean {
    return t instanceof Node && this.shown !== null && (this.shown.state.content.contains(t) || this.popoverEl()?.contains(t) === true);
  }

  private popoverEl(): HTMLElement | null {
    const s = this.shown;
    return s === null ? null : document.getElementById(s.state.id);
  }

  private move(e: PointerEvent): void {
    this.pointer = { x: e.clientX, y: e.clientY, inside: true };
    if (e.pointerType === "touch") return;
    if (e.buttons !== 0) {
      // Selecting text, not pointing at a link.
      this.cancelPending();
      return;
    }
    this.consider(e.target, e.metaKey || e.ctrlKey);
  }

  /** What the pointer rests on, as a candidate (or nothing). In live mode
   *  a link needs Mod to become one — but the link already previewed stays
   *  itself without it, so letting go of Mod to read keeps it open. */
  private candidateAt(target: EventTarget | null, mod: boolean): Candidate | null {
    const mode = this.host.mode();
    if (mode === "source" || !(target instanceof Element)) return null;
    const { x, y } = this.pointer;
    const live = mode === "live";
    const shownKey = this.shown?.c.key;
    const a = target.closest<HTMLAnchorElement>("a[href]");
    if (a !== null && this.host.root.contains(a) && a.closest(".embed-card, .md-props") === null && !a.classList.contains("anchor")) {
      if (live && !mod && a !== shownKey) return null;
      return { key: a, href: a.getAttribute("href") ?? "", byName: a.hasAttribute("data-wikilink"), rect: rectNear(a, x, y), via: "pointer" };
    }
    if (!live || (!mod && typeof shownKey !== "string")) return null;
    const view = this.host.editor();
    if (view === null || !view.contentDOM.contains(target)) return null;
    const pos = view.posAtCoords({ x, y });
    const link = pos === null ? null : sourceLinkAt(view, pos);
    if (link === null) return null;
    const key = `src:${link.from}:${link.href}`;
    if (!mod && key !== shownKey) return null;
    const lineHeight = view.defaultLineHeight;
    return { key, href: link.href, byName: link.byName, rect: new DOMRect(x - 4, y - lineHeight / 2, 8, lineHeight), via: "pointer" };
  }

  private consider(target: EventTarget | null, mod: boolean): void {
    if (this.inPopover(target)) {
      this.cancelHide();
      return;
    }
    const c = this.candidateAt(target, mod);
    const same = (o: Candidate | undefined): boolean => o !== undefined && c !== null && o.key === c.key;
    if (same(this.shown?.c)) {
      this.cancelHide();
      return;
    }
    if (same(this.pending?.c)) return;
    this.cancelPending();
    if (c === null || hoverTarget(c.href, c.byName) === null) {
      if (this.shown?.c.via === "pointer") this.scheduleHide();
      return;
    }
    const delay = this.host.mode() === "live" ? LIVE_DELAY : READING_DELAY;
    this.pending = {
      c,
      timer: setTimeout(() => {
        this.pending = null;
        this.show(c);
      }, delay),
    };
  }

  private cancelPending(): void {
    if (this.pending !== null) clearTimeout(this.pending.timer);
    this.pending = null;
  }

  private scheduleHide(): void {
    if (this.hideTimer !== null) return;
    this.hideTimer = setTimeout(() => {
      this.hideTimer = null;
      this.hide();
    }, LEAVE_GRACE);
  }

  private cancelHide(): void {
    if (this.hideTimer !== null) clearTimeout(this.hideTimer);
    this.hideTimer = null;
  }

  // --- keyboard ---------------------------------------------------------------------

  private key(e: KeyboardEvent): void {
    const modK = (isMac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "k";
    if (!modK || this.host.mode() === "source") return;
    const c = this.keyCandidate();
    if (c === null) return;
    e.preventDefault();
    e.stopPropagation();
    if (this.shown !== null && this.shown.c.key === c.key) {
      this.hide();
      return;
    }
    this.cancelPending();
    this.show(c);
  }

  /** The link the keyboard is on: the focused one, or the cursor's. */
  private keyCandidate(): Candidate | null {
    const active = document.activeElement;
    if (this.host.mode() === "reading") {
      if (!(active instanceof HTMLAnchorElement) || !this.host.root.contains(active) || !active.hasAttribute("href")) return null;
      return { key: active, href: active.getAttribute("href") ?? "", byName: active.hasAttribute("data-wikilink"), rect: rectNear(active, null, null), via: "key" };
    }
    const view = this.host.editor();
    if (view === null || !view.hasFocus) return null;
    const head = view.state.selection.main.head;
    const link = sourceLinkAt(view, head) ?? (head > 0 ? sourceLinkAt(view, head - 1) : null);
    const at = view.coordsAtPos(head);
    if (link === null || at === null) return null;
    return { key: `src:${link.from}:${link.href}`, href: link.href, byName: link.byName, rect: new DOMRect(at.left, at.top, 1, at.bottom - at.top), via: "key" };
  }

  private windowKey(e: KeyboardEvent): void {
    const s = this.shown;
    if (e.key === "Escape" && s !== null) {
      e.preventDefault();
      e.stopPropagation();
      this.hide();
      return;
    }
    if (e.key === "Meta" || e.key === "Control") {
      // Mod pressed with the pointer already resting on a link (live).
      if (this.pointer.inside && this.host.mode() === "live") {
        this.consider(document.elementFromPoint(this.pointer.x, this.pointer.y), true);
      }
      return;
    }
    // Any other key moves on (typing, a tab switch that parks this view):
    // a preview is a glance. Mod+K is the root's to toggle.
    const modK = (e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k";
    if (s !== null && !modK && !["Shift", "Alt", "AltGraph", "CapsLock"].includes(e.key)) this.hide();
  }

  // --- showing ----------------------------------------------------------------------

  private show(c: Candidate): void {
    const target = hoverTarget(c.href, c.byName);
    // A place in a document drawn by the daemon (too large for the browser)
    // has no text here to draw from.
    if (target === null || (target.kind === "self" && this.host.text() === null)) return;
    this.hide();
    const root = this.host.root;
    const box = root.getBoundingClientRect();
    const rel: Rect = { left: c.rect.left - box.left, top: c.rect.top - box.top, right: c.rect.right - box.left, bottom: c.rect.bottom - box.top };
    const content = document.createElement("div");
    const state: PreviewState = $state({
      id: `md-hover-${++seq}`,
      kind: "doc",
      visible: false,
      place: placePopover(rel, { width: box.width, height: box.height }, WANT),
      fontSize: this.host.fontSize(),
      path: this.host.docPath(),
      name: "",
      label: "",
      status: "loading",
      message: "",
      content,
      onEnter: () => this.cancelHide(),
      onLeave: () => {
        if (this.shown?.c.via === "pointer") this.scheduleHide();
      },
    });
    const component = mount(HoverPreview, { target: root, props: { s: state } });
    // The content node is in the page before anything draws into it: the
    // reader's passes (equations, fences) only work on connected nodes.
    flushSync();
    const abort = new AbortController();
    const cleanups: (() => void)[] = [];
    const reveal = setTimeout(() => {
      state.visible = true;
      if (state.status === "loading") state.message = "loading…";
    }, REVEAL_AFTER);
    cleanups.push(() => clearTimeout(reveal));
    this.shown = { c, state, component, abort, teardown: () => cleanups.forEach((f) => f()) };
    if (c.key instanceof Element) c.key.setAttribute("aria-describedby", state.id);
    const done = (): void => {
      if (abort.signal.aborted) return;
      clearTimeout(reveal);
      state.visible = true;
    };
    void this.load(target, state, abort.signal, cleanups).then(done, (err: unknown) => {
      if (abort.signal.aborted) return;
      state.status = "error";
      state.message = err instanceof Error ? err.message : "couldn't preview this link";
      done();
    });
  }

  private async load(t: HoverTarget, state: PreviewState, signal: AbortSignal, cleanups: (() => void)[]): Promise<void> {
    const docPath = this.host.docPath();
    if (t.kind === "self") {
      const text = this.host.text();
      if (text === null) throw new Error("this document hasn't loaded yet");
      this.drawDoc(state, docPath, text, t.anchor, cleanups);
      return;
    }
    const a = await this.host.embeds().ask({ target: t.target, byName: t.byName });
    if (signal.aborted) return;
    if (a === null) throw new Error("couldn't reach the daemon");
    if (isMissing(a)) {
      state.kind = "doc";
      state.name = decodeAnchor(t.target.split("#")[0] ?? t.target);
      state.status = "missing";
      state.message = "Not found: nothing at this path yet.";
      return;
    }
    if (a.kind === "file" && a.path === docPath) {
      const anchor = t.fragment !== null && !isLineFragment(t.fragment) ? decodeAnchor(t.fragment) : null;
      const text = this.host.text();
      if (text === null) throw new Error("this document hasn't loaded yet");
      this.drawDoc(state, docPath, text, anchor, cleanups);
      return;
    }
    if (a.kind === "file" && embedKind(a) === "markdown") {
      const source = await this.source(a, signal);
      if (signal.aborted) return;
      if (!isMarpSource(source)) {
        const anchor = t.fragment !== null && !isLineFragment(t.fragment) ? decodeAnchor(t.fragment) : null;
        this.drawDoc(state, a.path, source, anchor, cleanups);
        return;
      }
    }
    this.drawCard(state, a, t.fragment, cleanups);
  }

  /** A markdown file's text (bounded), remembered by version briefly. */
  private async source(a: TargetInfo, signal: AbortSignal): Promise<string> {
    const key = `${a.path}\u0000${a.version}`;
    const hit = this.sources.get(key);
    if (hit !== undefined) return hit;
    const chunk = await fsFile(a.path, 0, SOURCE_MAX, signal);
    const text = new TextDecoder().decode(chunk.bytes);
    if (text.length <= CACHE_ENTRY_MAX) {
      this.sources.set(key, text);
      while (this.sources.size > CACHE_MAX) this.sources.delete(this.sources.keys().next().value ?? "");
    }
    return text;
  }

  /** A document's section (or footnote, or opening) through the reading
   *  renderer, in the popover. */
  private drawDoc(state: PreviewState, path: string, source: string, anchor: string | null, cleanups: (() => void)[]): void {
    const { text } = bodyText(source);
    const cx = documentContext(text);
    const hydrator = new Hydrator({ docPath: path, links: this.host.links, theme: this.host.theme() });
    cleanups.push(() => hydrator.destroy());
    const article = document.createElement("article");
    article.className = "md-doc";
    const env = { t: hydrator.target, cx };
    const note = anchor === null ? null : this.footnote(cx, anchor);
    let label = "";
    let found = true;
    if (note !== null) {
      renderBlocks(article, note.children, env);
      label = `note ${note.n}`;
    } else {
      const bounds = sectionBounds(cx.outline, anchor, text.length);
      found = bounds.found;
      const nodes = topLevel(cx)
        .filter((n) => n.from >= bounds.from && n.from < bounds.to)
        .slice(0, MAX_BLOCKS);
      for (const run of htmlRuns(nodes, (n) => htmlOfNode(n, cx))) renderRun(article, run, env);
      if (anchor !== null && found) label = cx.outline.find((h) => h.from === bounds.from)?.text ?? anchor;
    }
    markTasks(article);
    state.kind = "doc";
    state.path = path;
    state.name = basename(path);
    state.label = label;
    state.status = "ready";
    state.message = found ? (article.childNodes.length === 0 ? "Empty." : "") : `No heading “${anchor ?? ""}”: the opening.`;
    state.content.replaceChildren(article);
    // Typeset and paint once in the page (the reader's passes).
    hydrator.settle(Array.from(article.childNodes));
  }

  private footnote(cx: DocContext, anchor: string): { n: number; children: Parameters<typeof renderBlocks>[1] } | null {
    const wanted = new Set(anchorIds(anchor));
    for (const f of footnotesOf(cx)) if (wanted.has(`user-content-fn-${escapeHref(f.label)}`)) return f;
    return null;
  }

  /** Any other file: the compact body its embed card would show. */
  private drawCard(state: PreviewState, a: TargetInfo, fragment: string | null, cleanups: (() => void)[]): void {
    const slot = document.createElement("div");
    slot.className = "hp-card";
    // A gallery tile's body: a fixed, shorter frame (a PDF page, a picture,
    // a table's first rows) — a hover is a glance.
    const card: EmbedHandle = mountEmbed(slot, { path: a.path, info: a, fragment, alt: "", compact: true });
    cleanups.push(() => card.destroy());
    state.kind = "card";
    state.path = a.path;
    state.name = basename(a.path);
    state.label = fragmentLabel(parseEmbedFragment(fragment, a.path));
    state.status = "ready";
    state.message = "";
    state.content.replaceChildren(slot);
  }
}
