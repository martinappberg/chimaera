/**
 * HTML character references as characters, for the markdown model (text,
 * link destinations and titles). lezer's `Entity` token is `&`, a name or
 * number, `;` — it can hold no markup. In a browser every HTML5 name
 * decodes (a textarea's innerHTML is RCDATA: references decode, tags cannot
 * form); without a DOM (the Vitest node runs) the common names below do,
 * and an unknown one stays as written — which is also what a browser shows
 * for a name HTML5 doesn't define.
 */

const NAMED: Record<string, string> = {
  amp: "&",
  lt: "<",
  gt: ">",
  quot: '"',
  apos: "'",
  nbsp: "\u00a0",
  copy: "\u00a9",
  reg: "\u00ae",
  trade: "\u2122",
  hellip: "\u2026",
  mdash: "\u2014",
  ndash: "\u2013",
  lsquo: "\u2018",
  rsquo: "\u2019",
  ldquo: "\u201c",
  rdquo: "\u201d",
  laquo: "\u00ab",
  raquo: "\u00bb",
  middot: "\u00b7",
  bull: "\u2022",
  deg: "\u00b0",
  plusmn: "\u00b1",
  times: "\u00d7",
  divide: "\u00f7",
  micro: "\u00b5",
  para: "\u00b6",
  sect: "\u00a7",
  euro: "\u20ac",
  pound: "\u00a3",
  yen: "\u00a5",
  cent: "\u00a2",
  larr: "\u2190",
  rarr: "\u2192",
  uarr: "\u2191",
  darr: "\u2193",
  harr: "\u2194",
  rArr: "\u21d2",
  lArr: "\u21d0",
  hArr: "\u21d4",
  le: "\u2264",
  ge: "\u2265",
  ne: "\u2260",
  asymp: "\u2248",
  infin: "\u221e",
  alpha: "\u03b1",
  beta: "\u03b2",
  gamma: "\u03b3",
  delta: "\u03b4",
  pi: "\u03c0",
  sigma: "\u03c3",
  mu: "\u03bc",
  lambda: "\u03bb",
  check: "\u2713",
  zwj: "\u200d",
  zwnj: "\u200c",
  shy: "\u00ad",
};

let decoder: HTMLTextAreaElement | null = null;

/** One character reference (`&amp;`, `&#35;`, `&#x41;`) as its text. */
export function decodeEntity(source: string): string {
  const m = /^&(?:#(\d{1,7})|#[xX]([0-9a-fA-F]{1,6})|([A-Za-z][A-Za-z0-9]*));$/.exec(source);
  if (m === null) return source;
  if (m[1] !== undefined || m[2] !== undefined) {
    const code = m[1] !== undefined ? Number(m[1]) : parseInt(m[2], 16);
    // CommonMark: 0 and out-of-range code points become U+FFFD.
    if (code === 0 || code > 0x10ffff || (code >= 0xd800 && code <= 0xdfff)) return "\ufffd";
    return String.fromCodePoint(code);
  }
  const named = NAMED[m[3]];
  if (named !== undefined) return named;
  if (typeof document === "undefined") return source;
  decoder ??= document.createElement("textarea");
  decoder.innerHTML = source;
  return decoder.value;
}

/** Every reference in `text` decoded (a destination or title, where lezer
 *  leaves them as raw text). */
export function decodeEntities(text: string): string {
  if (!text.includes("&")) return text;
  return text.replace(/&(?:#\d{1,7}|#[xX][0-9a-fA-F]{1,6}|[A-Za-z][A-Za-z0-9]*);/g, decodeEntity);
}

/** CommonMark's backslash escapes in a destination or title: an escaped
 *  ASCII punctuation character is itself. */
export function unescapeBackslashes(text: string): string {
  if (!text.includes("\\")) return text;
  return text.replace(/\\([!"#$%&'()*+,\-./:;<=>?@[\\\]^_`{|}~])/g, "$1");
}
