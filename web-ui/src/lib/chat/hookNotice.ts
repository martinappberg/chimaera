/**
 * A hook's words as claude reports them. A hook that prints several lines
 * arrives as one notice with every line prefixed alike (`Stop says: …`,
 * `PreToolUse:Bash says: …`); read as one run of text that repeats the
 * prefix mid-sentence. This splits such a notice into the prefix, said
 * once, and the hook's own lines.
 */

/** claude's per-line prefix: the hook's name (its event, with what it
 *  matched), then `says: ` — written before every line of the hook's text,
 *  an empty one included (claude 2.1.289). What a hook matched can be an MCP
 *  server's or a file's name, so no alphabet is assumed for the name: it is
 *  read up to the first ` says:` and must then repeat on every line. The
 *  space after the colon is absent only where the notice's own end was
 *  trimmed. */
const SAYS = /^(\S.{0,119}?) says:(?: (.*))?$/;

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
    lines.push(said[2] ?? "");
  }
  return hook === null ? null : { hook, lines };
}
