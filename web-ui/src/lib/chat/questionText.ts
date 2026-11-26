/**
 * A question's text as the card shows it. Agents write questions in
 * markdown — Antigravity links every file it mentions,
 * `[calc.py](file:///…/calc.py)`. The text is the agent's, untrusted, and a
 * question is one sentence, not a document: the card draws its own small
 * inline subset as elements (never HTML) — code spans, bold, and links,
 * where a `file:` link is the file it names and any other link reads as
 * its label. Nothing inside a code span is touched.
 */

export type AskedPart =
  | { kind: "text" | "code" | "strong"; text: string }
  /** A `file:` link: `text` is the label, `path` the absolute path. */
  | { kind: "file"; text: string; path: string };

const INLINE = /`([^`\n]+)`|\[([^\]\n]+)\]\(((?:[^()\s]|\([^()\s]*\))+)\)|\*\*([^*\n]+)\*\*/g;

/** The absolute path a `file:` URL names, or null (another scheme, a
 *  remote host, a malformed escape). */
function filePath(href: string): string | null {
  const m = /^file:\/\/(?:localhost)?(\/[^?#]*)/i.exec(href);
  if (m === null) return null;
  try {
    return decodeURIComponent(m[1]);
  } catch {
    return null;
  }
}

/** The question as inline parts, in order. */
export function askedParts(text: string): AskedPart[] {
  const parts: AskedPart[] = [];
  const say = (chunk: string) => {
    if (chunk === "") return;
    const last = parts[parts.length - 1];
    if (last !== undefined && last.kind === "text") last.text += chunk;
    else parts.push({ kind: "text", text: chunk });
  };
  let at = 0;
  for (const m of text.matchAll(INLINE)) {
    say(text.slice(at, m.index));
    at = m.index + m[0].length;
    if (m[1] !== undefined) parts.push({ kind: "code", text: m[1] });
    else if (m[4] !== undefined) parts.push({ kind: "strong", text: m[4] });
    else {
      const path = filePath(m[3]);
      if (path !== null) parts.push({ kind: "file", text: m[2], path });
      else say(m[2]);
    }
  }
  say(text.slice(at));
  return parts;
}

/** The question on one plain line (the answered row, a tooltip): links as
 *  their labels, bold as its words, code spans keeping their backticks. */
export function askedText(text: string): string {
  return askedParts(text)
    .map((part) => (part.kind === "code" ? `\`${part.text}\`` : part.text))
    .join("");
}

/**
 * Agents mark the option they would pick in its label — "div (Recommended)"
 * from Claude, "(Recommended) div" from Antigravity. The card shows the
 * label without the marker and says "recommended" beside it; the answer
 * sent back is always the agent's own label, marker included.
 */
export function optionLabel(label: string): { text: string; recommended: boolean } {
  const text = label.replace(/^\s*\(recommended\)\s*|\s*\(recommended\)\s*$/gi, "");
  return text !== label && text !== "" ? { text, recommended: true } : { text: label, recommended: false };
}
