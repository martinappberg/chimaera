/**
 * The context bridge's reference composer: selection knows its source.
 *
 * Pure functions build the exact text typed into an agent or shell input
 * (path relativization, shell escaping, selection truncation) — nothing here
 * ever appends a newline/carriage return, so a composed reference can NEVER
 * auto-submit. The two small stores coordinate the UI halves: views publish
 * the current selection (file views and terminals), the app publishes the
 * resolved target agent, and the floating affordances + the chord both funnel
 * through one registered handler (parity principle).
 *
 * Dev-only console.assert self-checks at the bottom, same policy as layout.ts.
 */

import { writable } from "svelte/store";

// --- selection + target coordination -----------------------------------------

export interface FileSelection {
  kind: "file";
  /** Absolute path on the daemon's filesystem. */
  path: string;
  /** 1-based line range; null when the view has no line mapping (md preview). */
  startLine: number | null;
  endLine: number | null;
  text: string;
}

export interface TerminalSelection {
  kind: "terminal";
  sessionId: string;
  text: string;
}

export type SelectionSource = FileSelection | TerminalSelection;

/**
 * The one live selection eligible for referencing (last writer wins across
 * views; a view only clears what it owns, so another view's fresher selection
 * is never wiped by a stale blur).
 */
export const activeSelection = writable<SelectionSource | null>(null);

/**
 * The agent session references would land in right now (focused agent, else
 * the workspace's most recently active agent), resolved by the app. Null
 * means no agent session exists — affordances render disabled.
 */
export const referenceTarget = writable<{ id: string; name: string } | null>(null);

let selectionOwner: unknown = null;

/** Publish `sel` as the active selection, owned by `owner`. */
export function setSelection(owner: unknown, sel: SelectionSource): void {
  selectionOwner = owner;
  activeSelection.set(sel);
}

/** Clear the active selection, but only if `owner` still owns it. */
export function clearSelection(owner: unknown): void {
  if (selectionOwner === owner) {
    selectionOwner = null;
    activeSelection.set(null);
  }
}

type ReferenceHandler = () => void;
let handler: ReferenceHandler | null = null;

/** App-level wiring: the one function that composes + types the reference. */
export function setReferenceHandler(fn: ReferenceHandler | null): void {
  handler = fn;
}

/** Invoked by every affordance (buttons and the chord alike). */
export function requestReference(): void {
  handler?.();
}

// --- pure composers -----------------------------------------------------------

/** Selection excerpt budget (~200 chars, per the bridge spec). */
export const SELECTION_MAX = 200;

/**
 * One-line excerpt of a selection: control characters (including newlines —
 * a typed "\n" would submit) collapse into spaces, runs of whitespace fold,
 * and anything past `max` chars is cut with an ellipsis.
 */
export function truncateSelection(text: string, max = SELECTION_MAX): string {
  // eslint-disable-next-line no-control-regex
  const flat = text.replace(/[\u0000-\u001f\u007f]/g, " ").replace(/\s+/g, " ").trim();
  if (flat.length <= max) return flat;
  return `${flat.slice(0, max).trimEnd()}…`;
}

/** Root without its trailing slash ("/" stays "/"). */
function normRoot(root: string): string {
  return root.length > 1 && root.endsWith("/") ? root.slice(0, -1) : root;
}

/**
 * `path` relative to the workspace `root` when under it, else the absolute
 * path unchanged (agents can resolve either).
 */
export function workspaceRelative(path: string, root: string): string {
  const r = normRoot(root);
  if (r === "/") return path.startsWith("/") ? path.slice(1) : path;
  if (path === r) return ".";
  return path.startsWith(`${r}/`) ? path.slice(r.length + 1) : path;
}

/**
 * `path` relative to a shell's current working directory when under it, else
 * absolute. No ".." climbing — outside the cwd, absolute is unambiguous.
 */
export function relativeToCwd(path: string, cwd: string): string {
  return workspaceRelative(path, cwd);
}

/** Characters safe to hand a POSIX shell without quoting. */
const SHELL_SAFE = /^[A-Za-z0-9_./-]+$/;

/**
 * Single-quote shell escaping (POSIX): safe paths pass through bare, anything
 * else is wrapped in single quotes with embedded quotes as `'\''` — handles
 * spaces, brackets, globs, `$`, backticks, the lot.
 */
export function shellEscapePath(path: string): string {
  if (path.length > 0 && SHELL_SAFE.test(path)) return path;
  return `'${path.replace(/'/g, "'\\''")}'`;
}

/**
 * A file-selection reference for a claude agent:
 * `@<rel-path>#L<start>-L<end> "<excerpt>" ` — trailing space, no newline
 * (NEVER submits). The `#L` range is omitted when the view has no line
 * mapping (markdown preview).
 */
export function composeFileReference(
  relPath: string,
  startLine: number | null,
  endLine: number | null,
  selection: string,
): string {
  const lines = startLine !== null && endLine !== null ? `#L${startLine}-L${endLine}` : "";
  return `@${relPath}${lines} "${truncateSelection(selection)}" `;
}

/**
 * A terminal-scrollback reference for a claude agent:
 * `<session display name> output: "<excerpt>" `.
 */
export function composeTerminalReference(displayName: string, selection: string): string {
  return `${displayName} output: "${truncateSelection(selection)}" `;
}

/** A bare path mention for a claude agent (drag-to-reference): `@<rel-path> `. */
export function composeAgentPathReference(relPath: string): string {
  return `@${relPath} `;
}

/**
 * The provenance suffix typed after a matching paste into an agent composer:
 * ` [from @<rel-path>#L3-L9] ` for file snippets, ` [from <name> output] `
 * for terminal snippets. Additive and visible — the pasted text itself is
 * never touched (plain paste stays plain), and there is never a newline.
 */
export function composeProvenanceSuffix(
  source: SelectionSource,
  relPath: string | null,
  terminalName: string | null,
): string {
  if (source.kind === "file") {
    const path = relPath ?? source.path;
    const lines =
      source.startLine !== null && source.endLine !== null
        ? `#L${source.startLine}-L${source.endLine}`
        : "";
    return ` [from @${path}${lines}] `;
  }
  return ` [from ${terminalName ?? "terminal"} output] `;
}

/**
 * A shell-ready path for a plain terminal (drag-to-reference): escaped,
 * relative to the session's current cwd when under it, else absolute.
 */
export function composeShellPathReference(path: string, cwd: string): string {
  return `${shellEscapePath(relativeToCwd(path, cwd))} `;
}

// --- dev-only self-checks ---------------------------------------------------
//
// Unit-style assertions matching layout.ts: they run once on the dev server
// (dead code in production builds) and fail loudly in the console.
if (import.meta.env.DEV) {
  const ok = (cond: boolean, msg: string) =>
    console.assert(cond, `reference.ts self-check: ${msg}`);

  // path relativization
  ok(workspaceRelative("/w/src/a.ts", "/w") === "src/a.ts", "relativizes under the root");
  ok(workspaceRelative("/w/src/a.ts", "/w/") === "src/a.ts", "tolerates a trailing-slash root");
  ok(workspaceRelative("/other/a.ts", "/w") === "/other/a.ts", "outside the root stays absolute");
  ok(workspaceRelative("/wx/a.ts", "/w") === "/wx/a.ts", "prefix match respects path boundaries");
  ok(workspaceRelative("/w", "/w") === ".", "the root itself is '.'");
  ok(workspaceRelative("/a b/c d.txt", "/a b") === "c d.txt", "spaces in the root are fine");
  ok(relativeToCwd("/w/data/x.tsv", "/w/data") === "x.tsv", "cwd relativization");
  ok(relativeToCwd("/w/data/x.tsv", "/w/other") === "/w/data/x.tsv", "outside cwd is absolute");

  // shell escaping
  ok(shellEscapePath("results/qc.tsv") === "results/qc.tsv", "safe paths pass bare");
  ok(shellEscapePath("my file.tsv") === "'my file.tsv'", "spaces are single-quoted");
  ok(shellEscapePath("a[1].bed") === "'a[1].bed'", "brackets are quoted");
  ok(shellEscapePath("it's.txt") === "'it'\\''s.txt'", "embedded single quotes escape");
  ok(shellEscapePath("$HOME/x") === "'$HOME/x'", "dollar signs never expand");
  ok(shellEscapePath("") === "''", "empty path still quotes");

  // truncation
  ok(truncateSelection("a\nb\tc") === "a b c", "newlines/tabs flatten to spaces");
  ok(truncateSelection("  padded  ") === "padded", "outer whitespace trims");
  const long = "x".repeat(500);
  ok(truncateSelection(long).length === SELECTION_MAX + 1, "long selections cut at the budget");
  ok(truncateSelection(long).endsWith("…"), "cut selections end in an ellipsis");
  ok(truncateSelection("\u0007bell\u0000nul") === "bell nul", "control chars sanitize");

  // composers: trailing space, never a newline
  ok(
    composeFileReference("src/a.ts", 3, 7, "let x = 1;") === '@src/a.ts#L3-L7 "let x = 1;" ',
    "file reference format",
  );
  ok(
    composeFileReference("README.md", null, null, "intro") === '@README.md "intro" ',
    "line-less reference omits the #L range",
  );
  ok(
    composeTerminalReference("snakemake", "Error in rule qc") ===
      'snakemake output: "Error in rule qc" ',
    "terminal reference format",
  );
  ok(composeAgentPathReference("src/a.ts") === "@src/a.ts ", "agent path mention");
  ok(
    composeProvenanceSuffix(
      { kind: "file", path: "/w/src/a.ts", startLine: 3, endLine: 9, text: "" },
      "src/a.ts",
      null,
    ) === " [from @src/a.ts#L3-L9] ",
    "file provenance suffix",
  );
  ok(
    composeProvenanceSuffix(
      { kind: "terminal", sessionId: "s-1", text: "" },
      null,
      "snakemake",
    ) === " [from snakemake output] ",
    "terminal provenance suffix",
  );
  ok(
    !/[\r\n]/.test(
      composeProvenanceSuffix({ kind: "terminal", sessionId: "s", text: "x\n" }, null, "n"),
    ),
    "provenance suffix never contains newlines",
  );
  ok(
    composeShellPathReference("/w/my file.tsv", "/w") === "'my file.tsv' ",
    "shell drop: relative + escaped",
  );
  ok(
    composeShellPathReference("/elsewhere/f.txt", "/w") === "/elsewhere/f.txt ",
    "shell drop: absolute outside cwd",
  );
  for (const composed of [
    composeFileReference("a", 1, 2, "s\nx"),
    composeTerminalReference("n", "s\r\n"),
    composeAgentPathReference("a"),
    composeShellPathReference("/w/a", "/w"),
  ]) {
    ok(!/[\r\n]/.test(composed), "composed references never contain newlines (never submit)");
  }
}
