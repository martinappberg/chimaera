/**
 * Messages from other agents, as the reader's transcript shows them. The
 * daemon hands an agent its peers' messages in one text format
 * (docs/agent-communication-plan.md §12): each message is a bracketed header
 * line, then its body — a peer's quoted line by line (`> `), the
 * Mastermind's not. A send that starts a turn leads with one bracketed line
 * saying why. This module turns that text (and the hook-delivered
 * `agent_message` event) into cards. Pure: the reducer and its tests share it.
 *
 * Everything here is attacker-influenced text (a peer may have read a
 * poisoned page): it is only ever split and labelled here, and rendered by
 * the sanitizing renderers.
 */

/** Who a message was addressed to, from the header's own words. */
export type AgentMessageTo = "you" | "everyone" | "mastermind";

export interface AgentMessage {
  /** The Timeline seq: the message's id (`#12`). */
  id: number;
  fromName: string;
  fromSid: string;
  /** The sender's vendor ("claude", "codex"); null = not said, or a word
   *  with no mark. */
  fromAgent: string | null;
  /** Null when the header named no recipient (a Mastermind's direction). */
  to: AgentMessageTo | null;
  /** From the workspace Mastermind: direction, not a peer's information. */
  mastermind: boolean;
  /** The message this one answers, when it is a reply. */
  replyTo: number | null;
  /** The body as its sender wrote it (a peer's quoting removed). */
  body: string;
}

export interface ParsedAgentText {
  /** The leading "why" line, brackets removed (`chimaera delivered these
   *  while you were idle: …`); null when the text has none. */
  caption: string | null;
  /** In order. Empty = no header parsed: show the raw text instead. */
  messages: AgentMessage[];
}

/** `[message #12 from "name" (s-1a2b, claude) to you — …]`, with `from the
 *  workspace Mastermind "name"` for the Mastermind. Names never hold a `"`
 *  (the daemon replaces it with `'`), so the quotes delimit them; the vendor
 *  is optional so a header without one still parses. */
const HEADER =
  /^\[message #(\d+) from (the workspace Mastermind )?"([^"\n]*)" \(([^,()\s]+)(?:, ?([^()\s]+))?\)(.*)\]\s*$/;
/** The recipient, right after the sender. */
const TO = /^\s*to (you|everyone|the Mastermind)\b/;
/** A reply marker anywhere in the header (`re #10`, `in reply to #10`). The
 *  header's own instruction ("reply_to 12") carries no `#`, so it can't match. */
const REPLY = /\b(?:re|reply to|in reply to|replying to|answering)\s+#(\d+)\b/i;

/** The vendors that have a mark (`SessionGlyph`); any other word — the
 *  daemon writes "agent" when it doesn't know — shows none rather than a
 *  wrong one. */
const VENDORS = new Set(["claude", "codex", "agy", "gemini"]);

/** A sender's vendor worth a mark, or null. */
export function knownVendor(agent: string | null | undefined): string | null {
  return typeof agent === "string" && VENDORS.has(agent) ? agent : null;
}

/** One header line, parsed; null when the line is not a header. */
export function parseHeader(line: string): Omit<AgentMessage, "body"> | null {
  const m = HEADER.exec(line);
  if (m === null) return null;
  const rest = m[6] ?? "";
  const to = TO.exec(rest)?.[1];
  const reply = REPLY.exec(rest);
  return {
    id: Number(m[1]),
    fromName: m[3],
    fromSid: m[4],
    fromAgent: knownVendor(m[5]),
    to: to === undefined ? null : to === "the Mastermind" ? "mastermind" : (to as AgentMessageTo),
    mastermind: m[2] !== undefined,
    replyTo: reply === null ? null : Number(reply[1]),
  };
}

/** Drop leading and trailing blank lines (inner ones are the body's). */
function trimBlankLines(lines: string[]): string[] {
  let start = 0;
  let end = lines.length;
  while (start < end && lines[start].trim() === "") start++;
  while (end > start && lines[end - 1].trim() === "") end--;
  return lines.slice(start, end);
}

/** A peer's body with its `> ` quoting removed — only where present, so a
 *  body the daemon forgot to quote still reads whole. */
function unquote(lines: string[]): string[] {
  return lines.map((l) => (l.startsWith("> ") ? l.slice(2) : l === ">" ? "" : l));
}

/** The why-line as a caption: one bracketed line loses its brackets; any
 *  other leading text is kept as written, so nothing is dropped. */
function captionOf(lines: string[]): string | null {
  const kept = trimBlankLines(lines);
  if (kept.length === 0) return null;
  const text = kept.join("\n").trim();
  const bracketed = /^\[([\s\S]*)\]$/.exec(text);
  return bracketed !== null && !bracketed[1].includes("\n") ? bracketed[1].trim() : text;
}

/**
 * Split a delivered text into its messages. A line is a header only when it
 * starts the line unquoted, so a peer quoting a header-shaped line inside its
 * body (every body line starts `> `) can't forge a second card. Text with no
 * header parses to no messages — the caller shows it whole.
 */
export function parseAgentText(text: string): ParsedAgentText {
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  const messages: AgentMessage[] = [];
  const lead: string[] = [];
  let current: Omit<AgentMessage, "body"> | null = null;
  let body: string[] = [];
  const close = () => {
    if (current === null) return;
    const kept = trimBlankLines(current.mastermind ? body : unquote(body));
    messages.push({ ...current, body: kept.join("\n") });
  };
  for (const line of lines) {
    const header = parseHeader(line);
    if (header !== null) {
      close();
      current = header;
      body = [];
    } else if (current === null) {
      lead.push(line);
    } else {
      body.push(line);
    }
  }
  close();
  if (messages.length === 0) return { caption: null, messages };
  return { caption: captionOf(lead), messages };
}

/** The hook-delivered journal event (`agent_message`) as a card; null when
 *  its required fields are missing (a corrupt line renders nothing). */
export function agentMessageFromEvent(ev: Record<string, unknown>): AgentMessage | null {
  if (typeof ev.message !== "number" || typeof ev.text !== "string") return null;
  const mastermind = ev.mastermind === true;
  return {
    id: ev.message,
    fromName: typeof ev.from_name === "string" ? ev.from_name : "an agent",
    fromSid: typeof ev.from_sid === "string" ? ev.from_sid : "",
    fromAgent: knownVendor(ev.from_agent as string | undefined),
    to: ev.broadcast === true ? "everyone" : "you",
    mastermind,
    replyTo: typeof ev.reply_to === "number" ? ev.reply_to : null,
    body: ev.text,
  };
}

/** A user-message origin that carries other agents' messages. */
export function isAgentOrigin(origin: string | null): origin is "agent" | "mastermind" {
  return origin === "agent" || origin === "mastermind";
}
