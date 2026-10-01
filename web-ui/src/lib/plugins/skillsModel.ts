/**
 * Pure derivations for the Skills view (design §6.4): grouping by where a
 * skill comes from (that is what says who else gets it), per-agent counts,
 * the "only one agent" filter, and the search. Unit-tested in
 * skillsModel.test.ts.
 */
import { agentName, type AgentId, type Skill, type SkillSource } from "./store";

export type SkillGroupKey = "project" | "plugin" | "user" | `builtin-${string}`;

export interface SkillGroup {
  key: SkillGroupKey;
  label: string;
  /** The quiet subtitle after the label. */
  hint: string;
  skills: Skill[];
}

const GROUP_ORDER: SkillGroupKey[] = ["project", "plugin", "user", "builtin-claude", "builtin-codex"];

const GROUP_LABELS: Record<SkillGroupKey, { label: string; hint: string }> = {
  project: { label: "This project", hint: "travels with the repository" },
  plugin: { label: "From plugins", hint: "" },
  user: { label: "Yours", hint: "every project on this host sees these" },
  "builtin-claude": { label: "Built into claude", hint: "" },
  "builtin-codex": { label: "Built into codex", hint: "" },
};

/** Where a skill comes from: this project, a plugin, the user, or the agent
 *  itself (claude's live catalog, codex's system skills). */
export function groupOf(source: SkillSource | string): "project" | "plugin" | "user" | "builtin" {
  switch (source) {
    case "project":
      return "project";
    case "plugin":
      return "plugin";
    case "user":
      return "user";
    default:
      return "builtin";
  }
}

export function usable(skill: Skill, agent: AgentId): boolean {
  return skill.agents[agent]?.state === "available";
}

export interface SkillCounts {
  total: number;
  claude: number;
  codex: number;
  both: number;
}

export function skillCounts(skills: readonly Skill[]): SkillCounts {
  let claude = 0;
  let codex = 0;
  let both = 0;
  for (const s of skills) {
    const c = usable(s, "claude");
    const x = usable(s, "codex");
    if (c) claude += 1;
    if (x) codex += 1;
    if (c && x) both += 1;
  }
  return { total: skills.length, claude, codex, both };
}

export type SkillFilter = string;

export function filterSkills(skills: readonly Skill[], filter: SkillFilter, query: string): Skill[] {
  const q = query.trim().toLowerCase();
  return skills.filter((s) => {
    if (filter !== "all" && !usable(s, filter)) return false;
    if (q === "") return true;
    return (
      s.name.toLowerCase().includes(q) ||
      s.description.toLowerCase().includes(q) ||
      (s.plugin ?? "").toLowerCase().includes(q)
    );
  });
}

/** Grouped in a fixed order; empty groups don't render. Within a group the
 *  daemon's order holds (it already sorts by name). A built-in skill sits
 *  under each agent it is built into (the one that lists it), so `/name`
 *  chips stay under claude and `$name` under codex. */
export function groupSkills(skills: readonly Skill[], host = ""): SkillGroup[] {
  const buckets = new Map<SkillGroupKey, Skill[]>();
  const put = (k: SkillGroupKey, s: Skill) => {
    const list = buckets.get(k);
    if (list === undefined) buckets.set(k, [s]);
    else list.push(s);
  };
  for (const s of skills) {
    const g = groupOf(s.source);
    if (g !== "builtin") {
      put(g, s);
      continue;
    }
    for (const [agent, state] of Object.entries(s.agents)) {
      if (state.state !== "absent") put(`builtin-${agent}`, s);
    }
  }
  const out: SkillGroup[] = [];
  for (const key of [...GROUP_ORDER, ...[...buckets.keys()].filter(k => !GROUP_ORDER.includes(k))]) {
    const list = buckets.get(key);
    if (list === undefined || list.length === 0) continue;
    const base = GROUP_LABELS[key] ?? {label: `Built into ${agentName(key.slice(8))}`, hint: ""};
    let hint = base.hint;
    if (key === "user" && host !== "") hint = `every project on ${host} sees these`;
    if (key === "plugin") {
      const plugins = [...new Set(list.map((s) => s.plugin).filter((p): p is string => !!p))];
      hint = plugins.join(" · ");
    }
    out.push({ key, label: base.label, hint, skills: list });
  }
  return out;
}

/** The agent's own invocation syntax — canonical vocabulary, never relabeled:
 *  `/name` for claude, `$name` for codex (the daemon's `invoke` wins). */
export function invokeSyntax(skill: Skill, agent: AgentId): string {
  const state = skill.agents[agent];
  if (state?.invoke === null) return "";
  return state?.invoke ?? (agent === "claude" ? `/${skill.name}` : agent === "codex" ? `$${skill.name}` : "");
}

/** The name as the list shows it: under its plugin's own header the plugin
 *  prefix is noise ("mycelium:analyze" → "analyze", "pdf:pdf" → "pdf"). The
 *  full name stays the invocation. */
export function shortName(skill: Skill): string {
  const colon = skill.name.indexOf(":");
  if (colon < 0) return skill.name;
  const prefix = skill.name.slice(0, colon);
  const rest = skill.name.slice(colon + 1);
  return rest !== "" && (skill.plugin === undefined || prefix === skill.plugin || prefix === rest)
    ? rest
    : skill.name;
}

export interface PluginSection {
  plugin: string;
  skills: Skill[];
  /** Agents that can use at least one of its skills. */
  agents: AgentId[];
}

/** Plugin skills, one section per plugin (by name) — a plugin is the unit
 *  you install, so it is the unit the list is read by. */
export function pluginSections(skills: readonly Skill[]): PluginSection[] {
  const by = new Map<string, Skill[]>();
  for (const s of skills) {
    if (groupOf(s.source) !== "plugin") continue;
    const plugin = s.plugin ?? (s.name.includes(":") ? s.name.slice(0, s.name.indexOf(":")) : "other");
    const list = by.get(plugin);
    if (list === undefined) by.set(plugin, [s]);
    else list.push(s);
  }
  return [...by.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([plugin, list]) => ({
      plugin,
      skills: list,
      agents: [...new Set(list.flatMap(s => Object.keys(s.agents)))].filter((a) => list.some((s) => usable(s, a))),
    }));
}
