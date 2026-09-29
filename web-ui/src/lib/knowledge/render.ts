/**
 * An entry's body, as written: the reader fetches the file its span names
 * (cached for the life of one snapshot — a new snapshot means its files
 * changed) and draws that slice through the reading renderer (the same
 * blocks, equations, callouts and tables the markdown preview draws).
 */
import { fsFile } from "../previews/files";
import type { LinkContext } from "../previews/docLinks";
import { bodyText, documentContext, htmlOfNode, htmlRuns } from "../previews/doc/model";
import { renderRun, topLevel } from "../previews/doc/render";
import { Hydrator, markTasks } from "../previews/doc/reader";
import { sliceLines } from "../previews/doc/slice";

/** The provider's own file cap; a bigger file isn't one it read. */
const FILE_MAX = 2 * 1024 * 1024;
/** Files held at once (a snapshot's topic files, decisions, learnings…). */
const CACHE_FILES = 32;

const cache = new Map<string, Promise<string>>();
let cacheFor: unknown = null;

/** The file's text, once per snapshot (`owner` is the snapshot object: a
 *  different one empties the cache). */
export function entrySource(path: string, owner: unknown): Promise<string> {
  if (owner !== cacheFor) {
    cache.clear();
    cacheFor = owner;
  }
  const hit = cache.get(path);
  if (hit !== undefined) return hit;
  const p = fsFile(path, 0, FILE_MAX).then((chunk) => new TextDecoder().decode(chunk.bytes));
  cache.set(path, p);
  p.catch(() => cache.delete(path));
  while (cache.size > CACHE_FILES) cache.delete(cache.keys().next().value ?? "");
  return p;
}

/** The markdown of lines `line`..`end` (`end` below `line`: to the end),
 *  without the heading line when `dropHeading` and it is one — the reader
 *  shows the title itself. */
export function entryMarkdown(source: string, line: number, end: number, dropHeading: boolean): string {
  const text = sliceLines(source, line, end);
  if (!dropHeading) return text;
  const nl = text.indexOf("\n");
  const first = nl < 0 ? text : text.slice(0, nl);
  return /^#{1,6}\s/.test(first) ? (nl < 0 ? "" : text.slice(nl + 1)) : text;
}

export interface DrawOptions {
  /** The file's absolute path (relative links and images resolve beside it). */
  docPath: string;
  markdown: string;
  links: () => LinkContext;
  theme: "light" | "dark";
  onLayout?: () => void;
}

/** Draw `markdown` into `into` as a reading-view article; returns the
 *  teardown. */
export function drawMarkdown(into: HTMLElement, o: DrawOptions): () => void {
  const { text } = bodyText(o.markdown);
  const cx = documentContext(text);
  const hydrator = new Hydrator({ docPath: o.docPath, links: o.links, theme: o.theme, onLayout: o.onLayout });
  const article = document.createElement("article");
  article.className = "md-doc";
  const env = { t: hydrator.target, cx };
  for (const run of htmlRuns(topLevel(cx), (n) => htmlOfNode(n, cx))) renderRun(article, run, env);
  markTasks(article);
  into.replaceChildren(article);
  hydrator.settle(Array.from(article.childNodes));
  return () => hydrator.destroy();
}
