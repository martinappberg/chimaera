/**
 * What an approval is about, read from the request and the tool row it
 * points at: the command, or the change to a file. Each agent says it in its
 * own argument names (claude `command` / `file_path`+`old_string`, codex
 * `command`, Antigravity `CommandLine` / a `diff` on the row); the card shows
 * one thing — the command or the diff — never the raw argument object.
 */
import type { PermissionOption, ToolContent } from "./store.svelte";

export interface ApprovalTool {
  tool: string;
  title: string;
  command: string | null;
  locations: string[];
  content: ToolContent | null;
  nativeInput?: unknown;
}

export type ApprovalSubject =
  | { kind: "command"; command: string; description: string | null }
  | { kind: "edit"; path: string; diffs: ToolContent[] }
  | { kind: "other" };

const str = (v: unknown): string | null => (typeof v === "string" && v.trim() !== "" ? v : null);

/** A shell command from an argument object, whichever name the agent uses. */
function commandOf(input: Record<string, unknown>): string | null {
  const raw = input.command ?? input.CommandLine ?? input.cmd;
  if (Array.isArray(raw) && raw.every((part) => typeof part === "string")) return raw.join(" ");
  return str(raw);
}

/** An edit from an argument object: claude Edit / Write / MultiEdit, and
 *  Antigravity's two servers. Null when the arguments are not an edit. */
function editOf(input: Record<string, unknown>): { path: string; diffs: ToolContent[] } | null {
  const path = str(input.file_path) ?? str(input.TargetFile) ?? str(input.Path) ?? str(input.path);
  if (path === null) return null;
  const diff = (oldText: unknown, newText: unknown): ToolContent | null =>
    typeof newText === "string"
      ? { kind: "diff", path, old_text: typeof oldText === "string" ? oldText : "", new_text: newText }
      : null;
  if (Array.isArray(input.edits)) {
    const diffs = input.edits
      .map((e) => (e !== null && typeof e === "object" ? diff((e as Record<string, unknown>).old_string, (e as Record<string, unknown>).new_string) : null))
      .filter((d): d is ToolContent => d !== null);
    return diffs.length > 0 ? { path, diffs } : null;
  }
  const one =
    diff(input.old_string, input.new_string) ??
    diff(input.TargetContent, input.ReplacementContent) ??
    diff(input.Target, input.Replacement) ??
    diff("", input.content);
  return one === null ? null : { path, diffs: [one] };
}

/** The command or change a permission request asks about. */
export function approvalSubject(inputPreview: unknown, tool: ApprovalTool | null): ApprovalSubject {
  const input =
    inputPreview !== null && typeof inputPreview === "object" && !Array.isArray(inputPreview)
      ? (inputPreview as Record<string, unknown>)
      : {};
  // The row's own diff is what the agent will apply (an ACP `diff`); the
  // arguments are the fallback for agents that only send those.
  const content = tool?.content ?? null;
  if (content !== null && content.kind === "diff" && content.path) {
    return { kind: "edit", path: content.path, diffs: [content] };
  }
  if (content !== null && content.kind === "batch" && (content.diffs?.length ?? 0) > 0) {
    const diffs = content.diffs ?? [];
    return { kind: "edit", path: diffs[0].path ?? tool?.locations[0] ?? "", diffs };
  }
  if (tool === null || tool.tool === "execute") {
    const command = tool?.command ?? commandOf(input) ?? (tool?.tool === "execute" ? str(tool.title) : null);
    if (command !== null) {
      return { kind: "command", command, description: str(input.description) ?? str(input.toolSummary) };
    }
  }
  if (tool === null || tool.tool === "edit") {
    const edit = editOf(input);
    if (edit !== null) return { kind: "edit", ...edit };
  }
  return { kind: "other" };
}

const RANK: Record<PermissionOption["kind"], number> = {
  allow_once: 0,
  allow_always: 1,
  reject_once: 2,
  reject_always: 3,
};

/** The agent's options in one order on every card — allow, allow and
 *  remember, deny — so the default answer is always the first button. An
 *  agent's own order survives within a kind. */
export function orderedOptions(options: readonly PermissionOption[]): PermissionOption[] {
  return options
    .map((option, i) => ({ option, i }))
    .sort((a, b) => (RANK[a.option.kind] ?? 9) - (RANK[b.option.kind] ?? 9) || a.i - b.i)
    .map(({ option }) => option);
}
