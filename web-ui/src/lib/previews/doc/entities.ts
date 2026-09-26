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
  nbsp: " ",
  copy: "©",
  reg: "®",
  trade: "™",
  hellip: "…",
  mdash: "—",
  ndash: "–",
  lsquo: "‘",
  rsquo: "’",
  ldquo: "“",
  rdquo: "”",
  laquo: "«",
  raquo: "»",
  middot: "·",
  bull: "•",
  deg: "°",
  plusmn: "±",
  times: "×",
  divide: "÷",
  micro: "µ",
  para: "¶",
  sect: "§",
  euro: "€",
  pound: "£",
  yen: "¥",
  cent: "¢",
  larr: "←",
  rarr: "→",
  uarr: "↑",
  darr: "↓",
  harr: "↔",
  rArr: "⇒",
  lArr: "⇐",
  hArr: "⇔",
  le: "≤",
  ge: "≥",
  ne: "≠",
  asymp: "≈",
  infin: "∞",
  alpha: "α",
  beta: "β",
  gamma: "γ",
  delta: "δ",
  pi: "π",
  sigma: "σ",
  mu: "μ",
  lambda: "λ",
  check: "✓",
  zwj: "‍",
  zwnj: "‌",
  shy: "­",
};

let decoder: HTMLTextAreaElement | null = null;

/** One character reference (`&amp;`, `&#35;`, `&#x41;`) as its text. */
export function decodeEntity(source: string): string {
  const m = /^&(?:#(\d{1,7})|#[xX]([0-9a-fA-F]{1,6})|([A-Za-z][A-Za-z0-9]*));$/.exec(source);
  if (m === null) return source;
  if (m[1] !== undefined || m[2] !== undefined) {
    const code = m[1] !== undefined ? Number(m[1]) : parseInt(m[2], 16);
    // CommonMark: 0 and out-of-range code points become U+FFFD.
    if (code === 0 || code > 0x10ffff || (code >= 0xd800 && code <= 0xdfff)) return "�";
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
