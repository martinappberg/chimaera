import type { SlashCommand } from "./store.svelte";

/** One argument the composer can complete after a native slash command. */
export interface SlashOption {
  value: string;
  label: string;
  description?: string;
}

/** The wire's command catalog plus client-native argument choices. */
export interface ComposerCommand extends SlashCommand {
  options?: SlashOption[];
}

export type SlashContext =
  | { kind: "command"; start: number; text: string }
  | {
      kind: "argument";
      commandStart: number;
      command: ComposerCommand;
      start: number;
      text: string;
    };

export interface SlashChoice {
  key: string;
  label: string;
  description: string;
  command: ComposerCommand;
  option?: SlashOption;
}

/**
 * Find the slash command or first argument under the caret. Whitespace is the
 * boundary deliberately: commands remain discoverable inside prose while a
 * URL/path fragment such as `src/foo` is left alone.
 */
export function slashContextAt(
  draft: string,
  caret: number,
  commands: ComposerCommand[],
): SlashContext | null {
  const prefix = draft.slice(0, Math.max(0, Math.min(caret, draft.length)));
  const commandMatch = /(^|\s)(\/[\w:-]*)$/.exec(prefix);
  if (commandMatch !== null) {
    return {
      kind: "command",
      start: prefix.length - commandMatch[2].length,
      text: commandMatch[2],
    };
  }

  const argumentMatch = /(^|\s)\/([\w:-]+)\s+([^\s]*)$/.exec(prefix);
  if (argumentMatch === null) return null;
  const command = commands.find(
    (candidate) => candidate.name.toLowerCase() === argumentMatch[2].toLowerCase(),
  );
  if (command?.options === undefined || command.options.length === 0) return null;
  return {
    kind: "argument",
    commandStart: argumentMatch.index + argumentMatch[1].length,
    command,
    start: prefix.length - argumentMatch[3].length,
    text: argumentMatch[3],
  };
}

export function slashChoices(
  context: SlashContext | null,
  commands: ComposerCommand[],
  limit = 8,
): SlashChoice[] {
  if (context === null) return [];
  if (context.kind === "command") {
    const query = context.text.slice(1).toLowerCase();
    const seen = new Set<string>();
    return commands
      .filter((command) => {
        const name = command.name.toLowerCase();
        if (!name.startsWith(query) || seen.has(name)) return false;
        seen.add(name);
        return true;
      })
      .slice(0, limit)
      .map((command) => ({
        key: `command:${command.name}`,
        label: `/${command.name}`,
        description: command.description ?? "",
        command,
      }));
  }

  const query = context.text.toLowerCase();
  return (context.command.options ?? [])
    .filter(
      (option) =>
        option.value.toLowerCase().startsWith(query) ||
        option.label.toLowerCase().startsWith(query),
    )
    .slice(0, limit)
    .map((option) => ({
      key: `option:${context.command.name}:${option.value}`,
      label: option.label,
      description: option.description ?? option.value,
      command: context.command,
      option,
    }));
}

export interface SkillBlock extends Record<string, unknown> {
  type: "skill";
  name: string;
  path: string;
}

function regexEscape(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Promote any exact `/skill-name` token into Codex's structured skill input.
 * The original text remains the user-visible message; these extra blocks are
 * the protocol-native selection signal and are de-duplicated/bounded.
 */
export function skillBlocksForText(
  text: string,
  commands: ComposerCommand[],
  limit = 8,
): SkillBlock[] {
  const out: SkillBlock[] = [];
  const seen = new Set<string>();
  for (const command of commands) {
    if (command.skill_path === undefined || out.length >= limit) continue;
    const name = command.name.toLowerCase();
    if (seen.has(name)) continue;
    const token = new RegExp(`(^|\\s)/${regexEscape(command.name)}(?=$|\\s)`, "i");
    if (!token.test(text)) continue;
    seen.add(name);
    out.push({ type: "skill", name: command.name, path: command.skill_path });
  }
  return out;
}
