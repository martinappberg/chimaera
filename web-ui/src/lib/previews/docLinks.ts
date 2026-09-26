/**
 * Following a link out of a markdown document, for the reading view and the
 * live editor alike. A file link resolves against the document's folder and
 * is confirmed by the daemon (`fs/validate`) before anything opens, so a dead
 * link says so in place instead of opening an empty tab. `#L12` opens the
 * file at the line (a reveal); `other.md#heading` opens the document and
 * then scrolls to the heading — a pending anchor, keyed by path, that the
 * target's view consumes once it has rendered. Same-document anchors never
 * touch `location.hash`: several documents share one page, and the hash is
 * the app's bootstrap channel.
 */

import { writable } from "svelte/store";
import { dirname, fsMarkdown, fsValidate, resolveDocPath, viewKindFor } from "./files";
import { openPath } from "../shared/openPath";
import { requestReveal, type Reveal } from "../shared/reveal";
import { activateUrl, webUrl } from "../shared/urlOpen";
import { anchorSourceLine } from "./doc/render";
import {
  anchorIds,
  classifyHref,
  decodeAnchor,
  parseLineFragment,
  parseSourcepos,
} from "./mdDoc";

// --- pending anchors -------------------------------------------------------------

/** Anchors waiting for their document to render, newest last. Small: one
 *  per link click, consumed within a render, so a few stragglers (a target
 *  that failed to render) are all it ever holds. */
const pending = new Map<string, string>();
const PENDING_CAP = 16;

/** Bumped on every request, so a view already showing the target re-checks. */
export const anchorRequests = writable(0);

export function requestAnchor(path: string, anchor: string): void {
  pending.delete(path);
  pending.set(path, anchor);
  while (pending.size > PENDING_CAP) {
    const oldest = pending.keys().next().value;
    if (oldest === undefined) break;
    pending.delete(oldest);
  }
  anchorRequests.update((n) => n + 1);
}

/** Consume the pending anchor for `path`, if any. */
export function takeAnchor(path: string): string | null {
  const a = pending.get(path);
  if (a === undefined) return null;
  pending.delete(path);
  return a;
}

// --- anchors in a rendered document ------------------------------------------------

/** The element an anchor names inside `root` (scoped: every open document
 *  shares the page, so a global id lookup could land in another pane). */
export function findAnchor(root: ParentNode, anchor: string): HTMLElement | null {
  for (const id of anchorIds(anchor)) {
    const el = root.querySelector<HTMLElement>(`[id="${CSS.escape(id)}"]`);
    if (el !== null) return el;
  }
  return null;
}

/** The source line an anchor's block starts on, read from a render's HTML
 *  (parsed inert: DOMParser documents run no script and load nothing). */
export function anchorLine(html: string, anchor: string): number | null {
  const doc = new DOMParser().parseFromString(html, "text/html");
  const el = findAnchor(doc, anchor);
  const block = el?.closest("[data-sourcepos]") ?? null;
  return parseSourcepos(block?.getAttribute("data-sourcepos"))?.start ?? null;
}

/**
 * The editor modes have no rendered ids: map the anchor to its line and
 * hand the editor a reveal. With the document's current text (the editor's
 * buffer, unsaved edits included) the client renderer's ids answer — the
 * same slugs the reading view and GitHub produce; without it, the daemon's
 * render of the file on disk does.
 */
export async function revealAnchorInSource(
  path: string,
  anchor: string,
  text: string | null = null,
): Promise<boolean> {
  let line: number | null;
  if (text !== null) {
    line = anchorSourceLine(text, anchor);
  } else {
    let html: string;
    try {
      html = (await fsMarkdown(path)).html;
    } catch {
      return false;
    }
    line = anchorLine(html, anchor);
  }
  if (line === null) return false;
  requestReveal(path, { line });
  return true;
}

// --- following --------------------------------------------------------------------

/** Where the workspace-relative fallback for a `/docs/x.md` link comes from. */
export interface LinkContext {
  wsRoot: string | null;
  /** Enables the daemon's unique-basename fallback; null when unknown. */
  workspaceId: string | null;
}

/** What a surface provides to follow links out of its document. */
export interface DocLinkHost extends LinkContext {
  /** The document the link is in (absolute daemon path). */
  docPath: string;
  /** Scroll this document to an anchor; false when it has none such. */
  toAnchor(anchor: string): boolean | Promise<boolean>;
  /** Scroll this document to a line range. */
  toLines(r: Reveal): void;
  /** A brief message where the gesture happened. */
  hint(text: string): void;
}

/** Whether `href` is something `followDocHref` acts on (the live editor
 *  consumes the gesture only then; mailto:/tel: stay the browser's). */
export function isFollowable(href: string): boolean {
  const k = classifyHref(href).kind;
  return k !== "none" && k !== "native";
}

/** Follow `href` from `host`'s document. `split` opens beside it. */
export async function followDocHref(href: string, split: boolean, host: DocLinkHost): Promise<void> {
  const t = classifyHref(href);
  switch (t.kind) {
    case "web": {
      const url = webUrl(t.url);
      if (url !== null) activateUrl(url, split);
      return;
    }
    case "anchor":
      if (!(await host.toAnchor(t.anchor))) host.hint("no such heading in this document");
      return;
    case "lines":
      host.toLines(t.reveal);
      return;
    case "path":
      await followPath(t.path, t.fragment, split, host);
      return;
    default:
      return;
  }
}

async function sameDocument(fragment: string | null, host: DocLinkHost): Promise<void> {
  if (fragment === null) return;
  const reveal = parseLineFragment(fragment);
  if (reveal !== null) {
    host.toLines(reveal);
    return;
  }
  const anchor = decodeAnchor(fragment);
  if (anchor !== "" && !(await host.toAnchor(anchor))) host.hint("no such heading in this document");
}

async function followPath(
  rel: string,
  fragment: string | null,
  split: boolean,
  host: DocLinkHost,
): Promise<void> {
  const { docPath } = host;
  if (resolveDocPath(docPath, rel) === docPath) {
    await sameDocument(fragment, host);
    return;
  }
  // As written first (the daemon joins it to the document's folder and
  // canonicalizes); a root-relative `/docs/x.md` — GitHub's reading — also
  // against the workspace root.
  const candidates = [rel];
  if (rel.startsWith("/") && host.wsRoot !== null && host.wsRoot !== "/") {
    candidates.push(`${host.wsRoot.replace(/\/+$/, "")}${rel}`);
  }
  let res: Awaited<ReturnType<typeof fsValidate>>;
  try {
    res = await fsValidate(candidates, dirname(docPath), host.workspaceId);
  } catch {
    host.hint("couldn't check this link — daemon unreachable");
    return;
  }
  const hit = candidates.map((c) => res.valid[c]).find((v) => v !== undefined);
  if (hit === undefined) {
    host.hint(`not found: ${rel}`);
    return;
  }
  if (hit.path === docPath) {
    await sameDocument(fragment, host);
    return;
  }
  const isFile = hit.kind === "file";
  const reveal = isFile && fragment !== null ? parseLineFragment(fragment) : null;
  const anchor = fragment !== null && reveal === null ? decodeAnchor(fragment) : "";
  const anchored = isFile && anchor !== "" && viewKindFor(hit.path) === "markdown";
  if (anchored) requestAnchor(hit.path, anchor);
  const opened = openPath(hit.path, hit.kind, reveal === null ? { split } : { split, reveal });
  if (!opened) {
    if (anchored) takeAnchor(hit.path);
    host.hint("files can't be opened from here");
  }
}

// --- the inline hint --------------------------------------------------------------

const hints = new WeakMap<HTMLElement, { el: HTMLElement; timer: ReturnType<typeof setTimeout> }>();
const HINT_MS = 2200;

/** A brief status line near the gesture, inside `host` (a positioned box).
 *  Built with textContent only — the text quotes a document's own href. */
export function showLinkHint(host: HTMLElement, clientX: number, clientY: number, text: string): void {
  const prev = hints.get(host);
  if (prev !== undefined) {
    clearTimeout(prev.timer);
    prev.el.remove();
  }
  const el = document.createElement("div");
  el.className = "md-link-hint";
  el.setAttribute("role", "status");
  el.textContent = text;
  const rect = host.getBoundingClientRect();
  const x = Math.max(8, Math.min(clientX - rect.left - 12, rect.width - 260));
  const y = Math.max(8, Math.min(clientY - rect.top + 16, rect.height - 40));
  el.style.left = `${x}px`;
  el.style.top = `${y}px`;
  host.append(el);
  const timer = setTimeout(() => {
    el.remove();
    if (hints.get(host)?.el === el) hints.delete(host);
  }, HINT_MS);
  hints.set(host, { el, timer });
}
