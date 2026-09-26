/**
 * Pure pieces of the log viewer: which lines to flag, and how a byte slice
 * read from the middle of a file is trimmed to whole lines.
 *
 * Slices are cut on `\n` bytes, which never occur inside a multi-byte UTF-8
 * sequence — so every trimmed slice decodes cleanly on its own, and adjacent
 * slices join without a torn character or a half line.
 */

/** Words that mark a failing line in the output of the tools people run on
 *  clusters (Python tracebacks, Slurm, Nextflow, Snakemake, compilers). */
const ERROR_RE =
  /\b(?:error|errors|fatal|critical|exception|traceback|panic(?:ked)?|segmentation fault|core dumped|out of memory|oom[- ]?kill(?:ed)?|killed|cancelled|failed|failure)\b/i;
/** A raised exception by name: `ValueError: …`, `java.io.IOException`. */
const RAISED_RE = /\b[A-Z][A-Za-z]*(?:Error|Exception)\b/;
const WARN_RE = /\b(?:warn|warning|warnings|deprecated|deprecation)\b/i;
/** "0 errors", "no warnings", "failed: 0": a clean summary, not a flag. */
const CLEAN_RE =
  /\b(?:0|no|zero|without)\s+(?:errors?|failures?|failed|warnings?|exceptions?)\b|\b(?:errors?|failed|failures?|warnings?)\s*[:=]\s*0\b/gi;

export type LineLevel = "error" | "warn" | null;

/** The level a line reads as, from its plain (escape-free) text. */
export function lineLevel(plain: string): LineLevel {
  if (plain.length === 0) return null;
  const text = plain.replace(CLEAN_RE, " ");
  if (ERROR_RE.test(text) || RAISED_RE.test(text)) return "error";
  if (WARN_RE.test(text)) return "warn";
  return null;
}

const NL = 0x0a;

/**
 * The byte range of whole lines inside a slice read at some offset of a file.
 * `fromFileStart`: the slice begins at byte 0, so its first line is whole.
 * `toFileEnd`: the slice reaches EOF, so its last line (even without a
 * newline) is whole. A slice holding no newline at all is one fragment of a
 * very long line and is kept whole rather than dropped.
 */
export function wholeLines(
  bytes: Uint8Array,
  fromFileStart: boolean,
  toFileEnd: boolean,
): { start: number; end: number } {
  let start = 0;
  if (!fromFileStart) {
    const nl = bytes.indexOf(NL);
    start = nl < 0 ? 0 : nl + 1;
  }
  let end = bytes.length;
  if (!toFileEnd) {
    const nl = bytes.lastIndexOf(NL);
    end = nl < 0 ? bytes.length : nl + 1;
  }
  if (end < start) return { start: 0, end: bytes.length };
  return { start, end };
}

/** Lines of a decoded slice; a final newline does not open an empty line. */
export function splitLines(text: string): string[] {
  if (text === "") return [];
  const lines = text.split("\n");
  if (lines[lines.length - 1] === "") lines.pop();
  return lines;
}
