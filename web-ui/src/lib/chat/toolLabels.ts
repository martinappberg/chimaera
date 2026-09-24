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
      const of = tools.filter(
        (t) =>
          isLive(t) === live &&
          (k.kind === "other" ? !KNOWN.has(t.tool) : t.tool === k.kind),
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
}

/** A run's failure badge. A failure is RECOVERED when a later call of the
 *  same tool against the same target completed (the read-before-write dance,
 *  a retried command) — a net-success run shouldn't wear the hard red badge.
 *  Presentation only: the failed row inside still shows its own error.
 *  Denials never recover (the user said no); a failure with no matching
 *  later success stays hard. */
export function toolRunHealth(tools: HealthTool[]): "failed" | "recovered" | null {
  const failed = tools.some((t, i) => {
    if (t.denied) return true;
    if (t.status !== "failed") return false;
    const sameTarget = (s: HealthTool) =>
      s.tool === t.tool &&
      (t.locations.length > 0 ? s.locations.some((l) => t.locations.includes(l)) : s.title === t.title);
    return !tools.some((s, j) => j > i && s.status === "completed" && !s.denied && sameTarget(s));
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
