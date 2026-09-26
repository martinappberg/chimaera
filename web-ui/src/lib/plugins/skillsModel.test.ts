import { describe, expect, it } from "vitest";
import type { Skill } from "./store";
import { filterSkills, groupSkills, invokeSyntax, skillCounts } from "./skillsModel";

function skill(
  name: string,
  source: Skill["source"],
  claude: Skill["agents"]["claude"]["state"],
  codex: Skill["agents"]["codex"]["state"],
  plugin?: string,
): Skill {
  return {
    name,
    description: `${name} description`,
    source,
    plugin,
    paths: {},
    agents: { claude: { state: claude }, codex: { state: codex, reason: codex === "off" ? "disabled" : undefined } },
  };
}

const skills: Skill[] = [
  skill("qc-report", "project", "available", "available"),
  skill("figure-style", "project", "available", "absent"),
  skill("mycelium:core", "plugin", "available", "available", "mycelium"),
  skill("mycelium:review", "plugin", "available", "off", "mycelium"),
  skill("lab-notebook", "user", "available", "absent"),
  skill("code-review", "builtin", "available", "absent"),
  skill("skill-creator", "system", "absent", "available"),
];

describe("skillCounts", () => {
  it("counts per agent, both, and only-one", () => {
    expect(skillCounts(skills)).toEqual({ total: 7, claude: 6, codex: 3, both: 2, onlyOne: 5 });
  });
});

describe("groupSkills", () => {
  it("groups by origin in a fixed order, dropping empty groups, naming the plugins", () => {
    const groups = groupSkills(skills, "sherlock");
    expect(groups.map((g) => g.key)).toEqual(["project", "plugin", "user", "builtin"]);
    expect(groups[1].hint).toBe("mycelium");
    expect(groups[2].hint).toContain("sherlock");
    expect(groups[3].skills.map((s) => s.name)).toEqual(["code-review", "skill-creator"]);
    expect(groupSkills(skills.filter((s) => s.source === "user")).map((g) => g.key)).toEqual(["user"]);
  });
});

describe("filterSkills", () => {
  it("filters by agent, by only-one, and by search text", () => {
    expect(filterSkills(skills, "codex", "").map((s) => s.name)).toEqual([
      "qc-report",
      "mycelium:core",
      "skill-creator",
    ]);
    expect(filterSkills(skills, "one", "").length).toBe(5);
    expect(filterSkills(skills, "all", "MYCEL").map((s) => s.name)).toEqual(["mycelium:core", "mycelium:review"]);
    expect(filterSkills(skills, "claude", "notebook").map((s) => s.name)).toEqual(["lab-notebook"]);
  });
});

describe("invokeSyntax", () => {
  it("uses each agent's own syntax, the daemon's invoke winning", () => {
    const s = skill("qc-report", "project", "available", "available");
    expect(invokeSyntax(s, "claude")).toBe("/qc-report");
    expect(invokeSyntax(s, "codex")).toBe("$qc-report");
    s.agents.codex.invoke = "$qc";
    expect(invokeSyntax(s, "codex")).toBe("$qc");
  });
});
