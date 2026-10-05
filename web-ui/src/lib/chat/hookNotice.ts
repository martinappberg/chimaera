/**
 * A hook's words as claude reports them. A hook that prints several lines
 * arrives as one notice with every line prefixed alike (`Stop says: …`,
 * `PreToolUse:Bash says: …`); read as one run of text that repeats the
 * prefix mid-sentence. This splits such a notice into the prefix, said
 * once, and the hook's own lines.
 */

/** claude's per-line prefix: the hook event (with its matcher), then `says:`. */
const SAYS = /^([A-Za-z][\w:.*|-]{0,80}) says: ?(.*)$/;

export interface HookNotice {
  /** The hook as claude names it, e.g. `Stop` or `PreToolUse:Bash`. */
  hook: string;
  /** The hook's lines, prefix removed. */
  lines: string[];
}

/** The notice as a hook's multi-line output, or `null` for anything else —
 *  a one-line hook notice included: it already reads right as it is. */
export function hookNotice(text: string): HookNotice | null {
  const rows = text.split(/\r?\n/);
  if (rows.length < 2) return null;
  let hook: string | null = null;
  const lines: string[] = [];
  for (const row of rows) {
    const said = SAYS.exec(row);
    if (!said || (hook !== null && said[1] !== hook)) return null;
    hook = said[1];
    lines.push(said[2]);
  }
  return hook === null ? null : { hook, lines };
}
