import katex from "katex";
import type { MarkedExtension } from "marked";

/** One bounded, non-trusting KaTeX policy for every chat surface. MathML
 * avoids inline geometry styles (which our sanitizer correctly strips) and
 * stays accessible to screen readers. */
export const mathOptions = {
  throwOnError: false,
  strict: "ignore" as const,
  trust: false,
  output: "mathml" as const,
  maxSize: 20,
  maxExpand: 1000,
};

export function renderMath(source: string, displayMode: boolean): string {
  return katex.renderToString(source, { ...mathOptions, displayMode });
}

const INLINE_DOLLAR =
  /^(\${1,2})(?!\$)((?:\\.|[^\\\n$])+?)\1(?=[\s?!.,:？！。，：]|$)/;
const BLOCK_DOLLAR = /^(\${1,2})\n((?:\\[^]|[^\\])+?)\n\1(?:\n|$)/;

/** Marked tokenizers for the math forms emitted by both supported agents.
 * Keeping these beside renderMath avoids a stale adapter peer range deciding
 * when KaTeX can move, while tokenizing before rendering keeps code literals
 * untouched. */
export const markdownMath = {
  extensions: [
    {
      name: "inlineDollarMath",
      level: "inline" as const,
      start(src: string) {
        let searchFrom = 0;
        while (searchFrom < src.length) {
          const index = src.indexOf("$", searchFrom);
          if (index < 0) return undefined;
          const startsAtBoundary = index === 0 || src[index - 1] === " ";
          if (startsAtBoundary && INLINE_DOLLAR.test(src.slice(index))) return index;
          searchFrom = index + 1;
          while (src[searchFrom] === "$") searchFrom += 1;
        }
        return undefined;
      },
      tokenizer(src: string) {
        const match = INLINE_DOLLAR.exec(src);
        if (match === null) return undefined;
        return {
          type: "inlineDollarMath",
          raw: match[0],
          text: match[2].trim(),
          displayMode: match[1].length === 2,
        };
      },
      renderer(token: Record<string, unknown>) {
        return renderMath(token.text as string, token.displayMode as boolean);
      },
    },
    {
      name: "blockDollarMath",
      level: "block" as const,
      tokenizer(src: string) {
        const match = BLOCK_DOLLAR.exec(src);
        if (match === null) return undefined;
        return {
          type: "blockDollarMath",
          raw: match[0],
          text: match[2].trim(),
          displayMode: match[1].length === 2,
        };
      },
      renderer(token: Record<string, unknown>) {
        return `${renderMath(token.text as string, token.displayMode as boolean)}\n`;
      },
    },
    {
      name: "inlineSlashMath",
      level: "inline" as const,
      start(src: string) {
        const index = src.indexOf("\\(");
        return index >= 0 ? index : undefined;
      },
      tokenizer(src: string) {
        const match = /^\\\(([^\n]*?)\\\)/.exec(src);
        if (match === null) return undefined;
        return {
          type: "inlineSlashMath",
          raw: match[0],
          text: match[1].trim(),
          displayMode: false,
        };
      },
      renderer(token: Record<string, unknown>) {
        return renderMath(token.text as string, false);
      },
    },
    {
      name: "blockSlashMath",
      level: "block" as const,
      start(src: string) {
        const index = src.indexOf("\\[");
        return index >= 0 ? index : undefined;
      },
      tokenizer(src: string) {
        const match = /^\\\[\s*([\s\S]*?)\s*\\\](?:\n|$)/.exec(src);
        if (match === null) return undefined;
        return {
          type: "blockSlashMath",
          raw: match[0],
          text: match[1].trim(),
          displayMode: true,
        };
      },
      renderer(token: Record<string, unknown>) {
        return `${renderMath(token.text as string, true)}\n`;
      },
    },
  ],
} satisfies MarkedExtension;

export type MathRun =
  | { kind: "text"; text: string }
  | { kind: "math"; source: string; display: boolean };

/** Split only math delimiters out of otherwise-verbatim user text. Inline
 * dollar math uses a conservative closing-boundary rule, so ordinary currency
 * such as "$5 and $10" stays plain. Unmatched delimiters stay verbatim. */
export function splitUserMath(text: string): MathRun[] {
  const runs: MathRun[] = [];
  let plainStart = 0;
  let i = 0;
  const addMath = (end: number, source: string, display: boolean) => {
    if (i > plainStart) runs.push({ kind: "text", text: text.slice(plainStart, i) });
    runs.push({ kind: "math", source, display });
    i = end;
    plainStart = end;
  };

  while (i < text.length) {
    if (text.startsWith("\\(", i)) {
      const close = text.indexOf("\\)", i + 2);
      if (close >= 0 && !text.slice(i + 2, close).includes("\n")) {
        addMath(close + 2, text.slice(i + 2, close).trim(), false);
        continue;
      }
    } else if (text.startsWith("\\[", i)) {
      const close = text.indexOf("\\]", i + 2);
      if (close >= 0) {
        addMath(close + 2, text.slice(i + 2, close).trim(), true);
        continue;
      }
    } else if (text.startsWith("$$", i)) {
      const close = text.indexOf("$$", i + 2);
      if (close >= 0) {
        addMath(close + 2, text.slice(i + 2, close).trim(), true);
        continue;
      }
    } else if (text[i] === "$" && text[i + 1] !== "$" && text[i - 1] !== "\\") {
      let close = text.indexOf("$", i + 1);
      while (close > i + 1 && text[close - 1] === "\\") {
        close = text.indexOf("$", close + 1);
      }
      if (close > i + 1) {
        const source = text.slice(i + 1, close);
        const after = text[close + 1];
        const boundary = after === undefined || /[\s?!.,:;)}\]]/.test(after);
        if (!source.includes("\n") && !/^\s|\s$/.test(source) && boundary) {
          addMath(close + 1, source, false);
          continue;
        }
      }
    }
    i += 1;
  }
  if (plainStart < text.length) runs.push({ kind: "text", text: text.slice(plainStart) });
  return runs;
}
