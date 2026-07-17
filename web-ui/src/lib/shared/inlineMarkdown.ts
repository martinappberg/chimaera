/**
 * Render a SMALL subset of inline markdown to safe HTML, for compact
 * single-line surfaces (a dashboard card's now-line, a status chip) where the
 * full `chat/Markdown.svelte` block renderer would be wrong — it emits
 * paragraphs/headings/lists with margins, streams, and stamps paths.
 *
 * Agent output is untrusted; this is XSS-safe BY CONSTRUCTION, not by a
 * post-hoc sanitizer: every HTML metacharacter is escaped FIRST, so the
 * model's own `<`/`>`/`&`/`"` become inert entities, and the ONLY tags in the
 * result are the literal ones this function injects around already-escaped
 * content (no attributes, no user-controlled tag names, no `<` can originate
 * from the input). That is the safe form the "never `{@html}` raw agent text"
 * rule points at — the text here is escaped, never raw. (Kept dependency-free
 * on purpose: DOMPurify needs a DOM and would only re-verify what escaping
 * already guarantees.)
 *
 * Handles `**bold**` / `__bold__`, `` `code` `` (its contents are protected
 * from further formatting), `~~strike~~`, and strips a single leading block
 * marker (`#`, `>`, `-`, `*`, `1.`) so a heading/bullet status line reads
 * clean. Single-`*`/`_` italic is deliberately NOT rendered: `_` is everywhere
 * in code identifiers (`foo_bar`) and a lone `*` is usually a bullet or
 * multiplication. An unclosed marker (a truncated tail like `**Workflow`)
 * stays literal.
 */

/** Private-use sentinel wrapping stashed code spans — never appears in real
 *  status text, and not matched by the bold/strike markers. */
const SENTINEL = String.fromCharCode(0xe000);

export function inlineMarkdown(text: string): string {
  // Strip one leading block marker on the RAW text (before `>` becomes an
  // entity), and drop any real sentinel char so it can't collide with ours.
  const stripped = text
    .split(SENTINEL)
    .join("")
    .replace(/^\s*(?:#{1,6}\s+|>\s+|[-*+]\s+|\d+\.\s+)/, "");

  // Escape HTML metacharacters that matter in TEXT content (`"` needs no
  // escaping outside an attribute), so the model's own markup is inert.
  let s = stripped
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");

  // Pull code spans out FIRST so bold/strike markers inside them stay literal;
  // their contents are already escaped, so the stashed text is inert.
  const code: string[] = [];
  s = s.replace(/`([^`]+)`/g, (_, inner: string) => {
    code.push(inner);
    return `${SENTINEL}${code.length - 1}${SENTINEL}`;
  });

  s = s
    .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
    .replace(/__([^_]+)__/g, "<strong>$1</strong>")
    .replace(/~~([^~]+)~~/g, "<del>$1</del>");

  return s.replace(
    new RegExp(`${SENTINEL}(\\d+)${SENTINEL}`, "g"),
    (_, i: string) => `<code>${code[Number(i)]}</code>`,
  );
}
