/**
 * The collapsed line of a reasoning row. Codex streams its reasoning as
 * titled sections (`**Checking the screen**`, a blank line, the body), and
 * both agents use inline markdown, so the preview reads the text as prose,
 * not source: no `**`, heading hashes or backticks.
 */

const MAX = 160;

/** A whole-line bold title — codex's reasoning-summary section header. */
const SECTION = /^\s*\*\*(.+?)\*\*\s*$/;

/** One line of markdown as plain text. A still-streaming title has no
 *  closing `**` yet, so a leading one goes on its own. */
function plain(line: string): string {
  return line
    .replace(/^\s*#{1,6}\s+/, "")
    .replace(/(\*\*|__)(.+?)\1/g, "$2")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/^\s*\*\*/, "")
    .trim();
}

/** The first line of the thought — or, while it is still being written,
 *  its newest section title (what the agent is on now, the way codex's own
 *  status line reads). */
export function thoughtPreview(text: string, live = false): string {
  let line: string | undefined;
  if (live) {
    const lines = text.split("\n");
    for (let i = lines.length - 1; i >= 0 && line === undefined; i--) {
      const title = SECTION.exec(lines[i]);
      if (title) line = title[1];
    }
  }
  line ??= text.trimStart().split("\n", 1)[0] ?? "";
  const out = plain(line);
  return out.length > MAX ? `${out.slice(0, MAX)}…` : out;
}
