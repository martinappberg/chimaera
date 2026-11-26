/**
 * A tool call's arguments as the rows an opened tool line lists: what the
 * agent passed, minus what the line already shows another way (the command,
 * the file it names, the text a diff draws).
 */

export interface ArgRow {
  key: string;
  value: string;
}

const MAX_ROWS = 12;
const MAX_VALUE = 600;

/** Shown elsewhere on the row: the command and its description lead an
 *  execute row, an edit's texts are its diff, a wait is plumbing. */
const SHOWN_ELSEWHERE = new Set([
  "command",
  "CommandLine",
  "description",
  "toolSummary",
  "old_string",
  "new_string",
  "content",
  "edits",
  "TargetContent",
  "ReplacementContent",
  "Target",
  "Replacement",
  "Content",
  "WaitMsBeforeAsync",
]);

function show(value: unknown): string | null {
  if (value === null || value === undefined || value === "") return null;
  const text = typeof value === "string" ? value : JSON.stringify(value);
  if (text === undefined || text === "{}" || text === "[]") return null;
  return text.length > MAX_VALUE ? `${text.slice(0, MAX_VALUE - 1)}…` : text;
}

/** The arguments worth listing, in the agent's order. `shown`: values the
 *  row already displays (its locations), dropped when an argument repeats one. */
export function argRows(input: unknown, shown: readonly string[] = []): ArgRow[] {
  if (input === null || typeof input !== "object" || Array.isArray(input)) return [];
  const rows: ArgRow[] = [];
  for (const [key, raw] of Object.entries(input as Record<string, unknown>)) {
    if (SHOWN_ELSEWHERE.has(key) || key.startsWith("_")) continue;
    const value = show(raw);
    if (value === null || shown.includes(value)) continue;
    rows.push({ key, value });
    if (rows.length === MAX_ROWS) break;
  }
  return rows;
}
