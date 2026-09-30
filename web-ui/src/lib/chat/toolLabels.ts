/**
 * The one-line title of a collapsed tool group. The agent's own past-tense
 * batch labels come first (claude `tool_use_summary`: "Listed files in
 * directory"); calls not labelled yet — the newest batch, whose label lands a
 * model round late — or never labelled (codex, labels switched off) read as
 * plain counts: "Ran 2 commands, read a file". Still-running calls are said in
 * the present tense, so a live group never claims work it has not finished.
 */

/** The fields of a tool row the title reads. */
export interface LabelledTool {
  tool: string;
  status: string;
  locations: string[];
  summary: string | null;
  /** The row's title ("Agent: Measure file sizes") — names a lone agent. */
  title?: string;
}

interface Phrase {
  one: string;
  many: (n: number) => string;
}

/** Past and present forms per tool kind, in the order they read best. */
const KINDS: { kind: string; done: Phrase; live: Phrase }[] = [
  {
    kind: "execute",
    done: { one: "ran a command", many: (n) => `ran ${n} commands` },
    live: { one: "running a command", many: (n) => `running ${n} commands` },
  },
  {
    kind: "edit",
    done: { one: "edited a file", many: (n) => `edited ${n} files` },
    live: { one: "editing a file", many: (n) => `editing ${n} files` },
  },
  {
    kind: "read",
    done: { one: "read a file", many: (n) => `read ${n} files` },
    live: { one: "reading a file", many: (n) => `reading ${n} files` },
  },
  {
    kind: "search",
    done: { one: "ran a search", many: (n) => `ran ${n} searches` },
    live: { one: "searching", many: (n) => `running ${n} searches` },
  },
  {
    kind: "fetch",
    done: { one: "fetched a page", many: (n) => `fetched ${n} pages` },
    live: { one: "fetching a page", many: (n) => `fetching ${n} pages` },
  },
  {
    kind: "agent",
    done: { one: "ran an agent", many: (n) => `ran ${n} agents` },
    live: { one: "running an agent", many: (n) => `running ${n} agents` },
  },
  {
    kind: "other",
    done: { one: "used a tool", many: (n) => `used ${n} tools` },
    live: { one: "using a tool", many: (n) => `using ${n} tools` },
  },
];

const KNOWN = new Set(KINDS.map((k) => k.kind).filter((k) => k !== "other"));

// --- agent communication (the chimaera MCP server) ---------------------------

/** The agent-communication tools every agent has (plan §3), in the order a
 *  group title says them. */
const COMMS_VERBS = ["send_message", "read_agent", "read_messages", "list_agents"] as const;
export type CommsVerb = (typeof COMMS_VERBS)[number];

export interface CommsCall {
  verb: CommsVerb;
  /** Who: `send_message`'s `to`, `read_agent`'s `agent`; null = not known. */
  target: string | null;
}

/** How the drivers title an MCP call — claude `send_message (chimaera)`,
 *  codex `chimaera.send_message` (an item) or `chimaera · send_message` (an
 *  approval) — plus the raw `mcp__chimaera__send_message` name. */
const COMMS_TITLE = [
  /^mcp__chimaera__([a-z_]+)(.*)$/,
  /^([a-z_]+) \(chimaera\)(.*)$/,
  /^chimaera(?:\.| · )([a-z_]+)(.*)$/,
];
/** An argument the title carries after the name: `→ loader refactor` (the
 *  claude teammate-message shape, a `: text` after it dropped) or `: s-1a2b`. */
const COMMS_TARGET = /^\s*(?:(?:→|->)\s*(.+?)(?::\s.*)?|:\s*(.+))$/;

function isCommsVerb(v: string): v is CommsVerb {
  return (COMMS_VERBS as readonly string[]).includes(v);
}

function cleanTarget(raw: string | undefined | null): string | null {
  const t = (raw ?? "").trim().replace(/^["'“]|["'”]$/g, "").trim();
  return t === "" ? null : t;
}

/** The agent-communication call a tool row is, from its title (and its
 *  input when the caller has it); null for every other tool. */
export function commsCall(title: string, input?: Record<string, unknown> | null): CommsCall | null {
  for (const re of COMMS_TITLE) {
    const m = re.exec(title.trim());
    if (m === null || !isCommsVerb(m[1])) continue;
    const verb = m[1];
    const arg = verb === "send_message" ? input?.to : verb === "read_agent" ? input?.agent : undefined;
    const fromTitle = COMMS_TARGET.exec(m[2] ?? "");
    const target =
      typeof arg === "string"
        ? cleanTarget(arg)
        : verb === "send_message" || verb === "read_agent"
          ? cleanTarget(fromTitle?.[1] ?? fromTitle?.[2])
          : null;
    return { verb, target };
  }
  return null;
}

/** A recipient as a sentence says it: the two reserved addresses in words. */
function targetLabel(target: string): string {
  if (target === "mastermind") return "the Mastermind";
  return target;
}

/** A comms call's card title: "Message to loader refactor", "Checked
 *  messages", "Listed agents", "Read loader refactor's work" — or, when the
 *  title named no one, "Sent a message", "Read another agent's work". */
export function commsTitle(call: CommsCall): string {
  switch (call.verb) {
    case "send_message":
      // The drivers' MCP titles carry no arguments today; the recipient
      // shows once one does (`send_message (chimaera) → fix CI`).
      return call.target !== null ? `Message to ${targetLabel(call.target)}` : "Sent a message";
    case "read_messages":
      return "Checked messages";
    case "list_agents":
      return "Listed agents";
    case "read_agent":
      return call.target !== null ? `Read ${targetLabel(call.target)}'s work` : "Read another agent's work";
  }
}

/** A tool row's title as the card shows it: agent-communication calls in
 *  words, every other tool as the driver titled it. */
export function readableToolTitle(t: { tool: string; title: string }): string {
  if (t.tool !== "other") return t.title;
  const call = commsCall(t.title);
  return call === null ? t.title : commsTitle(call);
}

interface CommsPhrase extends Phrase {
  /** A lone call whose target is known names it. */
  named?: (target: string) => string;
}

const COMMS_PHRASES: Record<CommsVerb, { done: CommsPhrase; live: CommsPhrase }> = {
  send_message: {
    done: { one: "sent a message", many: (n) => `sent ${n} messages`, named: (t) => `messaged ${t}` },
    live: { one: "sending a message", many: (n) => `sending ${n} messages`, named: (t) => `messaging ${t}` },
  },
  read_agent: {
    done: { one: "read an agent's work", many: (n) => `read ${n} agents' work`, named: (t) => `read ${t}'s work` },
    live: {
      one: "reading an agent's work",
      many: (n) => `reading ${n} agents' work`,
      named: (t) => `reading ${t}'s work`,
    },
  },
  read_messages: {
    done: { one: "checked messages", many: (n) => `checked messages ${n} times` },
    live: { one: "checking messages", many: () => "checking messages" },
  },
  list_agents: {
    done: { one: "listed agents", many: (n) => `listed agents ${n} times` },
    live: { one: "listing agents", many: () => "listing agents" },
  },
};

/** The comms calls among `tools`, as phrases (all of one tense). */
function commsPhrases(tools: LabelledTool[], live: boolean): string[] {
  const calls = tools.flatMap((t) => {
    const c = t.tool === "other" && t.title !== undefined ? commsCall(t.title) : null;
    return c === null ? [] : [c];
  });
  const out: string[] = [];
  for (const verb of COMMS_VERBS) {
    const of = calls.filter((c) => c.verb === verb);
    if (of.length === 0) continue;
    const p = COMMS_PHRASES[verb][live ? "live" : "done"];
    const target = of.length === 1 ? of[0].target : null;
    out.push(target !== null && p.named !== undefined ? p.named(targetLabel(target)) : of.length === 1 ? p.one : p.many(of.length));
  }
  return out;
}

function isCommsTool(t: LabelledTool): boolean {
  return t.tool === "other" && t.title !== undefined && commsCall(t.title) !== null;
}

/** Still running — said in the present tense, and a live dot on its line. */
export function isLive(t: { status: string }): boolean {
  return t.status === "pending" || t.status === "in_progress";
}

/** How many of `tools` a phrase counts: edits by distinct file (an edit
 *  without a path still counts once), everything else by call. */
function count(tools: LabelledTool[]): number {
  const files = new Set<string>();
  let pathless = 0;
  for (const t of tools) {
    if (t.tool === "edit" && t.locations.length > 0) for (const l of t.locations) files.add(l);
    else pathless++;
  }
  return files.size + pathless;
}

/** "ran 2 commands, reading a file" — lowercase, finished work first. */
export function countPhrase(tools: LabelledTool[]): string {
  const parts: string[] = [];
  for (const live of [false, true]) {
    for (const k of KINDS) {
      // Agent-communication calls say what they did, ahead of the
      // catch-all "used a tool".
      if (k.kind === "other") parts.push(...commsPhrases(tools.filter((t) => isLive(t) === live), live));
      const of = tools.filter(
        (t) =>
          isLive(t) === live &&
          (k.kind === "other" ? !KNOWN.has(t.tool) && !isCommsTool(t) : t.tool === k.kind),
      );
      const n = count(of);
      if (n === 0) continue;
      const p = live ? k.live : k.done;
      // A lone agent is named: which delegation is running matters more
      // than that one is.
      const agentName =
        k.kind === "agent" && n === 1 ? of[0].title?.replace(/^(Agent|Task): /, "") : undefined;
      if (agentName !== undefined && agentName !== "") {
        parts.push(`${live ? "running" : "ran"} agent “${agentName}”`);
        continue;
      }
      parts.push(n === 1 ? p.one : p.many(n));
    }
  }
  return parts.join(", ");
}

export function capitalize(s: string): string {
  return s.length > 0 ? s[0].toUpperCase() + s.slice(1) : s;
}

/** The fields of a tool row its run's health reads. */
export interface HealthTool {
  tool: string;
  title: string;
  status: string;
  locations: string[];
  denied: boolean;
  /** The shell command the call ran; absent on execute-kind calls that run
   *  none (claude's BashOutput / KillShell) and on pre-field journals. */
  command?: string | null;
}

/** The rest of a run's turn: `tools[from..]` are the calls made after it.
 *  Thought rows split one turn's calls into several groups, and the retry
 *  that fixed a failure usually lands in a later one. */
export interface TurnTail {
  tools: readonly HealthTool[];
  from: number;
}

/** Whether `later` completing undoes `failed`. Commands have no target to
 *  match (the title is the command line, which a retry almost always
 *  rewrites), so any later successful command counts — one that ran a
 *  command, not a background-output read or a kill. Everything else must
 *  hit the same file, or failing that the same title. */
function recovers(failed: HealthTool, later: HealthTool): boolean {
  if (later.status !== "completed" || later.denied || later.tool !== failed.tool) return false;
  if (failed.tool === "execute" && typeof later.command === "string") return true;
  return failed.locations.length > 0
    ? later.locations.some((l) => failed.locations.includes(l))
    : later.title === failed.title;
}

/** A run's failure badge. A failure is RECOVERED when a later call in the
 *  same turn made up for it (the read-before-write dance, a retried or
 *  reworked command) — a net-success run shouldn't wear the hard red badge.
 *  Presentation only: the failed row inside still shows its own error.
 *  Denials never recover (the user said no); a failure with no matching
 *  later success stays hard. Later calls in the run itself count, and so do
 *  those in `tail`. */
export function toolRunHealth(tools: HealthTool[], tail?: TurnTail): "failed" | "recovered" | null {
  const failed = tools.some((t, i) => {
    if (t.denied) return true;
    if (t.status !== "failed") return false;
    if (tools.some((s, j) => j > i && recovers(t, s))) return false;
    if (tail === undefined) return true;
    for (let j = tail.from; j < tail.tools.length; j++) if (recovers(t, tail.tools[j])) return false;
    return true;
  });
  if (failed) return "failed";
  return tools.some((t) => t.status === "failed" || t.denied) ? "recovered" : null;
}

export function toolGroupTitle(tools: LabelledTool[]): string {
  const labels: string[] = [];
  let lastLabelled = -1;
  tools.forEach((t, i) => {
    if (t.summary === null || t.summary === "") return;
    if (!labels.includes(t.summary)) labels.push(t.summary);
    lastLabelled = i;
  });
  const rest = countPhrase(tools.slice(lastLabelled + 1));
  if (labels.length === 0) return capitalize(rest);
  return rest === "" ? labels.join(" · ") : `${labels.join(" · ")} · ${rest}`;
}
