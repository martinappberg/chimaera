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

import { innerExtension } from "../previews/files";

/** Extensions a turn's gallery shows: the outputs people open — figures,
 *  reports, documents, tables, notebooks, slides, media. Source code is
 *  not an artifact (its diff is already in the tool card). */
const ARTIFACT_EXTS = new Set([
  // figures
  "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "avif",
  // reports and documents
  "html", "htm", "pdf", "md", "markdown", "docx", "pptx",
  // tables
  "csv", "tsv", "xlsx", "xls", "xlsm", "ods",
  "vcf", "bed", "bedgraph", "narrowpeak", "broadpeak", "gff", "gff3", "gtf",
  // notebooks
  "ipynb",
  // media
  "mp4", "webm", "m4v", "mov", "ogv", "mp3", "wav", "m4a", "flac", "ogg", "oga", "opus", "aac",
]);

/** Whether a path is the kind of file a turn's gallery shows. */
export function isArtifactPath(path: string): boolean {
  return ARTIFACT_EXTS.has(innerExtension(path));
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
      if (/^[a-z][a-z0-9+.-]*:/i.test(t) || t.startsWith("//")) continue;
      if (!isArtifactPath(t) || (t.startsWith(".") && !t.startsWith("./") && !t.startsWith("../"))) continue;
      seen.add(t);
      out.push(t);
      if (out.length >= cap) return out;
    }
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
