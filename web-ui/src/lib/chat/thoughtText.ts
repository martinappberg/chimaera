/**
 * The collapsed line of a reasoning row. Codex streams its reasoning as
 * titled sections (`**Checking the screen**`, a blank line, the body), and
 * both agents use inline markdown, so the preview reads the text as prose,
 * not source: no `**`, heading hashes or backticks.
 */

const MAX = 160;

/** A whole-line bold title — codex's reasoning-summary section header. */
const SECTION = /^\s*\*\*(.+?)\*\*\s*$/;

/** One line of markdown as plain text. Code spans are set aside first, so
 *  `__init__.py` and `**kwargs` survive; a still-streaming title has no
 *  closing `**` yet, so a leading one goes on its own. */
function plain(line: string): string {
  const code: string[] = [];
  return line
    .replace(/`([^`]+)`/g, (_, span: string) => `\u0000${code.push(span) - 1}\u0000`)
    .replace(/^\s*#{1,6}\s+/, "")
    .replace(/\*\*(.+?)\*\*/g, "$1")
    .replace(/^\s*\*\*/, "")
    .replace(/\u0000(\d+)\u0000/g, (_, i: string) => code[Number(i)] ?? "")
    .trim();
}

/** The newest whole-line section title, scanning back from the end (the
 *  live row re-asks on every streamed chunk). */
function newestSection(text: string): string | undefined {
  let end = text.length;
  while (end > 0) {
    const start = text.lastIndexOf("\n", end - 1) + 1;
    const title = SECTION.exec(text.slice(start, end));
    if (title) return title[1];
    end = start - 1;
  }
  return undefined;
}

/** The thought's newest section title — what the agent is on now while it
 *  streams, the way codex's own status line reads, and the same line once
 *  it settles — else its first line. */
export function thoughtPreview(text: string): string {
  const line = newestSection(text) ?? text.trimStart().split("\n", 1)[0] ?? "";
  const out = plain(line);
  return out.length > MAX ? `${out.slice(0, MAX)}…` : out;
}
