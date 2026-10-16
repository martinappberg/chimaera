/** Terminal references keep session identity on the wire and readable names in the UI. */
import { termReference } from "../workspace/agentLinks";
import { tokenSpans, type UploadTokens } from "./uploadTokens";

export interface TerminalOption {
  id: string;
  name: string;
  /** Daemon display name before the UI adds duplicate-name suffixes. */
  referenceName?: string;
  cwd?: string;
  status?: string;
  linkedAgentId?: string;
  linkedAgentName?: string;
}

/** Mirrors the daemon's quoted and unquoted @term: grammar. */
export function terminalMentions(text: string): { start: number; end: number; target: string }[] {
  const out = [];
  for (const m of text.matchAll(/@term:(?:"([^"]+)"|'([^']+)'|((?!["'])[^\s]+))/g)) {
    const quoted = m[1] ?? m[2];
    const target = quoted ?? m[3].replace(/[.,;:!?\)\]]+$/, "");
    if (target) {
      out.push({
        start: m.index,
        end: m.index + (quoted === undefined ? 6 + target.length : m[0].length),
        target,
      });
    }
  }
  return out;
}

/** Legacy names may resolve only when unique. Never guess between two shells. */
export function mentionedTerminal(target: string, terminals: TerminalOption[]): TerminalOption | undefined {
  const exact = terminals.find((t) => t.id === target);
  if (exact) return exact;
  const matches = terminals.filter((t) => (t.referenceName ?? t.name).toLowerCase() === target.toLowerCase());
  return matches.length === 1 ? matches[0] : undefined;
}

export function terminalLabel(terminal: TerminalOption, terminals: TerminalOption[]): string {
  const peers = terminals.filter((t) => t.id !== terminal.id && t.name.toLowerCase() === terminal.name.toLowerCase());
  if (peers.length === 0) return terminal.name;
  const folder = (t: TerminalOption) => t.cwd?.split("/").filter(Boolean).at(-1);
  const directory = folder(terminal);
  const suffix = directory && peers.every((t) => folder(t) !== directory)
    ? directory
    : terminal.id.replace(/^s-/, "");
  return `${terminal.name} · ${suffix}`;
}

export function terminalDescription(t: TerminalOption, agentId: string | null): string {
  const access = t.linkedAgentId === agentId
    ? "Linked here"
    : t.linkedAgentId
      ? `Moves from ${t.linkedAgentName ?? "another agent"} on send`
      : "Links on send";
  return [t.status, access].filter(Boolean).join(" · ");
}

/** Only explicit IDs are shortened. Typed names retain the daemon's ambiguity checks.
 * Short forms are never reassigned: undo must still refer to the originally picked shell. */
export function collapseTerminalMentions(
  text: string,
  terminals: TerminalOption[],
  tokens: UploadTokens,
  context = "",
): string {
  let out = "";
  let last = 0;
  for (const ref of terminalMentions(text)) {
    const terminal = terminals.find((t) => t.id === ref.target);
    if (!terminal) continue;
    const whole = termReference(terminal.id);
    const old = [...tokens].find(([short, full]) => full === whole && short.startsWith("@term:"));
    let short = old?.[0];
    if (!short) {
      const name = terminalLabel(terminal, terminals).replace(/["'\r\n]/g, " ").trim() || terminal.id;
      // Always quote labels: punctuation is part of a name, never a token terminator.
      short = `@term:"${name}"`;
      let suffix = 0;
      while (
        (tokens.has(short) && tokens.get(short) !== whole) ||
        tokenSpans(`${context}\n${text}`, new Map([[short, whole]])).length > 0
      ) {
        short = `@term:"${name} · ${terminal.id}${suffix++ ? ` ${suffix}` : ""}"`;
      }
      tokens.set(short, whole);
    }
    out += text.slice(last, ref.start) + short;
    last = ref.end;
  }
  return out + text.slice(last);
}
