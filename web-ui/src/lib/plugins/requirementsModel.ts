/**
 * The agent-plugin rows of a plugin card and of the attach sheet's first
 * step: what the plugin's manifest `requires` (it can't work without) and
 * `recommends` (it is more useful with) from each agent, matched against
 * what the agents themselves report on this host. Pure — no store, no DOM —
 * so the wording rules are unit-tested (`requirementsModel.test.ts`).
 *
 * The rules:
 *  - an agent that isn't installed on this host is never listed; a
 *    requirement none of the listed agents here can meet says so once;
 *  - while the first report is in flight the whole block is one line;
 *  - when the daemon can't ask the agents, a requirement keeps the honest
 *    "can't check" pill (a recommendation, being optional, says nothing).
 */
import type { AgentHook, AgentPlugins, PluginRequirement } from "./store";

/** The agents report's fetch state, as PluginsView tracks it. */
export type AgentsState = "idle" | "loading" | "ok" | "unavailable" | "error";

export type RowKind = "requires" | "recommends";

/** `unknown`: the daemon couldn't ask the agents. */
export type RowStatus = "installed" | "disabled" | "missing" | "unknown";

export type PillTone = "good" | "warn" | "neutral";

export interface RequirementRow {
  kind: RowKind;
  agent: string;
  /** The agent plugin id as the manifest names it (`mycelium@mycelium`). */
  id: string;
  marketplace: string;
  /** The id's part before `@`. */
  name: string;
  status: RowStatus;
  /** What the agent reports for its installed copy. */
  version: string | null;
  scope: string | null;
  /** The card's sentence for this row. */
  text: string;
  /** The status pill beside a `requires` sentence; null for a
   *  `recommends` row, whose sentence says it all. */
  pill: { text: string; tone: PillTone } | null;
  /** The agent is here and its plugin isn't: offer the agent's own install. */
  offerInstall: boolean;
  /** Codex hooks of this agent plugin still waiting for the user's trust. */
  untrustedHooks: AgentHook[];
}

export interface RequirementsModel {
  /** `none`: the plugin asks nothing of the agents; `checking`: the first
   *  report is in flight; `unavailable`: the daemon can't ask the agents;
   *  `error`: asking failed and there is no earlier report to show. */
  phase: "none" | "checking" | "unavailable" | "error" | "ready";
  /** One line in place of (or beside) the rows: "checking the agents…", or
   *  the requirement no agent on this host can meet. */
  notice: string | null;
  rows: RequirementRow[];
}

export interface RequirementsInput {
  requires: PluginRequirement[];
  recommends: PluginRequirement[];
  /** The plugin's `provides.knowledge` (a string when it provides knowledge). */
  knowledge: string | null | undefined;
  report: AgentPlugins | null;
  state: AgentsState;
}

export const CHECKING = "checking the agents…";

/** The id's part before `@` (how the agents list a marketplace plugin). */
export function baseId(id: string): string {
  return id.split("@")[0];
}

/** "claude" · "claude or codex" · "claude, codex or gemini". */
function orList(items: string[]): string {
  if (items.length <= 1) return items.join("");
  return `${items.slice(0, -1).join(", ")} or ${items[items.length - 1]}`;
}

function unique(items: string[]): string[] {
  return [...new Set(items)];
}

/** The sentence for a requirement no agent on this host can meet, naming
 *  the agents the manifest lists. */
function unmetNotice(requires: PluginRequirement[]): string {
  const agents = unique(requires.map((r) => r.agent));
  const ids = unique(requires.map((r) => r.id));
  const what =
    ids.length === 1
      ? `Requires ${orList(agents)} with plugin ${ids[0]}`
      : `Requires ${orList(requires.map((r) => `${r.agent} with plugin ${r.id}`))}`;
  const none =
    agents.length === 1
      ? `${agents[0]} isn't installed on this host`
      : agents.length === 2
        ? "neither is installed on this host"
        : "none of them is installed on this host";
  return `${what}; ${none}`;
}

function blankRow(kind: RowKind, r: PluginRequirement): RequirementRow {
  return {
    kind,
    agent: r.agent,
    id: r.id,
    marketplace: r.marketplace,
    name: baseId(r.id),
    status: "unknown",
    version: null,
    scope: null,
    text: kind === "requires" ? `Requires the ${r.agent} plugin ${r.id}` : `For ${r.agent}: ${baseId(r.id)}`,
    pill: null,
    offerInstall: false,
    untrustedHooks: [],
  };
}

export function requirementsModel(input: RequirementsInput): RequirementsModel {
  const { requires, recommends, knowledge, report, state } = input;
  if (requires.length === 0 && recommends.length === 0) return { phase: "none", notice: null, rows: [] };

  if (report === null) {
    if (state === "unavailable") {
      return {
        phase: "unavailable",
        notice: null,
        rows: requires.map((r) => ({
          ...blankRow("requires", r),
          pill: { text: `${r.agent} · can't check on this daemon`, tone: "neutral" },
        })),
      };
    }
    if (state === "error") {
      // The caller shows the error with its retry once, beside these.
      return { phase: "error", notice: null, rows: requires.map((r) => blankRow("requires", r)) };
    }
    return { phase: "checking", notice: CHECKING, rows: [] };
  }

  const here = (agent: string) => report.agents.find((a) => a.agent === agent && a.available) ?? null;

  function row(kind: RowKind, r: PluginRequirement): RequirementRow | null {
    const entry = here(r.agent);
    if (entry === null) return null;
    const base = baseId(r.id);
    const got = entry.plugins.find((p) => p.id === r.id || p.id === base) ?? null;
    const out = blankRow(kind, r);
    if (got === null) {
      out.status = "missing";
      out.offerInstall = true;
    } else {
      out.status = got.enabled ? "installed" : "disabled";
      out.version = got.version ?? null;
      out.scope = got.scope ?? null;
      out.untrustedHooks = (entry.hooks ?? []).filter(
        (h) => (h.plugin_id === r.id || h.plugin_id === base) && (h.trust === "untrusted" || h.trust === "modified"),
      );
    }
    const v = out.version !== null ? ` ${out.version}` : "";
    if (kind === "requires") {
      out.pill =
        out.status === "missing"
          ? { text: `${r.agent} · plugin not installed`, tone: "warn" }
          : out.status === "installed"
            ? { text: `${r.agent} · plugin${v} ✓`, tone: "good" }
            : { text: `${r.agent} · plugin${v} · disabled`, tone: "warn" };
    } else {
      out.text =
        out.status === "missing"
          ? `For ${r.agent}: install the ${r.id} plugin${typeof knowledge === "string" ? " so it can record knowledge" : ""}`
          : out.status === "installed"
            ? `For ${r.agent}: ${base} ✓${v}`
            : `For ${r.agent}: ${base}${v} is installed but disabled`;
    }
    return out;
  }

  const rows: RequirementRow[] = [];
  for (const r of requires) {
    const x = row("requires", r);
    if (x !== null) rows.push(x);
  }
  const notice = requires.length > 0 && rows.length === 0 ? unmetNotice(requires) : null;
  for (const r of recommends) {
    const x = row("recommends", r);
    if (x !== null) rows.push(x);
  }
  return { phase: "ready", notice, rows };
}

/** The attach sheet's wording for a row: a requirement reads as needed, a
 *  recommendation as optional. */
export function sheetText(row: RequirementRow, knowledge: string | null | undefined): string {
  const v = row.version !== null ? ` ${row.version}` : "";
  if (row.status === "installed") return `${row.agent} has ${row.name}${v} ✓`;
  if (row.status === "disabled") return `${row.agent} has ${row.name}${v}, but it is disabled`;
  if (row.kind === "requires") {
    return row.status === "unknown" && row.pill !== null
      ? `${row.agent} needs the ${row.id} plugin — can't check on this daemon`
      : `${row.agent} needs the ${row.id} plugin`;
  }
  return typeof knowledge === "string"
    ? `optional: the ${row.id} plugin lets ${row.agent} record knowledge as it works`
    : `optional: the ${row.id} plugin for ${row.agent}`;
}

/** Every codex hook of the plugin's agent plugins still waiting for trust
 *  (both requirements and recommendations), each once. */
export function hooksAwaitingTrust(model: RequirementsModel): AgentHook[] {
  const seen = new Set<string>();
  const out: AgentHook[] = [];
  for (const r of model.rows) {
    for (const h of r.untrustedHooks) {
      if (seen.has(h.key)) continue;
      seen.add(h.key);
      out.push(h);
    }
  }
  return out;
}
