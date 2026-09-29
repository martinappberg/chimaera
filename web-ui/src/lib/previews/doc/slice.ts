/**
 * A stretch of a document's lines, as its own markdown: what a line-range
 * hover (`notes.md#L40-L62`) and the Knowledge reader render. Parsing only
 * the slice keeps a preview of one entry in a large file cheap.
 */

/**
 * Lines `from`..`to` of `source` (1-based, inclusive), joined with `\n`;
 * `to` below `from` (or past the end) runs to the last line. Out of range
 * gives "". Line endings are normalized.
 */
export function sliceLines(source: string, from: number, to: number): string {
  if (from < 1) return "";
  const text = source.includes("\r") ? source.replace(/\r\n?/g, "\n") : source;
  let start = 0;
  for (let line = 1; line < from; line++) {
    const nl = text.indexOf("\n", start);
    if (nl < 0) return "";
    start = nl + 1;
  }
  if (to < from) return text.slice(start);
  let end = start;
  for (let line = from; line <= to; line++) {
    const nl = text.indexOf("\n", end);
    if (nl < 0) return text.slice(start);
    end = nl + 1;
  }
  return text.slice(start, end - 1);
}
