/**
 * ANSI escape handling for program output shown as text (the log viewer,
 * notebook stream and traceback outputs). SGR (`ESC [ … m`) becomes styled
 * runs; every other escape — cursor moves, erases, OSC titles and hyperlinks
 * — is dropped, never rendered as garbage.
 *
 * Colors stay symbolic: a run carries a 16-color palette index, and the view
 * maps indices to the active theme's terminal palette (`ansiVars`), so a log
 * reads in the same colors as a terminal in every theme. 256-color and
 * truecolor codes fold to the nearest of those 16 for the same reason.
 */

/** Palette index 0–15, or one of the two swaps an inverse run needs. */
export type AnsiColor = number | "fg" | "bg";

export interface AnsiStyle {
  fg: AnsiColor | null;
  bg: AnsiColor | null;
  bold: boolean;
  dim: boolean;
  italic: boolean;
  underline: boolean;
  inverse: boolean;
}

export interface AnsiRun {
  text: string;
  /** Null for the default style (no span needed). */
  style: AnsiStyle | null;
}

export function plainStyle(): AnsiStyle {
  return { fg: null, bg: null, bold: false, dim: false, italic: false, underline: false, inverse: false };
}

function isPlain(s: AnsiStyle): boolean {
  return s.fg === null && s.bg === null && !s.bold && !s.dim && !s.italic && !s.underline && !s.inverse;
}

/** xterm's default 16 colors — only used to find the nearest index for an
 *  extended color; what renders is the theme's own palette. */
const XTERM_16: [number, number, number][] = [
  [0, 0, 0], [205, 0, 0], [0, 205, 0], [205, 205, 0], [0, 0, 238], [205, 0, 205], [0, 205, 205], [229, 229, 229],
  [127, 127, 127], [255, 0, 0], [0, 255, 0], [255, 255, 0], [92, 92, 255], [255, 0, 255], [0, 255, 255], [255, 255, 255],
];

/** The 16-color index nearest an RGB triple. */
export function nearest16(r: number, g: number, b: number): number {
  let best = 0;
  let bestD = Infinity;
  for (let i = 0; i < XTERM_16.length; i++) {
    const [pr, pg, pb] = XTERM_16[i];
    // Weighted RGB distance: close enough to perceived difference to pick a
    // hue family, which is all a 16-color fold can honestly promise.
    const d = 2 * (r - pr) ** 2 + 4 * (g - pg) ** 2 + 3 * (b - pb) ** 2;
    if (d < bestD) {
      bestD = d;
      best = i;
    }
  }
  return best;
}

/** An xterm 256-color index folded to 16. */
export function fold256(n: number): number {
  if (n < 16) return n;
  if (n >= 232) {
    const v = 8 + (n - 232) * 10;
    return nearest16(v, v, v);
  }
  const i = n - 16;
  const level = (c: number) => (c === 0 ? 0 : 55 + c * 40);
  return nearest16(level(Math.floor(i / 36)), level(Math.floor(i / 6) % 6), level(i % 6));
}

/** Apply one SGR parameter list to `s` in place. */
function applySgr(s: AnsiStyle, params: number[]): void {
  if (params.length === 0) params = [0];
  for (let i = 0; i < params.length; i++) {
    const p = params[i];
    if (p === 0) Object.assign(s, plainStyle());
    else if (p === 1) s.bold = true;
    else if (p === 2) s.dim = true;
    else if (p === 3) s.italic = true;
    else if (p === 4) s.underline = true;
    else if (p === 7) s.inverse = true;
    else if (p === 22) s.bold = s.dim = false;
    else if (p === 23) s.italic = false;
    else if (p === 24) s.underline = false;
    else if (p === 27) s.inverse = false;
    else if (p >= 30 && p <= 37) s.fg = p - 30;
    else if (p === 39) s.fg = null;
    else if (p >= 40 && p <= 47) s.bg = p - 40;
    else if (p === 49) s.bg = null;
    else if (p >= 90 && p <= 97) s.fg = p - 90 + 8;
    else if (p >= 100 && p <= 107) s.bg = p - 100 + 8;
    else if (p === 38 || p === 48) {
      let color: number | null = null;
      if (params[i + 1] === 5 && i + 2 < params.length) {
        color = fold256(params[i + 2]);
        i += 2;
      } else if (params[i + 1] === 2 && i + 4 < params.length) {
        color = nearest16(params[i + 2], params[i + 3], params[i + 4]);
        i += 4;
      } else {
        // Malformed extended color: the rest of the list is its operands.
        return;
      }
      if (p === 38) s.fg = color;
      else s.bg = color;
    }
  }
}

/** The style a run renders with: inverse swaps foreground and background. */
function effective(s: AnsiStyle): AnsiStyle | null {
  if (isPlain(s)) return null;
  if (!s.inverse) return { ...s };
  return { ...s, fg: s.bg ?? "bg", bg: s.fg ?? "fg", inverse: false };
}

// CSI (`ESC [` or C1 U+009B: params, intermediates, final — grouped so SGR's
// parameters are captured) | OSC (`ESC ]` … BEL or ST) | DCS/SOS/PM/APC
// strings | any other escape (`ESC ( B` from tput, `ESC 7`, a lone ESC).
// eslint-disable-next-line no-control-regex
const ESCAPE = /\u001b\[([0-?]*)([ -/]*)([@-~])|\u009b([0-?]*)([ -/]*)([@-~])|\u001b\][^\u0007\u001b]*(?:\u0007|\u001b\\)?|\u001b[PX^_][^\u001b]*(?:\u001b\\)?|\u001b[ -/]*[0-~]?/g;
// Control characters other than tab (and the newline callers split on).
// eslint-disable-next-line no-control-regex
const CONTROLS = /[\u0000-\u0008\u000b-\u001a\u001c-\u001f\u007f]/g;

/**
 * Split `text` into styled runs, starting from `state` (a line inherits the
 * style the previous one left open). Returns the runs and the style in force
 * at the end. Adjacent runs never share a style object.
 */
export function parseAnsi(text: string, state: AnsiStyle = plainStyle()): { runs: AnsiRun[]; state: AnsiStyle } {
  const s = { ...state };
  const runs: AnsiRun[] = [];
  let last = 0;
  const push = (chunk: string) => {
    const clean = chunk.replace(CONTROLS, "");
    if (clean === "") return;
    const style = effective(s);
    const prev = runs[runs.length - 1];
    if (prev !== undefined && sameStyle(prev.style, style)) prev.text += clean;
    else runs.push({ text: clean, style });
  };
  if (!text.includes("\u001b") && !text.includes("\u009b")) {
    push(text);
    return { runs, state: s };
  }
  ESCAPE.lastIndex = 0;
  for (let m = ESCAPE.exec(text); m !== null; m = ESCAPE.exec(text)) {
    push(text.slice(last, m.index));
    last = m.index + m[0].length;
    const params = m[1] ?? m[4];
    const final = m[3] ?? m[6];
    const inter = m[2] ?? m[5] ?? "";
    if (final === "m" && inter === "" && params !== undefined && !/[<=>?]/.test(params)) {
      applySgr(
        s,
        params === "" ? [] : params.split(/[;:]/).map((p) => (p === "" ? 0 : Number(p))),
      );
    }
  }
  push(text.slice(last));
  return { runs, state: s };
}

function sameStyle(a: AnsiStyle | null, b: AnsiStyle | null): boolean {
  if (a === null || b === null) return a === b;
  return (
    a.fg === b.fg &&
    a.bg === b.bg &&
    a.bold === b.bold &&
    a.dim === b.dim &&
    a.italic === b.italic &&
    a.underline === b.underline
  );
}

/** `text` with every escape sequence and stray control character removed. */
export function stripAnsi(text: string): string {
  return text.replace(ESCAPE, "").replace(CONTROLS, "");
}

/**
 * What a terminal would show for one line that carries carriage returns: a
 * progress bar rewrites its line with `\r`, and only the last rewrite is
 * visible. A trailing `\r` (CRLF endings) is just dropped. Escapes before
 * the last `\r` still count for the style state, so they are kept.
 */
export function collapseCarriageReturns(line: string): string {
  let l = line.endsWith("\r") ? line.slice(0, -1) : line;
  const cr = l.lastIndexOf("\r");
  if (cr < 0) return l;
  // Keep the style-setting escapes from the overwritten part.
  const head = l.slice(0, cr).match(/\u001b\[[0-9;:]*m/g)?.join("") ?? "";
  l = head + l.slice(cr + 1);
  return l;
}

/** Class names for a run (`.af3` foreground 3, `.ab1` background 1, …). */
export function runClass(style: AnsiStyle): string {
  const c: string[] = [];
  if (style.fg !== null) c.push(`af${style.fg}`);
  if (style.bg !== null) c.push(`ab${style.bg}`);
  if (style.bold) c.push("a-b");
  if (style.dim) c.push("a-d");
  if (style.italic) c.push("a-i");
  if (style.underline) c.push("a-u");
  return c.join(" ");
}

/** Append `runs` to `parent` as text nodes and classed spans (never HTML). */
export function appendRuns(parent: Node, runs: AnsiRun[]): void {
  for (const r of runs) {
    if (r.style === null) {
      parent.appendChild(document.createTextNode(r.text));
    } else {
      const span = document.createElement("span");
      span.className = runClass(r.style);
      span.textContent = r.text;
      parent.appendChild(span);
    }
  }
}

const ANSI_KEYS = [
  "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
  "brightBlack", "brightRed", "brightGreen", "brightYellow", "brightBlue", "brightMagenta", "brightCyan", "brightWhite",
] as const;

/**
 * The theme's terminal palette as `--ansi-0` … `--ansi-15` custom properties,
 * for a style attribute on the view root (`ansiCss` holds the rules that
 * read them). Values come from the active theme, never literals here.
 */
export function ansiVars(palette: Record<(typeof ANSI_KEYS)[number], string>): string {
  return ANSI_KEYS.map((k, i) => `--ansi-${i}: ${palette[k]}`).join("; ");
}
