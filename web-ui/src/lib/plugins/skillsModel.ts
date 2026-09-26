/**
 * Pure derivations for the Skills view (design §6.4): grouping by where a
 * skill comes from (that is what says who else gets it), per-agent counts,
 * the "only one agent" filter, and the search. Unit-tested in
 * skillsModel.test.ts.
 */
import type { AgentId, Skill, SkillSource } from "./store";

export type SkillGroupKey = "project" | "plugin" | "user" | "builtin";

export interface SkillGroup {
  key: SkillGroupKey;
  label: string;
  /** The quiet subtitle after the label. */
  hint: string;
  skills: Skill[];
}

const GROUP_ORDER: SkillGroupKey[] = ["project", "plugin", "user", "builtin"];

const GROUP_LABELS: Record<SkillGroupKey, { label: string; hint: string }> = {
  project: { label: "This project", hint: "travels with the repo" },
  plugin: { label: "From plugins", hint: "" },
  user: { label: "Yours", hint: "user-level on this host — every project here sees these" },
  builtin: { label: "Built into the agent", hint: "seen in a running session" },
};

export function groupOf(source: SkillSource | string): SkillGroupKey {
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
  return skill.agents[agent].state === "available";
}

export interface SkillCounts {
  total: number;
  claude: number;
  codex: number;
  both: number;
  /** Usable by exactly one agent. */
  onlyOne: number;
}

export function skillCounts(skills: readonly Skill[]): SkillCounts {
  let claude = 0;
  let codex = 0;
  let both = 0;
  let onlyOne = 0;
  for (const s of skills) {
    const c = usable(s, "claude");
    const x = usable(s, "codex");
    if (c) claude += 1;
    if (x) codex += 1;
    if (c && x) both += 1;
    if (c !== x) onlyOne += 1;
  }
  return { total: skills.length, claude, codex, both, onlyOne };
}

export type SkillFilter = "all" | "claude" | "codex" | "one";

export function filterSkills(skills: readonly Skill[], filter: SkillFilter, query: string): Skill[] {
  const q = query.trim().toLowerCase();
  return skills.filter((s) => {
    if (filter === "claude" && !usable(s, "claude")) return false;
    if (filter === "codex" && !usable(s, "codex")) return false;
    if (filter === "one" && usable(s, "claude") === usable(s, "codex")) return false;
    if (q === "") return true;
    return (
      s.name.toLowerCase().includes(q) ||
      s.description.toLowerCase().includes(q) ||
      (s.plugin ?? "").toLowerCase().includes(q)
    );
  });
}

/** Grouped in a fixed order; empty groups don't render. Within a group the
 *  daemon's order holds (it already sorts by name). */
export function groupSkills(skills: readonly Skill[], host = ""): SkillGroup[] {
  const buckets = new Map<SkillGroupKey, Skill[]>();
  for (const s of skills) {
    const k = groupOf(s.source);
    const list = buckets.get(k);
    if (list === undefined) buckets.set(k, [s]);
    else list.push(s);
  }
  const out: SkillGroup[] = [];
  for (const key of GROUP_ORDER) {
    const list = buckets.get(key);
    if (list === undefined || list.length === 0) continue;
    const base = GROUP_LABELS[key];
    let hint = base.hint;
    if (key === "user" && host !== "") hint = `user-level on ${host} — every project here sees these`;
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
  return skill.agents[agent].invoke ?? (agent === "claude" ? `/${skill.name}` : `$${skill.name}`);
}
