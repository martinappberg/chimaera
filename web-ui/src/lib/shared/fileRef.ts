/**
 * File references in agent and terminal text: the one parser behind every
 * surface that turns a mention into a link (chat prose, code spans, markdown
 * link targets, user messages, terminal output).
 *
 * It only proposes CANDIDATES. Whether one links is the daemon's call
 * (POST /fs/validate resolves it against the right directories and confirms
 * it exists), so these rules bound daemon traffic and decide what the
 * underline covers, never what is true. A candidate is always a clean path:
 * the daemon never sees a line suffix, an anchor, a wrapper or a scheme.
 *
 * The shapes agents are told to write, and the ones they write anyway:
 *   src/x.rs:12   src/x.rs:12:3   src/x.rs:12-20   src/x.ts(12,3)
 *   src/x.rs#L12  src/x.rs#L12-L20  src/x.rs#L12C3  file:///abs/x.rs
 *   @src/x.ts (a Claude mention)  a/src/x.rs (a diff side: sent as written,
 *   the daemon strips the prefix when the path misses)  …/figs/plot.png (a
 *   TUI abbreviation: the tail is sent, the daemon suffix-matches it).
 *
 * Wrappers come off either side ((), [], {}, <>, quotes, backticks, bold
 * asterisks, full-width brackets and quotes), and trailing sentence
 * punctuation (ASCII and full-width) too. A closing bracket stays when it is
 * balanced inside the name, so `Screenshot (1).png` survives. Spaces belong
 * to a path only when the text is delimited: a whole link target or a whole
 * code span.
 */

import type { Reveal } from "./reveal";

/** A parsed reference. Lines and columns are 1-based. */
export interface FileRef {
  /** The path candidate as the daemon should see it. */
  path: string;
  line?: number;
  col?: number;
  /** Last line of a range (`:12-20`, `#L12-L20`); always greater than `line`. */
  endLine?: number;
}

/** A reference found inside a longer string. */
export interface FoundRef {
  ref: FileRef;
  /** [start, end) of the text a link covers: the reference and its line
   *  suffix, without wrappers or trailing punctuation. */
  start: number;
  end: number;
}

export interface FileRefOptions {
  /** The whole text is one reference (a link target, a whole code span):
   *  spaces are part of the path and `?query` is dropped. */
  delimited?: boolean;
  /** Also admit a single-segment name with no extension (`crates`,
   *  `justfile`). The terminal's per-line hover path only: prose is never
   *  mass-validated on the daemon (it runs on shared login nodes). */
  bare?: boolean;
}

/** The daemon's per-candidate ceiling, in UTF-8 bytes. */
export const FILE_REF_MAX_BYTES = 1024;

/** Stripped from the left of a reference. */
const OPENERS = new Set([..."\"'`*([{<“‘「『【〈《（［｛"]);
/** Stripped from the right (brackets only while unbalanced). */
const CLOSERS = new Set([..."\"'`*)]}>”’」』】〉》）］｝"]);
/** A closer's opener: a balanced pair inside the name is part of it. */
const PAIRS: Record<string, string> = {
  ")": "(",
  "]": "[",
  "}": "{",
  "）": "（",
  "］": "［",
  "｝": "｛",
};
/** Sentence punctuation that trails a mention and never ends a path. */
const TRAIL = new Set([...".,;:!?…。，、；：！？"]);

/** `name.ext`: a letter-led extension. Long ones are real (`.safetensors`). */
const NAME_EXT_RE = /^(.+)\.([\p{L}][\p{L}\p{N}_]{0,15})$/u;
/** Characters no path candidate carries: controls, globs, shell syntax,
 *  backslashes (Windows paths never resolve on the daemon). */
const FORBIDDEN_RE = /[\u0000-\u001f\u007f\\*?"<>|`{}$]/;
/** `me@example.com`: an address, not `icon@2x.png`. */
const EMAIL_RE =
  /^[^@/\s]+@[^@/\s]+\.(?:com|org|net|edu|gov|io|dev|ai|co|uk|de|fr|jp|cn|ch|info|me|app)$/i;

/** Extensionless names worth a link anywhere (not only on an `ls` line). */
const KNOWN_BARE = new Set([
  "Makefile",
  "makefile",
  "GNUmakefile",
  "Dockerfile",
  "Containerfile",
  "Justfile",
  "justfile",
  "Gemfile",
  "Rakefile",
  "Procfile",
  "Vagrantfile",
  "Jenkinsfile",
  "Snakefile",
  "Pipfile",
  "Brewfile",
  "LICENSE",
  "COPYING",
  "README",
  "NOTICE",
  "CHANGELOG",
  "AUTHORS",
  "CODEOWNERS",
]);

/** A spaced code span that starts with one of these is a command, not a
 *  path; its path-like words are still offered one by one. */
const COMMANDS = new Set([
  "cat",
  "cd",
  "cp",
  "mv",
  "rm",
  "ls",
  "less",
  "head",
  "tail",
  "vim",
  "vi",
  "nvim",
  "nano",
  "code",
  "open",
  "python",
  "python3",
  "node",
  "bash",
  "sh",
  "zsh",
  "source",
  "git",
  "cargo",
  "npm",
  "npx",
  "just",
  "make",
  "touch",
  "mkdir",
  "chmod",
  "grep",
  "rg",
  "find",
  "sed",
  "awk",
  "diff",
  "sbatch",
  "srun",
  "Rscript",
  "uv",
  "pip",
]);

function utf8Bytes(s: string): number {
  // Every UTF-16 unit is at most 3 UTF-8 bytes; skip the encode when that
  // bound already fits.
  if (s.length * 3 <= FILE_REF_MAX_BYTES) return s.length;
  return new TextEncoder().encode(s).length;
}

function count(s: string, ch: string): number {
  let n = 0;
  for (const c of s) if (c === ch) n += 1;
  return n;
}

/** Whether a clean path is worth asking the daemon about. */
function qualifies(path: string, bare: boolean): boolean {
  if (path.length === 0 || utf8Bytes(path) > FILE_REF_MAX_BYTES) return false;
  if (!/[\p{L}\p{N}]/u.test(path)) return false;
  if (FORBIDDEN_RE.test(path) || path.includes(":")) return false;
  if (path.startsWith("-") || path.startsWith("//")) return false;
  if (/\s/.test(path)) {
    // Only a delimited candidate gets here. Flags and a leading command
    // word mark a command line; its words are offered separately.
    const words = path.split(/\s+/);
    if (words.some((w) => w.startsWith("-")) || COMMANDS.has(words[0])) return false;
  }
  if (path.includes("/")) {
    // Dates and fractions (2024/01/02, 1/2) are not paths.
    return !path.split("/").every((seg) => seg === "" || /^\d+$/.test(seg));
  }
  if (/^\.[\p{L}_]/u.test(path)) return true; // .env, .claude, .github
  const m = NAME_EXT_RE.exec(path);
  if (m !== null) {
    // e.g, i.e, a.m: two single letters are an abbreviation.
    if (/^\p{L}$/u.test(m[1]) && m[2].length === 1) return false;
    return !EMAIL_RE.test(path);
  }
  if (KNOWN_BARE.has(path)) return true;
  return bare && /\p{L}/u.test(path);
}

/** `#L12`, `#L12-L20`, `#L12C3`, `#L12C3-L20C1`. */
const LINE_ANCHOR_RE = /^L(\d{1,7})(?:C(\d{1,7}))?(?:-L?(\d{1,7})(?:C\d{1,7})?)?$/;
/** `:12`, `:12:3`, `:12-20`, then grep's `:content` (kept out of the link). */
const LINE_SUFFIX_RE = /^:(\d{1,7})(?::(\d{1,7}))?(?:-(\d{1,7}))?(:.*)?$/s;
/** tsc / MSVC: `x.ts(12,3)`. */
const PAREN_SUFFIX_RE = /^(.*[^\s(])\((\d{1,7})(?:,(\d{1,7}))?\)$/s;
/** A tool-call wrapper: `Read(src/x.ts)`, `Update(…)`. */
const CALL_RE = /^([\p{L}\p{N}_.]+)\((.+)\)$/su;

function withLines(path: string, line?: string, col?: string, endLine?: string): FileRef {
  const ref: FileRef = { path };
  const l = line !== undefined ? Number.parseInt(line, 10) : 0;
  if (l >= 1) {
    ref.line = l;
    const c = col !== undefined ? Number.parseInt(col, 10) : 0;
    if (c >= 1) ref.col = c;
    const e = endLine !== undefined ? Number.parseInt(endLine, 10) : 0;
    if (e > l) ref.endLine = e;
  }
  return ref;
}

/**
 * Find the reference in `text`, one token (or, `delimited`, one whole link
 * target / code span). Offsets are into `text`. Null when it is not a
 * plausible path.
 */
export function findFileRef(text: string, opts: FileRefOptions = {}): FoundRef | null {
  const delimited = opts.delimited === true;
  const bare = opts.bare === true;
  let s = 0;
  let e = text.length;
  if (delimited) {
    while (s < e && /\s/.test(text[s])) s += 1;
    while (e > s && /\s/.test(text[e - 1])) e -= 1;
  } else if (/\s/.test(text)) {
    return null;
  }

  // Peel wrappers and punctuation, then a tool-call wrapper, until stable.
  for (let round = 0; round < 4; round++) {
    for (let guard = 0; guard < 24 && s < e; guard++) {
      const first = text[s];
      const last = text[e - 1];
      if (OPENERS.has(first)) {
        s += 1;
        continue;
      }
      if (TRAIL.has(last)) {
        e -= 1;
        continue;
      }
      if (CLOSERS.has(last)) {
        const open = PAIRS[last];
        if (open !== undefined) {
          const body = text.slice(s, e);
          if (count(body, last) <= count(body, open)) break; // balanced: part of the name
        }
        e -= 1;
        continue;
      }
      break;
    }
    const call = CALL_RE.exec(text.slice(s, e));
    // `x.ts(12,3)` is a line suffix, not a call.
    if (call === null || /^\d+(?:,\d+)?$/.test(call[2])) break;
    s += call[1].length + 1;
    e -= 1;
  }
  if (s >= e) return null;

  let from = s; // where the path text starts in `text`
  let end = e; // where the link ends in `text`
  if (text[from] === "@" && text[from + 1] !== "@") from += 1;
  let p = text.slice(from, e);

  let url = false;
  if (/^file:/i.test(p)) {
    const m = /^file:(?:\/\/([^/]*))?(\/.*)?$/is.exec(p);
    if (m === null || m[2] === undefined) return null;
    const host = (m[1] ?? "").toLowerCase();
    if (host !== "" && host !== "localhost") return null;
    from += p.length - m[2].length;
    p = m[2];
    url = true;
  } else if (/^[a-z][a-z0-9+.-]*:\/\//i.test(p)) {
    return null; // another scheme: a web link, never a path
  }

  let anchor: string | null = null;
  const hash = p.indexOf("#");
  if (hash >= 0) {
    anchor = p.slice(hash + 1);
    p = p.slice(0, hash);
  }
  if (delimited || url) {
    const q = p.indexOf("?");
    if (q >= 0) p = p.slice(0, q);
  }

  let ref: FileRef;
  const lineAnchor = anchor !== null ? LINE_ANCHOR_RE.exec(anchor) : null;
  const colon = p.indexOf(":");
  if (colon >= 0) {
    const m = LINE_SUFFIX_RE.exec(p.slice(colon));
    // A colon that is not a line suffix: a scheme, `std::fs`, `host:path`.
    if (m === null) return null;
    // grep's `path:12:content`: the link stops after the numbers. (Only
    // token text is still `text.slice(from, e)` here.)
    if (m[4] !== undefined && !delimited && !url && hash < 0) end = from + p.length - m[4].length;
    ref = withLines(p.slice(0, colon), m[1], m[2], m[3]);
  } else if (lineAnchor !== null) {
    ref = withLines(p, lineAnchor[1], lineAnchor[2], lineAnchor[3]);
  } else {
    const m = PAREN_SUFFIX_RE.exec(p);
    ref = m !== null ? withLines(m[1], m[2], m[3]) : { path: p };
  }

  let path = ref.path;
  if (/%[0-9A-Fa-f]{2}/.test(path)) {
    try {
      path = decodeURIComponent(path);
    } catch {
      // A malformed escape: keep the text as written.
    }
  }
  // A TUI abbreviation (`…/figs/plot.png`, `src/.../x.rs`): send the tail,
  // dropping a segment the ellipsis cut into.
  const ell = Math.max(path.lastIndexOf("…"), path.lastIndexOf("..."));
  if (ell >= 0) {
    let tail = path.slice(ell + (path[ell] === "…" ? 1 : 3));
    if (tail.startsWith("/")) {
      tail = tail.slice(1);
    } else {
      const slash = tail.indexOf("/");
      if (slash < 0) return null;
      tail = tail.slice(slash + 1);
    }
    path = tail;
  }
  if (!qualifies(path, bare)) return null;
  ref.path = path;
  return { ref, start: s, end };
}

/** Parse one reference (a token, or `delimited` a whole target). */
export function parseFileRef(text: string, opts: FileRefOptions = {}): FileRef | null {
  return findFileRef(text, opts)?.ref ?? null;
}

/** Token boundaries: whitespace, `--flag=value`, table pipes, and the
 *  full-width separators and corner quotes CJK prose uses without spaces
 *  (full-width parentheses stay: file names carry them, `資料（1）.pdf`). */
const TOKEN_RE = /[^\s=|、，；。：「」『』【】〈〉《》]+/gu;
/** Python tracebacks: `File "x.py", line 12` carries its line outside. */
const TRACEBACK_LINE_RE = /^["']?,\s*line\s+(\d{1,7})\b/;

/** Every reference in a run of prose or a terminal line, in order. */
export function extractFileRefs(text: string, opts: { bare?: boolean } = {}): FoundRef[] {
  const out: FoundRef[] = [];
  for (const m of text.matchAll(TOKEN_RE)) {
    const found = findFileRef(m[0], { bare: opts.bare });
    if (found === null) continue;
    const start = m.index + found.start;
    const end = m.index + found.end;
    if (found.ref.line === undefined) {
      const tb = TRACEBACK_LINE_RE.exec(text.slice(end, end + 40));
      if (tb !== null) Object.assign(found.ref, withLines(found.ref.path, tb[1]));
    }
    out.push({ ref: found.ref, start, end });
  }
  return out;
}

/** The spot to reveal once the file opens, when the reference names one. */
export function revealOf(ref: FileRef | null | undefined): Reveal | undefined {
  if (ref?.line === undefined) return undefined;
  const r: Reveal = { line: ref.line };
  if (ref.endLine !== undefined) r.endLine = ref.endLine;
  if (ref.col !== undefined) r.col = ref.col;
  return r;
}

/** Where a session's relative references resolve (App's one answer, shared
 *  by the terminal link provider and chat). */
export interface LinkContext {
  /** The live working directory (cwd_current), else the spawn directory. */
  cwd: string | null;
  /** The directory the session started in, when it differs from `cwd`:
   *  scrollback and older messages were written from there. */
  spawnCwd?: string | null;
  /** The workspace root. */
  root: string | null;
  /** Enables the daemon's workspace-index fallbacks (unique basename,
   *  unique path suffix). Null degrades to base-only resolution. */
  workspaceId: string | null;
}

/**
 * The absolute directories to resolve `path` against, in order (the first
 * is the request's `base`, the rest its `bases`). An absolute or `~` path
 * needs none, but the API requires a base. `./` and `../` are relative to
 * where the session is, nothing else. Empty when nothing is known.
 */
export function resolveBases(ctx: LinkContext, path: string): string[] {
  if (path.startsWith("/") || path.startsWith("~")) {
    const any = ctx.cwd ?? ctx.spawnCwd ?? ctx.root ?? "/";
    return [any];
  }
  const here = ctx.cwd ?? ctx.spawnCwd ?? null;
  if (path.startsWith("./") || path.startsWith("../")) return here !== null ? [here] : [];
  const out: string[] = [];
  for (const b of [ctx.cwd, ctx.spawnCwd, ctx.root]) {
    if (b != null && b.startsWith("/") && !out.includes(b)) out.push(b);
  }
  return out;
}
