/**
 * Jupyter notebooks: the `GET /fs/notebook` client (cells paged by the
 * daemon, outputs already normalized and capped) and the pure pieces of the
 * notebook view — which output to draw, how markdown cells mark their math,
 * and how headings are addressed.
 */

import type { MarkedExtension, Tokens } from "marked";
import { api, ApiError } from "../net/api";

export interface NotebookOutput {
  output_type: string;
  /** stream: stdout | stderr. */
  name?: string;
  /** stream text. */
  text?: string;
  /** error */
  ename?: string;
  evalue?: string;
  traceback?: string;
  /** Cut at the daemon's text cap. */
  truncated?: boolean;
  /** display_data / execute_result: mime → payload (the richest drawable
   *  one plus text/plain). */
  data?: Record<string, string>;
  /** mime → byte size of a payload over the daemon's cap. */
  omitted?: Record<string, number>;
  execution_count?: number | null;
}

export interface NotebookCell {
  index: number;
  cell_type: "code" | "markdown" | "raw" | string;
  source: string;
  execution_count?: number | null;
  outputs?: NotebookOutput[];
  /** Markdown cell attachments: name → {data, omitted} (as for outputs). */
  attachments?: Record<string, { data: Record<string, string>; omitted: Record<string, number> }>;
  truncated?: boolean;
}

export interface NotebookPage {
  cells: NotebookCell[];
  offset: number;
  total: number;
  language: string | null;
  nbformat: number | null;
}

/** Server cap on cells per page. */
export const NOTEBOOK_PAGE_MAX = 100;

export async function fsNotebook(path: string, offset: number, limit: number): Promise<NotebookPage> {
  const q = new URLSearchParams({ path, offset: String(offset), limit: String(limit) });
  const res = await api(`/fs/notebook?${q.toString()}`);
  if (!res.ok) {
    let message = `request failed with status ${res.status}`;
    try {
      const body = (await res.json()) as { error?: string };
      if (body.error) message = body.error;
    } catch {
      // non-JSON error body; keep the generic message
    }
    throw new ApiError(res.status, message);
  }
  return (await res.json()) as NotebookPage;
}

/** Drawable mimes, richest first (the daemon keeps the same order). */
const DRAWABLE = [
  "image/png",
  "image/jpeg",
  "image/gif",
  "image/svg+xml",
  "text/html",
  "text/markdown",
  "text/latex",
  "text/plain",
] as const;
export type DrawableMime = (typeof DRAWABLE)[number];

/** The mime an output bundle is drawn as, or null when it has none. */
export function pickMime(data: Record<string, string> | undefined): DrawableMime | null {
  if (data === undefined) return null;
  return DRAWABLE.find((m) => typeof data[m] === "string") ?? null;
}

/** A base64 payload as a data URL (nbformat allows line breaks in it). */
export function base64Url(mime: string, b64: string): string {
  return `data:${mime};base64,${b64.replace(/\s+/g, "")}`;
}

/** An SVG document as a data URL for an <img> — the one context where an
 *  untrusted SVG can never run script or load anything. */
export function svgUrl(svg: string): string {
  const doc = /<svg\b[^>]*\bxmlns=/.test(svg) ? svg : svg.replace(/<svg\b/, '<svg xmlns="http://www.w3.org/2000/svg"');
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(doc)}`;
}

/** Jupyter's heading anchor: the text with spaces as dashes (`#My-Section`). */
export function headingSlug(text: string): string {
  return text.trim().replace(/\s+/g, "-");
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
}

/** A math placeholder: the source as text in a marked span that the view
 *  typesets after sanitizing (so KaTeX loads only when a cell has math). */
function mathSpan(source: string, display: boolean): string {
  return `<span class="nb-math" data-display="${display ? "1" : "0"}">${escapeHtml(source)}</span>`;
}

// Jupyter's (MathJax) delimiters: `$…$` inline, `$$…$$` and `\[…\]` display,
// `\(…\)` inline, and bare `\begin{env}…\end{env}` blocks. Unlike the chat
// dialect, inline `$` needs no word boundary after it — notebooks write
// `$x$-axis` — but it must not open or close on a space (`$5 and $10`).
const INLINE_DOLLAR = /^\$(?!\$)((?:\\.|[^\\\n$])*?[^\\\s$])\$(?!\d)/;
const DISPLAY_DOLLAR = /^\$\$([\s\S]+?)\$\$/;
const DISPLAY_BRACKET = /^\\\[([\s\S]+?)\\\]/;
const INLINE_PAREN = /^\\\(([\s\S]+?)\\\)/;
const ENVIRONMENT = /^\\begin\{([a-zA-Z*]+)\}[\s\S]*?\\end\{\1\}/;

interface MathToken extends Tokens.Generic {
  text: string;
  display: boolean;
}

function inlineStart(src: string): number | undefined {
  const i = src.search(/\$|\\\(|\\\[/);
  return i < 0 ? undefined : i;
}

/** Marked extension turning notebook math into placeholders. */
export const notebookMath: MarkedExtension = {
  extensions: [
    {
      name: "nbMathBlock",
      level: "block",
      start(src: string) {
        const i = src.search(/^ {0,3}(?:\$\$|\\\[|\\begin\{)/m);
        return i < 0 ? undefined : i;
      },
      tokenizer(src: string): MathToken | undefined {
        const lead = /^ {0,3}/.exec(src)?.[0] ?? "";
        const rest = src.slice(lead.length);
        const m = DISPLAY_DOLLAR.exec(rest) ?? DISPLAY_BRACKET.exec(rest) ?? ENVIRONMENT.exec(rest);
        if (m === null) return undefined;
        // A block only when nothing but whitespace follows on its line.
        const after = rest.slice(m[0].length);
        const eol = /^[ \t]*(?:\n|$)/.exec(after);
        if (eol === null) return undefined;
        const body = m[0].startsWith("\\begin") ? m[0] : m[1];
        return {
          type: "nbMathBlock",
          raw: lead + m[0] + eol[0],
          text: body.trim(),
          display: true,
        };
      },
      renderer(token) {
        return `<p class="nb-math-block">${mathSpan((token as MathToken).text, true)}</p>\n`;
      },
    },
    {
      name: "nbMathInline",
      level: "inline",
      start: inlineStart,
      tokenizer(src: string): MathToken | undefined {
        let m = DISPLAY_DOLLAR.exec(src);
        if (m !== null) return { type: "nbMathInline", raw: m[0], text: m[1].trim(), display: true };
        m = DISPLAY_BRACKET.exec(src);
        if (m !== null) return { type: "nbMathInline", raw: m[0], text: m[1].trim(), display: true };
        m = INLINE_PAREN.exec(src);
        if (m !== null) return { type: "nbMathInline", raw: m[0], text: m[1].trim(), display: false };
        m = INLINE_DOLLAR.exec(src);
        if (m !== null) return { type: "nbMathInline", raw: m[0], text: m[1], display: false };
        return undefined;
      },
      renderer(token) {
        const t = token as MathToken;
        return mathSpan(t.text, t.display);
      },
    },
  ],
};
