/**
 * A failure's words as a row shows them: one short headline, the whole text
 * behind a disclosure when the headline left something out. Agents hand a
 * failure over as raw text — claude's `API Error: 401 {"type":"error",…}`
 * puts a JSON body on screen — so the row reads the API's own message out
 * of it and keeps the rest (the request id included) one click away.
 * Nothing here explains or advises: the headline is the agent's or the
 * API's words, shortened, never reworded.
 */

export interface FailureText {
  /** One line; "" when the agent gave no reason. */
  headline: string;
  /** The whole text, when the headline does not show all of it; null when
   *  it does (a short plain reason has nothing more to open). */
  detail: string | null;
}

/** A plain reason up to this long is shown whole; a longer one is clipped. */
export const HEADLINE_MAX = 120;

const API_ERROR = /^API Error:\s*(\d{3})\b\s*([\s\S]*)$/;

/** The message an error body carries: `error.message` (the Anthropic and
 *  OpenAI shapes), a top-level `message`, else the error's `type`. */
function bodyMessage(body: unknown): string | null {
  if (body === null || typeof body !== "object") return null;
  const record = body as Record<string, unknown>;
  const error = record.error;
  if (error !== null && typeof error === "object") {
    const inner = error as Record<string, unknown>;
    if (typeof inner.message === "string" && inner.message.trim() !== "") return inner.message.trim();
    if (typeof inner.type === "string" && inner.type.trim() !== "") return inner.type.trim();
  }
  if (typeof error === "string" && error.trim() !== "") return error.trim();
  if (typeof record.message === "string" && record.message.trim() !== "") return record.message.trim();
  return null;
}

/** The JSON object in `text` (first `{` to last `}`), with what surrounds
 *  it; null when there is none or it does not parse. */
function jsonIn(text: string): { value: unknown; before: string; after: string } | null {
  const start = text.indexOf("{");
  const end = text.lastIndexOf("}");
  if (start === -1 || end <= start) return null;
  try {
    return { value: JSON.parse(text.slice(start, end + 1)), before: text.slice(0, start), after: text.slice(end + 1) };
  } catch {
    return null;
  }
}

/** One line of at most `max` characters, cut at a word where one is near. */
function clip(line: string, max: number): string {
  if (line.length <= max) return line;
  const cut = line.slice(0, max - 1);
  const space = cut.lastIndexOf(" ");
  return `${(space > max * 0.6 ? cut.slice(0, space) : cut).trimEnd()}…`;
}

/** The detail for a parsed body: the reason with its JSON laid out. */
function laidOut(found: { value: unknown; before: string; after: string }): string {
  const json = JSON.stringify(found.value, null, 2);
  const before = found.before.trim();
  const after = found.after.trim();
  return [before, json, after].filter((part) => part !== "").join("\n");
}

export function failureText(reason: string): FailureText {
  const text = reason.trim();
  if (text === "") return { headline: "", detail: null };

  const api = API_ERROR.exec(text);
  if (api !== null) {
    const found = jsonIn(api[2]);
    const message = found !== null ? bodyMessage(found.value) : null;
    if (found !== null && message !== null) {
      return {
        headline: `${clip(message.split("\n")[0], HEADLINE_MAX)} (${api[1]})`,
        detail: `API Error: ${api[1]}\n${laidOut(found)}`,
      };
    }
  } else if (text.startsWith("{")) {
    // A bare error body (some agents pass the API's JSON through as is).
    const found = jsonIn(text);
    const message = found !== null ? bodyMessage(found.value) : null;
    if (found !== null && message !== null) {
      return { headline: clip(message.split("\n")[0], HEADLINE_MAX), detail: laidOut(found) };
    }
  }

  const firstLine = text.split("\n")[0].trim();
  if (firstLine === text && text.length <= HEADLINE_MAX) return { headline: text, detail: null };
  return { headline: clip(firstLine, HEADLINE_MAX), detail: text };
}
