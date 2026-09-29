/**
 * What a `ui/1` screen's nodes share (the host `PluginScreen` provides it
 * through context) and the pure pieces its nodes use: tones, the node set
 * this client draws, the built-in actions, and the prose diff.
 * Design: docs/plugin-platform-plan.md §3; every node and prop:
 * docs/agent-guides/plugins.md ("Screens").
 */
import { getContext, setContext } from "svelte";

export type Tone = "neutral" | "accent" | "good" | "warn" | "bad";

export function tone(v: unknown): Tone {
  return v === "accent" || v === "good" || v === "warn" || v === "bad" ? v : "neutral";
}

/** Every node type this client draws. Anything else draws its `fallback`,
 *  else a quiet placeholder with its children. */
export const KNOWN_NODES: ReadonlySet<string> = new Set([
  "stack", "row", "grid", "split", "tabs", "section", "card", "divider",
  "text", "heading", "markdown", "code", "keyvalue", "badge", "icon", "progress", "empty", "callout",
  "list", "table", "file", "link", "image",
  "button", "toggle", "select", "textfield", "form",
  "editor", "pdf", "diagnostics", "diff", "log",
]);

/** Actions the client carries out itself; a plugin names them on a button. */
export const BUILTIN_ACTIONS: ReadonlySet<string> = new Set([
  "save-to-workspace", "open-file", "open-view", "open-url", "copy", "ask-agent", "install-tool",
]);

/** What a screen's nodes may ask of their host. */
export interface ScreenCtx {
  ws: string;
  wsRoot: string | null;
  plugin: string;
  view: string;
  /** Run a node's action (the plugin's, or a built-in one). */
  act(action: string, payload: unknown, extra?: { form?: Record<string, unknown>; value?: unknown }): Promise<void>;
  /** A workspace path or `output:<path>`, made absolute. */
  resolve(ref: string): Promise<string>;
  /** A list or table's next page, through the plugin's `query`. */
  query(name: string, args: unknown): Promise<unknown>;
  /** Whether an action is in flight (inputs hold still meanwhile). */
  readonly busy: boolean;
}

const KEY = Symbol("plugin-screen");

export function provideScreen(ctx: ScreenCtx): void {
  setContext(KEY, ctx);
}

export function useScreen(): ScreenCtx {
  return getContext<ScreenCtx>(KEY);
}

/** A form's fields, collected by the inputs inside it. */
export interface FormCtx {
  values: Record<string, unknown>;
}

const FORM = Symbol("plugin-form");

export function provideForm(f: FormCtx): void {
  setContext(FORM, f);
}

export function useForm(): FormCtx | undefined {
  return getContext<FormCtx | undefined>(FORM);
}

/** `v` as an array of nodes (anything else: none). */
export function nodes(v: unknown): { type: string; [k: string]: unknown }[] {
  return Array.isArray(v)
    ? v.filter((n): n is { type: string } => typeof n === "object" && n !== null && typeof (n as { type?: unknown }).type === "string")
    : [];
}

export function str(v: unknown, fallback = ""): string {
  return typeof v === "string" ? v : typeof v === "number" || typeof v === "boolean" ? String(v) : fallback;
}

// --- the diff node ------------------------------------------------------------

export type DiffOp = { kind: "same" | "add" | "del"; text: string };

/** Longest-common-subsequence ops over `a` and `b` (tokens), bounded:
 *  past `MAX` tokens either side it falls back to "all removed, all added". */
function lcs(a: string[], b: string[]): DiffOp[] {
  const MAX = 2000;
  if (a.length > MAX || b.length > MAX) {
    return [...a.map((text) => ({ kind: "del" as const, text })), ...b.map((text) => ({ kind: "add" as const, text }))];
  }
  const n = a.length;
  const m = b.length;
  const dp: Uint16Array[] = Array.from({ length: n + 1 }, () => new Uint16Array(m + 1));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i][j] = a[i] === b[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
    }
  }
  const out: DiffOp[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      out.push({ kind: "same", text: a[i] });
      i++;
      j++;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
      out.push({ kind: "del", text: a[i++] });
    } else {
      out.push({ kind: "add", text: b[j++] });
    }
  }
  while (i < n) out.push({ kind: "del", text: a[i++] });
  while (j < m) out.push({ kind: "add", text: b[j++] });
  return out;
}

/** One line of a diff: a kept, added or removed line, or (prose) a changed
 *  paragraph with its words marked. */
export type DiffLine =
  | { kind: "same" | "add" | "del"; text: string }
  | { kind: "change"; words: DiffOp[] };

/**
 * Lines compared, and in `prose` mode a removed line followed by an added
 * one is compared word by word, so a rewrapped or lightly edited paragraph
 * is not marked whole.
 */
export function diffLines(before: string, after: string, prose: boolean): DiffLine[] {
  const ops = lcs(before.split("\n"), after.split("\n"));
  const out: DiffLine[] = [];
  for (let k = 0; k < ops.length; k++) {
    const op = ops[k];
    const next = ops[k + 1];
    if (prose && op.kind === "del" && next?.kind === "add") {
      out.push({ kind: "change", words: lcs(op.text.split(/(\s+)/), next.text.split(/(\s+)/)) });
      k++;
      continue;
    }
    out.push(op);
  }
  return out;
}
