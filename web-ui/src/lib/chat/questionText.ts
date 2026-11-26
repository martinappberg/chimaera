/**
 * A question's text as the card shows it. Agents write questions in
 * markdown — Antigravity links every file it mentions,
 * `[calc.py](file:///…/calc.py)` — and the card draws plain text (the text
 * is the agent's, untrusted, and a question is one sentence, not a
 * document): a link reads as its label, bold as its words. Code spans keep
 * their backticks; nothing inside one is touched.
 */
export function askedText(text: string): string {
  const code: string[] = [];
  return text
    .replace(/`[^`\n]+`/g, (span) => `\u0000${code.push(span) - 1}\u0000`)
    .replace(/\[([^\]\n]+)\]\((?:[^()\s]|\([^()\s]*\))+\)/g, "$1")
    .replace(/\*\*([^*\n]+)\*\*/g, "$1")
    .replace(/\u0000(\d+)\u0000/g, (_, i: string) => code[Number(i)] ?? "");
}
