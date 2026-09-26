/**
 * What counts as a turn's "made this turn" file, and the cheap honest
 * signal for the ones no edit tool reported. Pure (artifacts.test.ts): the
 * chat reducer runs it at every turn end, replay included.
 *
 * Edit tools name what they wrote (a tool row's `locations`). Files written
 * by a shell command — a plot saved by a script, an HTML report, a PDF from
 * a notebook run — are named nowhere structured. What the turn does say is
 * text: the command line, its output ("saved figs/umap.png"), the agent's
 * own summary. So the reducer lists the artifact-shaped paths those texts
 * mention, and the gallery keeps only the ones the daemon confirms exist
 * AND were modified inside the turn's time window (the journal stamps both
 * ends; the daemon stamps the mtimes — one clock). A file merely `cat`-ed
 * or mentioned in passing is older than the turn and stays out.
 */

import { viewKindFor } from "../previews/files";
import { isMissing, splitTarget, type TargetResult } from "../shared/embed/embed";

/** What a turn's file is FOR, which decides how the gallery shows it:
 *  a `visual` is looked at (a figure, a rendered report, a PDF, a clip) and
 *  gets a tile up front; a `document` is opened (a markdown note, a table,
 *  a notebook, a deck) and gets a one-line chip — its tile only on request. */
export type ArtifactShape = "visual" | "document";

/** The shape of a path's file, or null when it is not an artifact. Decided
 *  from the workbench's own view kinds (one extension table, so a format
 *  the previews learn is one the gallery knows). Source code, logs and
 *  configs are not artifacts: an edit's diff is already in the tool card. */
export function artifactShape(path: string): ArtifactShape | null {
  switch (viewKindFor(path)) {
    case "image":
    case "html":
    case "pdf":
    case "video":
    case "audio":
      return "visual";
    case "markdown":
    case "table":
    case "xlsx":
    case "notebook":
    case "docx":
    case "pptx":
      return "document";
    default:
      return null;
  }
}

/** Whether a path is the kind of file a turn's gallery shows. */
export function isArtifactPath(path: string): boolean {
  return artifactShape(path) !== null;
}

/** A URL (`https://…`, `file:`) or a protocol-relative `//host` — never a
 *  local path. */
function isUrlLike(token: string): boolean {
  return /^[a-z][a-z0-9+.-]*:/i.test(token) || token.startsWith("//");
}

/** A whitespace/quote/bracket-delimited token. */
const TOKEN = /[^\s"'`<>()[\]{}|,;=]+/g;
/** Characters a path token may hold (letters and digits of any script). */
const PATHLIKE = /^[\p{L}\p{N}_./~@+%-]+$/u;
/** Bytes of one text scanned: a command's head, an output's head and tail. */
const HEAD_BYTES = 4096;
const TAIL_BYTES = 4096;

/** The parts of a long text worth scanning: outputs announce what they
 *  wrote at the end ("saved to …"), commands name it up front. */
function scanned(text: string): string {
  if (text.length <= HEAD_BYTES + TAIL_BYTES) return text;
  return `${text.slice(0, HEAD_BYTES)}\n${text.slice(-TAIL_BYTES)}`;
}

/**
 * Artifact-shaped paths mentioned in `texts` (most relevant first), as
 * written — relative ones resolve later against the session's directories.
 * Deduped, URLs skipped, at most `cap`.
 */
export function artifactMentions(texts: readonly string[], cap = 24): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const text of texts) {
    for (const m of scanned(text).matchAll(TOKEN)) {
      // Trailing sentence punctuation and markdown emphasis come off.
      const t = m[0].replace(/^[*_]+/, "").replace(/[.:!?*_]+$/, "");
      if (t.length < 3 || seen.has(t) || !PATHLIKE.test(t)) continue;
      if (isUrlLike(t)) continue;
      if (!isArtifactPath(t) || (t.startsWith(".") && !t.startsWith("./") && !t.startsWith("../"))) continue;
      seen.add(t);
      out.push(t);
      if (out.length >= cap) return out;
    }
  }
  return out;
}

/** A markdown image/embed in agent prose: `![alt](target)`, the target
 *  either `<…>`-wrapped (spaces allowed) or bare, optionally with a
 *  `#fragment`. */
const PROSE_EMBED = /!\[[^\]]*\]\(\s*(?:<([^>]+)>|([^)\s]+))/g;
/** Fenced and inline code, where an embed is text, not a card. */
const CODE_SPANS = /```[\s\S]*?```|`[^`\n]*`/g;

/**
 * The local files the turn's prose already embeds as cards
 * (`![](figs/plot.png)`, `![](paper.pdf#page=3)`), as written, fragments
 * off. Rendered inline beside the words that describe them, they need no
 * second showing in the gallery. An embed quoted inside code is not one.
 */
export function proseEmbedTargets(texts: readonly string[]): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const text of texts) {
    for (const m of text.replace(CODE_SPANS, "").matchAll(PROSE_EMBED)) {
      const target = splitTarget(m[1] ?? m[2]).path;
      if (target === "" || seen.has(target) || isUrlLike(target)) continue;
      seen.add(target);
      out.push(target);
    }
  }
  return out;
}

/** A path with any leading `./` and `../` hops removed, for suffix matching. */
function hopless(path: string): string {
  return path.replace(/^(\.\.?\/)+/, "");
}

/**
 * Whether `name`, as the prose wrote it, refers to `path` (absolute from a
 * tool, or as written elsewhere). A relative name matches by suffix on a
 * directory boundary — the same file is named two ways in one turn — and
 * an absolute name matches exactly.
 */
export function namesFile(name: string, path: string): boolean {
  if (name === path) return true;
  if (name.startsWith("/")) return false;
  const rel = hopless(name);
  return rel !== "" && (hopless(path) === rel || path.endsWith(`/${rel}`));
}

/**
 * The files among `paths` that the prose's `names` refer to. Each name
 * claims one file, the shallowest match — a bare `notes.md` is the one at
 * the base, not `docs/notes.md` — so a name covers a file, never a family.
 */
export function proseCovered(paths: readonly string[], names: readonly string[]): Set<string> {
  const out = new Set<string>();
  for (const name of names) {
    let best: string | null = null;
    let bestDepth = Infinity;
    for (const p of paths) {
      if (!namesFile(name, p)) continue;
      const depth = p.split("/").length;
      if (depth < bestDepth) {
        best = p;
        bestDepth = depth;
      }
    }
    if (best !== null) out.add(best);
  }
  return out;
}

/** Slack between the journal's clock and a file's mtime (one host, but
 *  NFS stamps with the file server's clock). */
export const MTIME_SLACK_MS = 3000;

/** Whether a file modified at `mtimeMs` was written during a turn that ran
 *  from `startedAtMs` to `endedAtMs` (journal timestamps). */
export function writtenDuring(
  mtimeMs: number | null,
  startedAtMs: number | null,
  endedAtMs: number | null,
): boolean {
  if (mtimeMs === null || startedAtMs === null) return false;
  if (mtimeMs < startedAtMs - MTIME_SLACK_MS) return false;
  return endedAtMs === null || mtimeMs <= endedAtMs + MTIME_SLACK_MS;
}

/** What a chip can say about its file now, against the turn that wrote
 *  it: still as written, rewritten by a later turn (or by hand), or gone. */
export type FileState = "present" | "changed" | "gone";

/** The state of a resolved file for a turn that ended at `endedAtMs`. A
 *  modification after the turn (past the clock slack) is a change the
 *  reader should know about before opening what this turn wrote. */
export function fileStateAfter(r: TargetResult, endedAtMs: number | null): FileState {
  if (isMissing(r)) return "gone";
  if (endedAtMs !== null && r.mtime_ms !== null && r.mtime_ms > endedAtMs + MTIME_SLACK_MS) {
    return "changed";
  }
  return "present";
}

/**
 * Chip labels for a set of paths: each file's name, widened with parent
 * directories only where two files share one (`a/README.md`, `b/README.md`),
 * one directory at a time until every label is distinct.
 */
export function chipLabels(paths: readonly string[]): Map<string, string> {
  const segments = new Map(paths.map((p) => [p, p.split("/").filter((s) => s !== "")]));
  const depth = new Map(paths.map((p) => [p, 1]));
  for (;;) {
    const groups = new Map<string, string[]>();
    for (const p of paths) {
      const segs = segments.get(p) ?? [p];
      const label = segs.slice(Math.max(0, segs.length - (depth.get(p) ?? 1))).join("/") || p;
      const members = groups.get(label);
      if (members === undefined) groups.set(label, [p]);
      else members.push(p);
    }
    let widened = false;
    for (const members of groups.values()) {
      if (members.length < 2) continue;
      for (const p of members) {
        const d = depth.get(p) ?? 1;
        if (d < (segments.get(p)?.length ?? 0)) {
          depth.set(p, d + 1);
          widened = true;
        }
      }
    }
    if (!widened) {
      const out = new Map<string, string>();
      for (const [label, members] of groups) for (const p of members) out.set(p, label);
      return out;
    }
  }
}
