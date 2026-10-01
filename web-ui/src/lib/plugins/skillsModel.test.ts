import { describe, expect, it } from "vitest";
import type { Skill } from "./store";
import { filterSkills, groupSkills, invokeSyntax, pluginSections, shortName, skillCounts } from "./skillsModel";

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
  it("counts per agent and both", () => {
    expect(skillCounts(skills)).toEqual({ total: 7, claude: 6, codex: 3, both: 2 });
  });
});

describe("groupSkills", () => {
  it("groups by origin in a fixed order, dropping empty groups, naming the plugins", () => {
    const groups = groupSkills(skills, "sherlock");
    expect(groups.map((g) => g.key)).toEqual(["project", "plugin", "user", "builtin-claude", "builtin-codex"]);
    expect(groups.map((g) => g.label)).toEqual([
      "This project",
      "From plugins",
      "Yours",
      "Built into claude",
      "Built into codex",
    ]);
    expect(groups[1].hint).toBe("mycelium");
    expect(groups[2].hint).toBe("every project on sherlock sees these");
    expect(groups[3].skills.map((s) => s.name)).toEqual(["code-review"]);
    expect(groups[4].skills.map((s) => s.name)).toEqual(["skill-creator"]);
    expect(groupSkills(skills.filter((s) => s.source === "user")).map((g) => g.key)).toEqual(["user"]);
  });

  it("a built-in either agent lists sits under each; one neither lists goes by its source", () => {
    const both = skill("help", "builtin", "available", "available");
    const orphan = skill("ghost", "system", "absent", "absent");
    const groups = groupSkills([both, orphan]);
    expect(groups.map((g) => [g.key, g.skills.map((s) => s.name)])).toEqual([
      ["builtin-claude", ["help"]],
      ["builtin-codex", ["help"]],
    ]);
  });
});

describe("filterSkills", () => {
  it("filters by agent and by search text", () => {
    expect(filterSkills(skills, "codex", "").map((s) => s.name)).toEqual([
      "qc-report",
      "mycelium:core",
      "skill-creator",
    ]);
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

describe("shortName + pluginSections", () => {
  it("drops the plugin prefix under the plugin's own header", () => {
    expect(shortName(skill("mycelium:analyze", "plugin", "available", "available", "mycelium"))).toBe("analyze");
    expect(shortName(skill("pdf:pdf", "plugin", "absent", "available"))).toBe("pdf");
    expect(shortName(skill("qc-report", "project", "available", "absent"))).toBe("qc-report");
    expect(shortName(skill("other:thing", "plugin", "available", "absent", "mycelium"))).toBe("other:thing");
  });
  it("sections plugin skills by plugin, naming the agents that can use them", () => {
    const sections = pluginSections([
      ...skills,
      skill("pdf:pdf", "plugin", "absent", "available"),
    ]);
    expect(sections.map((x) => [x.plugin, x.skills.length, x.agents])).toEqual([
      ["mycelium", 2, ["claude", "codex"]],
      ["pdf", 1, ["codex"]],
    ]);
  });
});

// Agent identities and invocation syntax come from native inventories.
it("handles future agents without impersonating Codex", () => {
  const item: Skill = {name: "review", description: "Review", source: "builtin", paths: {}, agents: {
    "vendor.pi": {state: "available", invoke: "/review"},
    grok: {state: "available", invoke: null},
  }};
  expect(filterSkills([item], "vendor.pi", "")).toEqual([item]);
  expect(invokeSyntax(item, "vendor.pi")).toBe("/review");
  expect(invokeSyntax(item, "grok")).toBe("");
  expect(invokeSyntax(item, "unknown")).toBe("");
  expect(groupSkills([item]).map(g => g.key)).toEqual(["builtin-vendor.pi", "builtin-grok"]);
});
