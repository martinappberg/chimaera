/** The chimaera MCP tool an agent uses to propose a project's setup command.
 *  Claude Code names MCP tools `mcp__<server>__<tool>` and carries the input
 *  (`tool_render_data`); Codex reports neither, so its proposals are not
 *  placed in the conversation (the input is not on the wire). */
const PROPOSE = "mcp__chimaera__update_cloud_profile";
/** The daemon's own cap on a setup command (`CloudProfile::validate`). */
const MAX_BYTES = 16 * 1024;

interface ToolLike { kind: string; nativeName?: string; nativeInput?: unknown }

/** The setup commands this conversation proposed, oldest first and without
 *  repeats: exactly the text the agent sent, never reformatted, since a
 *  decision may only apply to the command the user is shown. */
export function conversationProposals(blocks: readonly ToolLike[]): string[] {
  const out: string[] = [];
  for (const block of blocks) {
    if (block.kind !== "tool" || block.nativeName !== PROPOSE) continue;
    const input = block.nativeInput;
    if (typeof input !== "object" || input === null) continue;
    const command = (input as { setup_command?: unknown }).setup_command;
    if (typeof command !== "string" || command.trim() === "" || command.length > MAX_BYTES) continue;
    const at = out.indexOf(command);
    if (at !== -1) out.splice(at, 1);
    out.push(command);
  }
  return out;
}

/** Same list, same order: lets the slot skip republishing on every block. */
export function sameProposals(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((value, index) => value === b[index]);
}
