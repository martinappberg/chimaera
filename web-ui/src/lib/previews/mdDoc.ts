/**
 * Pure pieces of the markdown document view: the tolerant frontmatter reader
 * behind the reading view's properties panel, comrak's `data-sourcepos`
 * ranges (reading-mode line mapping), link-target classification, fragment
 * parsing, and the per-file mode memory. No DOM and no network here — the
 * mode memory takes its Storage — so every piece is unit-tested
 * (mdDoc.test.ts); `docLinks.ts` and `MarkdownView.svelte` do the wiring.
 */

import type { Reveal } from "../shared/reveal";

// --- frontmatter ---------------------------------------------------------------

export type FmValue =
  | { kind: "text"; text: string }
  | { kind: "list"; items: string[] }
  | { kind: "bool"; value: boolean }
  /** A nested map, a list of maps, a flow map: shown verbatim (dedented). */
  | { kind: "raw"; text: string };

export interface FmEntry {
  key: string;
  value: FmValue;
}

/** Bounds the work on a pathological block; real frontmatter is a few lines. */
const FM_MAX_LINES = 400;

/** The YAML between the fences, whether or not the daemon kept them. */
export function stripFences(raw: string): string {
  const lines = raw.replace(/\s+$/, "").split(/\r?\n/);
  if (lines.length > 0 && lines[0].trim() === "---") lines.shift();
  const last = lines.length - 1;
  if (last >= 0 && (lines[last].trim() === "---" || lines[last].trim() === "...")) lines.pop();
  return lines.join("\n");
}

/** How many source lines the block occupies, fences included — the
 *  properties panel stands in for lines 1..N of the file. */
export function frontmatterLineSpan(raw: string): number {
  const trimmed = raw.replace(/\s+$/, "");
  if (trimmed.trim() === "") return 2;
  const lines = trimmed.split(/\r?\n/);
  return lines[0].trim() === "---" ? Math.max(lines.length, 2) : lines.length + 2;
}

const KEY_LINE = /^([^\s#:\-[{][^:]*?|-[^\s:][^:]*?|"[^"]*"|'[^']*')\s*:(?:\s+(.*))?$/;
const LIST_ITEM = /^(\s*)-(?:\s+(.*))?$/;
const BLOCK_SCALAR = /^[|>][+-]?\d*$/;

function indentOf(line: string): number {
  return line.length - line.trimStart().length;
}

function unquote(s: string): string {
  const t = s.trim();
  if (t.length >= 2 && t.startsWith('"') && t.endsWith('"')) {
    try {
      return JSON.parse(t) as string;
    } catch {
      return t.slice(1, -1);
    }
  }
  if (t.length >= 2 && t.startsWith("'") && t.endsWith("'")) return t.slice(1, -1).replace(/''/g, "'");
  return t;
}

/** A plain scalar's trailing ` # comment` is not part of the value. */
function plainScalar(s: string): string {
  const t = s.trim();
  if (t.startsWith('"') || t.startsWith("'")) return unquote(t);
  const hash = t.search(/\s#/);
  return (hash >= 0 ? t.slice(0, hash) : t).trim();
}

/** `[a, "b, c", d]` → its items; null when it nests (shown raw instead). */
function flowList(s: string): string[] | null {
  const inner = s.trim().slice(1, -1);
  if (inner.trim() === "") return [];
  const items: string[] = [];
  let cur = "";
  let quote: string | null = null;
  for (const ch of inner) {
    if (quote !== null) {
      cur += ch;
      if (ch === quote) quote = null;
    } else if (ch === '"' || ch === "'") {
      quote = ch;
      cur += ch;
    } else if (ch === "[" || ch === "{") {
      return null;
    } else if (ch === ",") {
      items.push(unquote(cur));
      cur = "";
    } else {
      cur += ch;
    }
  }
  if (cur.trim() !== "") items.push(unquote(cur));
  return items;
}

function dedent(lines: string[]): string {
  const body = lines.filter((l) => l.trim() !== "");
  const min = body.length === 0 ? 0 : Math.min(...body.map(indentOf));
  return lines
    .map((l) => l.slice(Math.min(min, indentOf(l))))
    .join("\n")
    .replace(/\s+$/, "");
}

/**
 * A deliberately small YAML reader for the properties panel: top-level
 * `key: value`, `key: [a, b]`, `key:` followed by `- item` lines, `|`/`>`
 * block scalars, and true/false. A value it can't flatten (a nested map, a
 * list of maps) becomes a raw entry; a line that fits no top-level shape
 * returns null, and the panel shows the whole block as source. No YAML
 * library — the file stays the truth, this only has to read well.
 */
export function parseFrontmatter(raw: string): FmEntry[] | null {
  const lines = stripFences(raw).split(/\r?\n/).slice(0, FM_MAX_LINES);
  const out: FmEntry[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (line.trim() === "" || line.trimStart().startsWith("#")) {
      i++;
      continue;
    }
    if (indentOf(line) > 0) return null;
    const m = KEY_LINE.exec(line);
    if (m === null) return null;
    const key = unquote(m[1]);
    const rest = (m[2] ?? "").trim();
    i++;
    // The lines that belong to this key: indented ones, and (YAML allows it)
    // `- item` lines at the key's own column.
    const block: string[] = [];
    while (i < lines.length) {
      const l = lines[i];
      if (l.trim() !== "" && indentOf(l) === 0 && !LIST_ITEM.test(l)) break;
      block.push(l);
      i++;
    }
    while (block.length > 0 && block[block.length - 1].trim() === "") block.pop();
    out.push({ key, value: valueOf(rest, block) });
  }
  return out;
}

function valueOf(rest: string, block: string[]): FmValue {
  if (BLOCK_SCALAR.test(rest)) {
    const text = dedent(block);
    // `>` folds single newlines into spaces; a blank line stays a break.
    return {
      kind: "text",
      text: rest.startsWith(">") ? text.replace(/([^\n])\n(?=[^\n])/g, "$1 ") : text,
    };
  }
  if (rest === "") {
    const body = block.filter((l) => l.trim() !== "");
    if (body.length === 0) return { kind: "text", text: "" };
    const items = body.map((l) => LIST_ITEM.exec(l));
    const col = indentOf(body[0]);
    const flat = items.every(
      (it, n) => it !== null && indentOf(body[n]) === col && !KEY_LINE.test((it[2] ?? "").trim()),
    );
    if (flat) return { kind: "list", items: items.map((it) => plainScalar(it?.[2] ?? "")) };
    return { kind: "raw", text: dedent(block) };
  }
  if (rest.startsWith("[") && rest.endsWith("]") && block.length === 0) {
    const items = flowList(rest);
    return items === null ? { kind: "raw", text: rest } : { kind: "list", items };
  }
  if (rest.startsWith("{") || rest.startsWith("[") || rest.startsWith("&") || rest.startsWith("*")) {
    return { kind: "raw", text: [rest, ...block].join("\n").replace(/\s+$/, "") };
  }
  const scalar = plainScalar(rest);
  if (block.length > 0) {
    // A plain scalar continued on indented lines folds with spaces.
    const more = block.map((l) => l.trim()).filter((l) => l !== "");
    return { kind: "text", text: [scalar, ...more].join(" ") };
  }
  if (!/^["']/.test(rest)) {
    if (/^(true|false)$/i.test(scalar)) return { kind: "bool", value: scalar.toLowerCase() === "true" };
    if (scalar === "~" || scalar === "null") return { kind: "text", text: "" };
  }
  return { kind: "text", text: scalar };
}

// --- source positions ------------------------------------------------------------

/** A 1-based, inclusive line range of the original file. */
export interface SourceRange {
  start: number;
  end: number;
}

/** comrak's `data-sourcepos="startLine:startCol-endLine:endCol"`; null for
 *  anything else (an older daemon emits none). */
export function parseSourcepos(attr: string | null | undefined): SourceRange | null {
  const m = /^(\d+):(\d+)-(\d+):(\d+)$/.exec((attr ?? "").trim());
  if (m === null) return null;
  const start = Number(m[1]);
  if (start < 1) return null;
  return { start, end: Math.max(Number(m[3]), start) };
}

/** The lines a selection covers, from the ranges at its two ends (either
 *  may be unknown — a node outside any mapped block). */
export function spanLines(a: SourceRange | null, b: SourceRange | null): SourceRange | null {
  if (a === null) return b;
  if (b === null) return a;
  return { start: Math.min(a.start, b.start), end: Math.max(a.end, b.end) };
}

/**
 * Which block a reveal of `line` lands on, given every mapped block's range
 * in document order (null = unmapped): the tightest range holding the line
 * (the later one on a tie — deeper in the tree), else the first block
 * starting after it (the line was blank or inside something unmapped), else
 * the last block that ends before it. -1 when nothing is mapped.
 */
export function revealIndex(ranges: readonly (SourceRange | null)[], line: number): number {
  let best = -1;
  let bestSpan = Infinity;
  let after = -1;
  let before = -1;
  ranges.forEach((r, i) => {
    if (r === null) return;
    if (r.start <= line && line <= r.end) {
      const span = r.end - r.start;
      if (span <= bestSpan) {
        best = i;
        bestSpan = span;
      }
    } else if (r.start > line) {
      if (after === -1) after = i;
    } else {
      before = i;
    }
  });
  if (best !== -1) return best;
  return after !== -1 ? after : before;
}

// --- links and fragments -----------------------------------------------------------

/** `decodeURIComponent` that keeps the raw text on a malformed escape. */
export function safeDecode(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}

/** A fragment (`#My%20Note`, or without the `#`) as the anchor it names. */
export function decodeAnchor(fragment: string): string {
  return safeDecode(fragment.startsWith("#") ? fragment.slice(1) : fragment);
}

/** The element ids an anchor may name, in preference order. The daemon
 *  prefixes heading and footnote ids with `user-content-` (GitHub's rule, so
 *  a document can't clobber the app's own ids); a link written against the
 *  prefixed id, or a hand-written `id=`, still resolves, and so does a
 *  heading link whose case differs from the lowercase slug. */
export function anchorIds(anchor: string): string[] {
  if (anchor === "") return [];
  const ids = anchor.startsWith("user-content-") ? [anchor] : [`user-content-${anchor}`, anchor];
  const lower = anchor.toLowerCase();
  if (lower !== anchor && !lower.startsWith("user-content-")) ids.push(`user-content-${lower}`);
  return ids;
}

/** GitHub's line fragments: `L12`, `L12-L20`, `L12C3-L14C1` (the `-20`
 *  shorthand too). Null for anything else — a heading anchor. */
export function parseLineFragment(fragment: string): Reveal | null {
  const f = fragment.startsWith("#") ? fragment.slice(1) : fragment;
  const m = /^L(\d+)(?:C(\d+))?(?:-L?(\d+)(?:C\d+)?)?$/.exec(f);
  if (m === null) return null;
  const line = Number(m[1]);
  if (line < 1) return null;
  const r: Reveal = { line };
  if (m[2] !== undefined) r.col = Number(m[2]);
  if (m[3] !== undefined && Number(m[3]) > line) r.endLine = Number(m[3]);
  return r;
}

export type DocHref =
  /** `#heading`, `#fn-1`: a place in this same document. */
  | { kind: "anchor"; anchor: string }
  /** `#L12-L20` in this same document. */
  | { kind: "lines"; reveal: Reveal }
  | { kind: "web"; url: string }
  /** mailto:/tel: — the browser's to handle; they can't navigate the app. */
  | { kind: "native" }
  /** A file path, decoded: document-relative, or absolute; fragment without `#`. */
  | { kind: "path"; path: string; fragment: string | null }
  /** Nothing to follow: an empty href, or a scheme we never hand onward. */
  | { kind: "none" };

/** What a link in a markdown document points at. The caller still passes a
 *  "web" result through the shared URL wall (`webUrl`) before opening it. */
export function classifyHref(href: string): DocHref {
  const h = href.trim();
  if (h === "") return { kind: "none" };
  if (h.startsWith("#")) {
    const reveal = parseLineFragment(h);
    if (reveal !== null) return { kind: "lines", reveal };
    const anchor = decodeAnchor(h);
    return anchor === "" ? { kind: "none" } : { kind: "anchor", anchor };
  }
  if (/^(mailto|tel):/i.test(h)) return { kind: "native" };
  if (/^(https?:\/\/|www\.)/i.test(h)) return { kind: "web", url: h };
  if (/^file:/i.test(h)) {
    try {
      const u = new URL(h);
      if (u.hostname !== "" && u.hostname !== "localhost") return { kind: "none" };
      const fragment = u.hash.length > 1 ? u.hash.slice(1) : null;
      return { kind: "path", path: safeDecode(u.pathname), fragment };
    } catch {
      return { kind: "none" };
    }
  }
  if (/^[a-z][a-z0-9+.-]*:/i.test(h)) return { kind: "none" };
  const hash = h.indexOf("#");
  const beforeHash = hash >= 0 ? h.slice(0, hash) : h;
  const fragment = hash >= 0 && hash < h.length - 1 ? h.slice(hash + 1) : null;
  const q = beforeHash.indexOf("?");
  const path = safeDecode(q >= 0 ? beforeHash.slice(0, q) : beforeHash);
  if (path === "") {
    if (fragment === null) return { kind: "none" };
    return classifyHref(`#${fragment}`);
  }
  return { kind: "path", path, fragment };
}

// --- mode memory -------------------------------------------------------------------

export type MdMode = "live" | "reading" | "source";

const MODES: readonly string[] = ["live", "reading", "source"];

export function isMdMode(v: unknown): v is MdMode {
  return typeof v === "string" && MODES.includes(v);
}

export interface ModeMemory {
  get(path: string): MdMode | null;
  set(path: string, mode: MdMode): void;
}

type ModeStore = Pick<Storage, "getItem" | "setItem">;

/**
 * The last mode picked per file: a least-recently-used list of `[path,
 * mode]` pairs (newest last) under one key, capped at `cap`. Storage can be
 * missing or throw anywhere (private windows, a full quota, a sandboxed
 * webview): every access is guarded, and a failure only forgets.
 */
export function createModeMemory(
  storage: () => ModeStore | null,
  key = "chimaera.markdownModes",
  cap = 300,
): ModeMemory {
  function load(): [string, MdMode][] {
    try {
      const raw = storage()?.getItem(key) ?? null;
      if (raw === null) return [];
      const data: unknown = JSON.parse(raw);
      if (!Array.isArray(data)) return [];
      return data.filter(
        (e): e is [string, MdMode] =>
          Array.isArray(e) && e.length === 2 && typeof e[0] === "string" && isMdMode(e[1]),
      );
    } catch {
      return [];
    }
  }
  function save(list: [string, MdMode][]): void {
    try {
      storage()?.setItem(key, JSON.stringify(list.slice(-cap)));
    } catch {
      // quota / unavailable: the memory is a convenience
    }
  }
  return {
    get(path) {
      const list = load();
      const i = list.findIndex((e) => e[0] === path);
      if (i === -1) return null;
      const entry = list[i];
      if (i !== list.length - 1) {
        list.splice(i, 1);
        list.push(entry);
        save(list);
      }
      return entry[1];
    },
    set(path, mode) {
      const list = load().filter((e) => e[0] !== path);
      list.push([path, mode]);
      save(list);
    },
  };
}
