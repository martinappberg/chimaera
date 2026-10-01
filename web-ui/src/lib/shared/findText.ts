/** Literal matching preserves UTF-16 offsets even when case-folding changes length. */
export function literalPattern(query: string, caseSensitive = false): RegExp | null {
  return query === "" ? null : new RegExp(query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), caseSensitive ? "gu" : "giu");
}

export function textMatches(text: string, query: string, caseSensitive = false, limit = 1000): Array<[number, number]> {
  const pattern = literalPattern(query, caseSensitive);
  if (pattern === null || limit <= 0) return [];
  const matches: Array<[number, number]> = [];
  for (const match of text.matchAll(pattern)) {
    matches.push([match.index, match.index + match[0].length]);
    if (matches.length >= limit) break;
  }
  return matches;
}
