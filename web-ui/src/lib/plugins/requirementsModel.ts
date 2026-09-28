/**
 * The agent-side plugin of a plugin card and of the attach sheet's first
 * step: what the plugin's manifest `requires` (it can't work without) and
 * `recommends` (it is more useful with) from each agent, matched against
 * what the agents themselves report on this host. Pure — no store, no DOM —
 * so the wording rules are unit-tested (`requirementsModel.test.ts`).
 *
 * The rules:
 *  - an agent that isn't installed on this host is never listed; a
 *    requirement none of the listed agents here can meet says so once;
 *  - while the first report is in flight the whole box is one line;
 *  - when the daemon can't ask the agents, a requirement says so honestly
 *    (a recommendation, being optional, says nothing);
 *  - each agent's row is its state in words and at most one action:
 *    "installed 0.7.2" · "not installed" [Install] · "2 hooks not trusted"
 *    [Review] · "installed, disabled".
 */
import type { AgentHook, AgentPlugins, PluginRequirement } from "./store";

/** The agents report's fetch state, as PluginsView tracks it. */
export type AgentsState = "idle" | "loading" | "ok" | "unavailable" | "error";

export type RowKind = "requires" | "recommends";

/** `unknown`: the daemon couldn't ask the agents. */
export type RowStatus = "installed" | "disabled" | "missing" | "unknown";

export type Tone = "good" | "warn" | "neutral";

/** The one thing a row offers: the agent's own install, or reviewing the
 *  codex hooks that wait for the user's trust. */
export type RowAction = "install" | "review" | null;

export interface RequirementRow {
  kind: RowKind;
  agent: string;
  /** The agent plugin id as the manifest names it (`mycelium@mycelium`). */
  id: string;
  marketplace: string;
  /** The id's part before `@`. */
  name: string;
  status: RowStatus;
  /** A short report cannot identify this marketplace; setup must not guess. */
  identityAmbiguous?: boolean;
  /** What the agent reports for its installed copy. */
  version: string | null;
  scope: string | null;
  /** The card's words for this agent's state ("installed 0.7.2"). */
  state: string;
  tone: Tone;
  action: RowAction;
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
  /** One line in place of (or beside) the rows: "asking the agents…", or
   *  the requirement no agent on this host can meet. */
  notice: string | null;
  rows: RequirementRow[];
}

/** Installation presence is independent of enablement, and belongs to the
 *  requested add-on: a different plugin for the same agent cannot complete it. */
export function installationDetected(rows: RequirementRow[], agent: string, id: string): boolean {
  return rows.some((r) => r.agent === agent && r.id === id && (r.status === "installed" || r.status === "disabled"));
}

/** Setup needs an enabled add-on and all mandatory add-ons for that agent.
 *  Missing optional add-ons are fine; an unknown identity is never readiness. */
export function agentsForSetup(model: RequirementsModel, available: string[]): string[] {
  if (model.phase === "none") return available;
  if (model.phase !== "ready") return [];
  return available.filter((agent) => {
    const rows = model.rows.filter((r) => r.agent === agent);
    return rows.some((r) => r.status === "installed") && rows.every(
      (r) => r.status !== "unknown" && (r.kind !== "requires" || r.status === "installed"),
    );
  });
}

export interface RequirementsInput {
  requires: PluginRequirement[];
  recommends: PluginRequirement[];
  /** The plugin's `provides.knowledge` (a string when it provides knowledge). */
  knowledge: string | null | undefined;
  report: AgentPlugins | null;
  state: AgentsState;
}

export const CHECKING = "asking the agents…";

/** The id's part before `@` (how the agents list a marketplace plugin). */
export function baseId(id: string): string {
  return id.split("@")[0];
}

/** "claude" · "claude or codex" · "claude, codex or gemini". */
function joinList(items: string[], word: "or" | "and"): string {
  if (items.length <= 1) return items.join("");
  return `${items.slice(0, -1).join(", ")} ${word} ${items[items.length - 1]}`;
}

function unique(items: string[]): string[] {
  return [...new Set(items)];
}

/** The sentence for a requirement no agent on this host can meet. */
function unmetNotice(requires: PluginRequirement[]): string {
  const agents = unique(requires.map((r) => r.agent));
  if (agents.length === 1) return `It needs ${agents[0]}, which isn't installed on this host.`;
  return agents.length === 2
    ? `It needs ${agents[0]} or ${agents[1]}, and neither is installed on this host.`
    : `It needs ${joinList(agents, "or")}, and none of them is installed on this host.`;
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
    state: "",
    tone: "neutral",
    action: null,
    offerInstall: false,
    untrustedHooks: [],
  };
}

export function requirementsModel(input: RequirementsInput): RequirementsModel {
  const { requires, recommends, report, state } = input;
  if (requires.length === 0 && recommends.length === 0) return { phase: "none", notice: null, rows: [] };

  if (report === null) {
    if (state === "unavailable") {
      return {
        phase: "unavailable",
        notice: null,
        rows: requires.map((r) => ({ ...blankRow("requires", r), state: "can't check on this daemon" })),
      };
    }
    if (state === "error") {
      // The caller shows the error with its retry once, beside these.
      return {
        phase: "error",
        notice: null,
        rows: requires.map((r) => ({ ...blankRow("requires", r), state: "couldn't ask" })),
      };
    }
    return { phase: "checking", notice: CHECKING, rows: [] };
  }

  const here = (agent: string) => report.agents.find((a) => a.agent === agent && a.available) ?? null;

  function row(kind: RowKind, r: PluginRequirement): RequirementRow | null {
    const entry = here(r.agent);
    if (entry === null) return null;
    const base = baseId(r.id);
    const candidateIds = new Set([
      ...[...requires, ...recommends]
        .filter((candidate) => candidate.agent === r.agent && baseId(candidate.id) === base)
        .map((candidate) => candidate.id),
      ...entry.plugins.filter((p) => p.id !== base && baseId(p.id) === base).map((p) => p.id),
    ]);
    const unambiguous = candidateIds.size === 1;
    const baseReport = entry.plugins.find((p) => p.id === base);
    const exact = r.id !== base ? entry.plugins.find((p) => p.id === r.id) : undefined;
    const got = exact ?? (unambiguous ? baseReport : undefined);
    const out = blankRow(kind, r);
    if (got === undefined && baseReport !== undefined) {
      out.identityAmbiguous = true;
      out.state = "marketplace unclear — check in the agent";
      out.tone = "warn";
      return out;
    }
    if (got === undefined) {
      out.status = "missing";
      out.offerInstall = true;
      out.state = "not installed";
      out.tone = kind === "requires" ? "warn" : "neutral";
      out.action = "install";
      return out;
    }
    out.status = got.enabled ? "installed" : "disabled";
    out.version = got.version ?? null;
    out.scope = got.scope ?? null;
    out.untrustedHooks = (entry.hooks ?? []).filter(
      (h) => (h.plugin_id === r.id || (unambiguous && h.plugin_id === base)) && (h.trust === "untrusted" || h.trust === "modified"),
    );
    const n = out.untrustedHooks.length;
    if (!got.enabled) {
      out.state = "installed, disabled";
      out.tone = "warn";
    } else if (n > 0) {
      out.state = `${n} hook${n === 1 ? "" : "s"} not trusted`;
      out.tone = "warn";
      out.action = "review";
    } else {
      out.state = out.version !== null ? `installed ${out.version}` : "installed";
      out.tone = "good";
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

/** The card's "Agent-side plugin" box: one per kind the manifest names. */
export interface AgentSideBlock {
  kind: RowKind;
  /** "Agent-side plugin", or "… · needed" for a requirement. */
  title: string;
  /** The author's sentence (`requires_summary` / `recommends_summary`), or
   *  a plain one when the manifest has none. */
  summary: string;
  /** The agent plugin's own page, when its marketplace names one. */
  link: { label: string; url: string } | null;
  rows: RequirementRow[];
  /** One line in place of the rows ("asking claude and codex…", or a
   *  requirement no agent here can meet). */
  notice: string | null;
}

export interface BlockInput {
  name: string;
  requires: PluginRequirement[];
  recommends: PluginRequirement[];
  requires_summary: string | null;
  recommends_summary: string | null;
  knowledge: string | null | undefined;
}

const GITHUB_SLUG = /^[A-Za-z0-9][A-Za-z0-9_.-]*\/[A-Za-z0-9_.-]+$/;

/** The page an agent marketplace names: `owner/repo` is a GitHub
 *  repository; an http(s) URL is itself. */
export function marketplaceUrl(marketplace: string): string | null {
  const m = marketplace.trim();
  if (GITHUB_SLUG.test(m) && !m.includes("..")) return `https://github.com/${m.replace(/\.git$/, "")}`;
  if (/^https?:\/\/[^\s]+$/i.test(m)) return m;
  return null;
}

/** The box per kind, only when there is something to say: while asking,
 *  when a requirement can't be met, or when an agent that could use the
 *  plugin is installed here. */
export function agentSideBlocks(model: RequirementsModel, p: BlockInput): AgentSideBlock[] {
  if (model.phase === "none") return [];
  const out: AgentSideBlock[] = [];
  for (const kind of ["requires", "recommends"] as const) {
    const named = kind === "requires" ? p.requires : p.recommends;
    if (named.length === 0) continue;
    const rows = model.rows.filter((r) => r.kind === kind);
    let notice: string | null = null;
    if (model.phase === "checking") {
      notice = `asking ${joinList(unique(named.map((r) => r.agent)), "and")}…`;
    } else if (kind === "requires" && model.notice !== null) {
      notice = model.notice;
    }
    if (rows.length === 0 && notice === null) continue;
    const pages = unique(named.map((r) => r.marketplace))
      .map((m) => marketplaceUrl(m))
      .filter((u): u is string => u !== null);
    const names = unique(named.map((r) => baseId(r.id)));
    const authored = kind === "requires" ? p.requires_summary : p.recommends_summary;
    out.push({
      kind,
      title: kind === "requires" ? "Agent-side plugin · needed" : "Agent-side plugin",
      summary: authored ?? fallbackSummary(kind, p),
      link: pages.length > 0 ? { label: `${names[0]} on GitHub`, url: pages[0] } : null,
      rows,
      notice,
    });
  }
  return out;
}

function fallbackSummary(kind: RowKind, p: BlockInput): string {
  if (kind === "requires") return `${p.name} works only when the agents you use have their own plugin for it.`;
  return typeof p.knowledge === "string"
    ? `With their own plugin, your agents can record what they learn as they work; ${p.name} works without it.`
    : `An optional plugin for your agents that makes ${p.name} more useful; ${p.name} works without it.`;
}

/** The attach sheet's wording for a row: a requirement reads as needed, a
 *  recommendation as optional. */
export function sheetText(row: RequirementRow, knowledge: string | null | undefined): string {
  const v = row.version !== null ? ` ${row.version}` : "";
  if (row.identityAmbiguous) return `${row.agent}'s ${row.id}: ${row.state}`;
  if (row.status === "installed") return `${row.agent} has ${row.name}${v} ✓`;
  if (row.status === "disabled") return `${row.agent} has ${row.name}${v}, but it is disabled`;
  if (row.kind === "requires") {
    return row.status === "unknown" && row.state === "can't check on this daemon"
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
