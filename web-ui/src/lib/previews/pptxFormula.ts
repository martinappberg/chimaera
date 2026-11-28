/**
 * The arithmetic PowerPoint writes in an animation's time-value list
 * (`p:tav`'s `p:strVal`, `p:anim`'s `from`/`to`/`by`): `#ppt_x+.1`,
 * `0-#ppt_w/2`, `4*ppt_h`. Pure, no `eval`.
 *
 * The grammar is numbers, `+ - * /`, unary signs, parentheses and the four
 * shape variables `ppt_x ppt_y ppt_w ppt_h` (with or without `#`), nothing
 * else: a function call, `$`, an unknown name or a stray character is
 * refused (null), and the caller plays the effect as a fade or not at all.
 * Formulas come from the file, so the source and its nesting are bounded.
 */

/** A shape's place, in fractions of the slide: its centre and its size. */
export interface FormulaVars {
  ppt_x: number;
  ppt_y: number;
  ppt_w: number;
  ppt_h: number;
}

export type Formula =
  | { op: "num"; v: number }
  | { op: "var"; name: keyof FormulaVars }
  | { op: "neg"; a: Formula }
  | { op: "+" | "-" | "*" | "/"; a: Formula; b: Formula };

/** Longest source read, in UTF-16 units. */
export const FORMULA_MAX_LENGTH = 256;
/** Deepest nesting of parentheses and unary signs. */
export const FORMULA_MAX_DEPTH = 16;

const VARS = new Set<string>(["ppt_x", "ppt_y", "ppt_w", "ppt_h"]);
const TOKEN = /\s*(?:(\d+\.?\d*(?:[eE][-+]?\d+)?|\.\d+(?:[eE][-+]?\d+)?)|(#?[A-Za-z_][A-Za-z0-9_]*)|([-+*/()]))/y;

type Tok = { k: "num"; v: number } | { k: "var"; name: keyof FormulaVars } | { k: "op"; v: string };

function tokens(src: string): Tok[] | null {
  const out: Tok[] = [];
  TOKEN.lastIndex = 0;
  let at = 0;
  while (at < src.length) {
    if (src.slice(at).trim() === "") break;
    TOKEN.lastIndex = at;
    const m = TOKEN.exec(src);
    if (m === null) return null;
    at = TOKEN.lastIndex;
    if (m[1] !== undefined) {
      const v = Number(m[1]);
      if (!Number.isFinite(v)) return null;
      out.push({ k: "num", v });
    } else if (m[2] !== undefined) {
      const name = m[2].startsWith("#") ? m[2].slice(1) : m[2];
      if (!VARS.has(name)) return null;
      out.push({ k: "var", name: name as keyof FormulaVars });
    } else out.push({ k: "op", v: m[3]! });
  }
  return out;
}

/** Parse `src`, or null when it is anything but the grammar above. */
export function parseFormula(src: string): Formula | null {
  if (src.length > FORMULA_MAX_LENGTH) return null;
  const toks = tokens(src);
  if (toks === null || toks.length === 0) return null;
  let i = 0;
  const peekOp = (): string | null => {
    const t = toks[i];
    return t?.k === "op" ? t.v : null;
  };
  // Recursive descent; `depth` bounds both parentheses and unary chains.
  const expr = (depth: number): Formula | null => {
    let a = term(depth);
    while (a !== null && (peekOp() === "+" || peekOp() === "-")) {
      const op = peekOp() as "+" | "-";
      i++;
      const b = term(depth);
      a = b === null ? null : { op, a, b };
    }
    return a;
  };
  const term = (depth: number): Formula | null => {
    let a = unary(depth);
    while (a !== null && (peekOp() === "*" || peekOp() === "/")) {
      const op = peekOp() as "*" | "/";
      i++;
      const b = unary(depth);
      a = b === null ? null : { op, a, b };
    }
    return a;
  };
  const unary = (depth: number): Formula | null => {
    if (depth > FORMULA_MAX_DEPTH) return null;
    const op = peekOp();
    if (op === "-" || op === "+") {
      i++;
      const a = unary(depth + 1);
      return a === null ? null : op === "-" ? { op: "neg", a } : a;
    }
    const t = toks[i];
    if (t === undefined) return null;
    if (t.k === "num") {
      i++;
      return { op: "num", v: t.v };
    }
    if (t.k === "var") {
      i++;
      return { op: "var", name: t.name };
    }
    if (t.v !== "(") return null;
    i++;
    const inner = expr(depth + 1);
    if (inner === null || peekOp() !== ")") return null;
    i++;
    return inner;
  };
  const f = expr(0);
  return f !== null && i === toks.length ? f : null;
}

/** The value of `f` for a shape; null when it isn't a finite number (a
 *  division by zero). */
export function evalFormula(f: Formula, vars: FormulaVars): number | null {
  const go = (n: Formula): number => {
    switch (n.op) {
      case "num":
        return n.v;
      case "var":
        return vars[n.name];
      case "neg":
        return -go(n.a);
      case "+":
        return go(n.a) + go(n.b);
      case "-":
        return go(n.a) - go(n.b);
      case "*":
        return go(n.a) * go(n.b);
      case "/":
        return go(n.a) / go(n.b);
    }
  };
  const v = go(f);
  return Number.isFinite(v) ? v : null;
}

/** Parse and evaluate in one go (null on either failing). */
export function formulaValue(src: string, vars: FormulaVars): number | null {
  const f = parseFormula(src);
  return f === null ? null : evalFormula(f, vars);
}
