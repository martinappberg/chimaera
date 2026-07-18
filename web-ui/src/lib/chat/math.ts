import katex from "katex";

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
