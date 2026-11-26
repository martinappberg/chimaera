/**
 * References to other agents in a message: `@agent:SESSION_ID` in the text
 * the agent receives, the agent's name on a chip wherever the user reads it.
 *
 * Chimaera's agents reach each other by session id (`read_agent`,
 * `message_agent`; a name works only while it is unique), so a reference
 * the user makes carries the id: "ask @agent:s-1a2b3c4d what it found" names
 * one session whatever it is called by then. The composer shows the id as
 * `@agent:“name”`, one unit like a linked terminal's chip (the same short
 * forms, `uploadTokens.ts`), and puts the id back in every text that leaves
 * it. A sent message shows the same chip, which opens that agent.
 *
 * App owns who can be mentioned (`mentionableAgents`) and how one is opened
 * (`setAgentOpener`): it knows the sessions, their names, and the layout.
 * Those live in `agentRoster.ts`, which App imports without the chat.
 */

import { get } from "svelte/store";
import { registerReferenceSource, type RefSite, type RefTarget } from "../shared/references";
import { sessionGlyph } from "../shared/SessionGlyph.svelte";
import { agentReference, mentionableAgents, openAgentFrom, type AgentOption } from "./agentRoster";
import { tokenSpans, type UploadTokens } from "./uploadTokens";

export { agentReference, mentionableAgents, openAgent, setAgentOpener, type AgentOption } from "./agentRoster";

/**
 * Every `@agent:` mention in `text`: a bare target up to the next
 * whitespace (trailing sentence punctuation is not part of it), or a quoted
 * one, in straight or curly quotes. The quoted forms are how a name with a
 * space is written by hand.
 */
export function agentMentions(text: string): { start: number; end: number; target: string }[] {
  const out = [];
  for (const m of text.matchAll(/@agent:(?:"([^"]+)"|“([^”]+)”|((?!["“])[^\s]+))/g)) {
    const quoted = m[1] ?? m[2];
    const target = quoted ?? m[3].replace(/[.,;:!?)\]]+$/, "");
    if (target) {
      out.push({
        start: m.index,
        end: m.index + (quoted === undefined ? 7 + target.length : m[0].length),
        target,
      });
    }
  }
  return out;
}

/** The agent a mention names: by id, else by a name only one agent has
 *  (the rule the daemon's own tools apply). Never a guess between two. */
export function mentionedAgent(target: string, agents: readonly AgentOption[]): AgentOption | undefined {
  const exact = agents.find((a) => a.id === target);
  if (exact) return exact;
  const named = agents.filter((a) => a.alive && a.name.toLowerCase() === target.toLowerCase());
  return named.length === 1 ? named[0] : undefined;
}

/** A name that tells two same-named agents apart. */
export function agentLabel(agent: AgentOption, agents: readonly AgentOption[]): string {
  const twin = agents.some((a) => a.id !== agent.id && a.name.toLowerCase() === agent.name.toLowerCase());
  return twin ? `${agent.name} · ${agent.id.replace(/^s-/, "")}` : agent.name;
}

/** Who a chat's composer can mention: the live agents of its workspace,
 *  never itself. */
export function mentionChoices(
  agents: readonly AgentOption[],
  self: string | null,
  workspaceId: string | null,
): AgentOption[] {
  return agents.filter((a) => a.alive && a.id !== self && (workspaceId === null || a.workspaceId === workspaceId));
}

/** The picker's rows for what was typed after `@agent:`. */
export function agentMatches(choices: readonly AgentOption[], query: string, limit = 8): AgentOption[] {
  const q = query.toLowerCase();
  return choices.filter((a) => `${a.name} ${a.id} ${a.agentKind ?? ""}`.toLowerCase().includes(q)).slice(0, limit);
}

/**
 * Show each `@agent:<id>` of a known agent as `@agent:“<label>”`, recording
 * what it stands for in `tokens`. Only ids are shortened: a name typed by
 * hand stays as written, for the receiving agent's own lookup. Curly quotes
 * and never-reassigned short forms for the reasons `terminalMentions.ts`
 * gives (macOS rewrites straight quotes beside typed text; undo must bring
 * back the agent that was picked).
 */
export function collapseAgentMentions(
  text: string,
  agents: readonly AgentOption[],
  tokens: UploadTokens,
  context = "",
): string {
  let out = "";
  let last = 0;
  for (const ref of agentMentions(text)) {
    const agent = agents.find((a) => a.id === ref.target);
    if (!agent) continue;
    const whole = agentReference(agent.id);
    let short = [...tokens].find(([s, full]) => full === whole && s.startsWith("@agent:"))?.[0];
    if (!short) {
      const name = agentLabel(agent, agents).replace(/["'‘’“”\s]+/g, " ").trim() || agent.id;
      short = `@agent:“${name}”`;
      let suffix = 0;
      while (
        (tokens.has(short) && tokens.get(short) !== whole) ||
        tokenSpans(`${context}\n${text}`, new Map([[short, whole]])).length > 0
      ) {
        short = `@agent:“${name} · ${agent.id}${suffix++ ? ` ${suffix}` : ""}”`;
      }
      tokens.set(short, whole);
    }
    out += text.slice(last, ref.start) + short;
    last = ref.end;
  }
  return out + text.slice(last);
}

// --- an agent's own prose ---------------------------------------------------------

/**
 * The tag an agent's id wears in rendered prose: what an agent wrote as
 * `@agent:s-1a2b3c4d` ("I sent your message to @agent:…"; it may, it need
 * not) or as the bare session id reads as that agent's name and opens it.
 * Only an id of an agent in this window's workspace: any other `s-…` stays
 * the text it was.
 */
export function agentTagTargets(id: string, agents: readonly AgentOption[], at?: RefSite): RefTarget[] {
  const mention = id.startsWith("@agent:");
  const sid = mention ? id.slice("@agent:".length) : id;
  const agent = agents.find((a) => a.id === sid);
  if (agent === undefined) return [];
  // A bare id is also a folder and a file name: an agent's uploads live in
  // `…/uploads/<its session id>/`, and its settings in `<id>-settings.json`.
  // There it is part of a path, which must stay whole to become a link.
  if (!mention && at !== undefined && inPath(at.text, at.index, id.length)) return [];
  return [
    {
      key: agent.id,
      kind: "agent",
      title: agent.name,
      label: agentLabel(agent, agents),
      icon: sessionGlyph("agent", agent.agentKind),
      open: (from) => void openAgentFrom(agent.id, from.paneId),
    },
  ];
}

/** Whether `text[index, index + length)` is a segment of a path or the stem
 *  of a file name: a slash on either side, or an extension after it. */
function inPath(text: string, index: number, length: number): boolean {
  const before = text[index - 1] ?? "";
  const after = text.slice(index + length, index + length + 2);
  return before === "/" || before === "\\" || after.startsWith("/") || after.startsWith("\\") || /^\.\w/.test(after);
}

// Registered with the chat's own modules (this one loads with the first
// chat), through the registry every prose surface already links ids with
// (`shared/references.ts`): transcripts, the Timeline, documents.
registerReferenceSource({
  id: "agents",
  shapes: [
    { kind: "agent", pattern: "@agent:s-[0-9a-f]{8}" },
    { kind: "agent", pattern: "s-[0-9a-f]{8}" },
  ],
  root: null,
  lookup: (id, _kind, _near, at) => agentTagTargets(id, get(mentionableAgents), at),
});
