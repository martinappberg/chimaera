/**
 * Short forms for uploaded files in the chat composer.
 *
 * A desktop drop lands in the session's landing pad and types its mention
 * where the user is writing — `@/Users/…/.chimaera/uploads/s-1a2b3c4d/plot.png`,
 * a hundred machine-made characters in front of the sentence. The composer
 * shows the file's name in its place (`@plot.png`), right where the
 * reference sits, and puts the whole mention back whenever text leaves the
 * composer (a send, a copy, the saved draft). The agent therefore receives
 * exactly the text a drop always typed; typed and picked `@` mentions are
 * never touched.
 *
 * A short form is one unit, like a chip: the caret never rests inside it
 * (`snapRange`), a deletion that reaches into it takes all of it, and text
 * typed against its end is kept apart from it (`settleEdit`). It is
 * recognized by its text (`tokenSpans`), so undo, redo and paste bring one
 * back whole. `tokens` maps each short form to the whole mention it stands
 * for, and only grows while a composer lives — an undo can restore a short
 * form that was deleted, and it must still expand.
 */

import { extractFileRefs } from "../shared/fileRef";
import { agentMention } from "../shared/reference";
import { regexEscape } from "./composer";
import { uploadName } from "./paths";

/** Short form → the whole mention it stands for. */
export type UploadTokens = Map<string, string>;

export interface TokenSpan {
  start: number;
  end: number;
  token: string;
}

/** A short form, unless a name continues it before the next whitespace —
 *  a letter, a digit, `_`, or the `@` / `/` that start or extend a path
 *  (`@plot.png.bak` and `@plot.png/x` are other files, not `@plot.png` +
 *  text). Whatever precedes it is fine: the send separates it
 *  (`expandUploadMentions`). */
function tokenRe(tokens: Iterable<string>): RegExp {
  const alternatives = [...tokens]
    .sort((a, b) => b.length - a.length)
    .map(regexEscape)
    .join("|");
  return new RegExp(`(?:${alternatives})(?=[^\\s\\p{L}\\p{N}_@/]*(?:\\s|$))`, "gu");
}

/** Each map's compiled pattern. A map only ever gains short forms, so its
 *  size says whether the pattern is current. (`matchAll` copies the
 *  pattern, so the shared `g` state is never touched.) */
const compiled = new WeakMap<UploadTokens, { size: number; re: RegExp }>();

/** Every short form in `text`, in order. */
export function tokenSpans(text: string, tokens: UploadTokens): TokenSpan[] {
  if (tokens.size === 0 || !text.includes("@")) return [];
  let hit = compiled.get(tokens);
  if (hit === undefined || hit.size !== tokens.size) {
    hit = { size: tokens.size, re: tokenRe(tokens.keys()) };
    compiled.set(tokens, hit);
  }
  return [...text.matchAll(hit.re)].map((m) => ({
    start: m.index,
    end: m.index + m[0].length,
    token: m[0],
  }));
}

/**
 * Show each mention of a landing-pad file in `text` by its name, recording
 * what the name stands for in `tokens`. A mention with a suffix (`:12`,
 * `#L3`) stays whole — the short form must say exactly what the long one
 * did — and so does one whose short form would be ambiguous: already
 * standing for another file, or already written in `context` (the draft it
 * joins) or elsewhere in `text`, where it means something else.
 */
export function collapseUploadMentions(text: string, tokens: UploadTokens, context = ""): string {
  let out = "";
  let last = 0;
  for (const found of extractFileRefs(text)) {
    const span = text.slice(found.start, found.end);
    const name = uploadName(found.ref.path);
    if (!span.startsWith("@") || name === null) continue;
    if (!span.endsWith(`/${name}`) && !span.endsWith(`/${name}"`)) continue;
    const short = agentMention(name);
    const known = tokens.get(short);
    if (known !== undefined ? known !== span : tokenRe([short]).test(`${context}\n${text}`)) continue;
    tokens.set(short, span);
    out += text.slice(last, found.start) + short;
    last = found.end;
  }
  return out + text.slice(last);
}

/**
 * The text with every short form put back to its whole mention — what the
 * agent, the clipboard and the saved draft get. Claude reads a mention only
 * after whitespace, so one written against the text before it gets a space
 * there; after it only punctuation can follow (`tokenSpans`), which claude
 * trims off the mention.
 */
export function expandUploadMentions(text: string, tokens: UploadTokens): string {
  let out = "";
  let last = 0;
  for (const span of tokenSpans(text, tokens)) {
    out += text.slice(last, span.start);
    if (out.length > 0 && !/\s$/.test(out)) out += " ";
    out += tokens.get(span.token) ?? span.token;
    last = span.end;
  }
  return out + text.slice(last);
}

/**
 * Where a caret or selection may sit: a caret inside a short form moves to
 * its nearer edge (the end on a tie); a selection grows to take every short
 * form it touches whole.
 */
export function snapRange(spans: TokenSpan[], start: number, end: number): [number, number] {
  let a = start;
  let b = end;
  for (const span of spans) {
    if (a === b) {
      if (span.start < a && a < span.end) {
        const edge = a - span.start < span.end - a ? span.start : span.end;
        return [edge, edge];
      }
      continue;
    }
    if (span.start < a && a < span.end) a = span.start;
    if (span.start < b && b < span.end) b = span.end;
  }
  return [a, b];
}

/**
 * Whether replacing `text[from, to)` with `insert` leaves every short form
 * outside that stretch reading as one — the check the field makes before an
 * edit happens (typing against one's end, deleting the space that keeps it
 * apart from the next word), so it can shape the edit instead of fixing it.
 */
export function keepsTokens(text: string, from: number, to: number, insert: string, tokens: UploadTokens): boolean {
  const next = text.slice(0, from) + insert + text.slice(to);
  return lostTokens(tokenSpans(text, tokens), next, from, to, insert.length, tokens).length === 0;
}

/**
 * The short forms of `old` (spans in a text before an edit replaced its
 * `[from, to)` with `inserted` characters) that the edit left alone but that
 * no longer read as one in `text`, the text after it — at their positions
 * there.
 */
function lostTokens(
  old: TokenSpan[],
  text: string,
  from: number,
  to: number,
  inserted: number,
  tokens: UploadTokens,
): TokenSpan[] {
  const kept = old.filter((s) => s.end <= from || s.start >= to);
  if (kept.length === 0) return [];
  const shift = inserted - (to - from);
  const now = new Set(tokenSpans(text, tokens).map((s) => `${s.start}:${s.token}`));
  return kept
    .map((s) => (s.start >= to ? { ...s, start: s.start + shift, end: s.end + shift } : s))
    .filter((s) => !now.has(`${s.start}:${s.token}`));
}

/**
 * Keep short forms whole across an edit the keyboard handling did not
 * shape (a word deletion, a drag, autocorrect, an IME commit): given the
 * text `before` and `after` an edit that left the caret at `caret`, answer
 * the text it should have become, or null when it is fine as it is. A short
 * form the edit cut into is removed whole (what it had left of it goes too);
 * one the edit glued text onto gets a space after it.
 */
export function settleEdit(
  before: string,
  after: string,
  caret: number,
  tokens: UploadTokens,
): { text: string; caret: number } | null {
  const old = tokenSpans(before, tokens);
  if (old.length === 0) return null;
  // The changed stretch: `before[from, oldEnd)` became `after[from, newEnd)`,
  // the unchanged tail not reaching back past the caret.
  let tail = 0;
  const tailMax = Math.min(after.length - caret, before.length, after.length);
  while (tail < tailMax && before[before.length - 1 - tail] === after[after.length - 1 - tail]) tail++;
  let from = 0;
  const headMax = Math.min(before.length - tail, after.length - tail);
  while (from < headMax && before[from] === after[from]) from++;
  const oldEnd = before.length - tail;
  const inserted = after.slice(from, after.length - tail);

  // A short form the change reached into without taking whole goes whole.
  let cutFrom = from;
  let cutTo = oldEnd;
  for (const span of old) {
    const touched =
      cutFrom === cutTo ? span.start < cutFrom && cutFrom < span.end : span.start < cutTo && span.end > cutFrom;
    if (!touched) continue;
    cutFrom = Math.min(cutFrom, span.start);
    cutTo = Math.max(cutTo, span.end);
  }
  let text = before.slice(0, cutFrom) + inserted + before.slice(cutTo);
  let at = cutFrom + inserted.length;
  let changed = cutFrom !== from || cutTo !== oldEnd;

  // A surviving short form that no longer reads as one had text glued to its
  // end: part them with a space (the caret stays after what was typed). Last
  // first, so each space leaves the earlier positions standing.
  for (const { end } of lostTokens(old, text, cutFrom, cutTo, inserted.length, tokens).reverse()) {
    text = `${text.slice(0, end)} ${text.slice(end)}`;
    // A caret right at the seam (a deleted space) stays where it was.
    if (at > end) at += 1;
    changed = true;
  }
  return changed ? { text, caret: at } : null;
}
