import { describe, expect, it } from "vitest";
import type { AgentHook, AgentPlugin, AgentPlugins, PluginRequirement } from "./store";
import {
  CHECKING,
  agentSideBlocks,
  hooksAwaitingTrust,
  marketplaceUrl,
  requirementsModel,
  sheetText,
  type BlockInput,
  type RequirementsInput,
} from "./requirementsModel";

const myc = (agent: string): PluginRequirement => ({ agent, id: "mycelium@mycelium", marketplace: "mycelium" });

function report(agents: { agent: string; available?: boolean; plugins?: AgentPlugin[]; hooks?: AgentHook[] }[]): AgentPlugins {
  return {
    schema: 1,
    host: "laptop",
    agents: agents.map((a) => ({ agent: a.agent, available: a.available ?? true, plugins: a.plugins ?? [], hooks: a.hooks })),
  };
}

const mycelium = (enabled = true, id = "mycelium@mycelium"): AgentPlugin => ({ id, version: "0.7.2", scope: "user", enabled });

function input(over: Partial<RequirementsInput>): RequirementsInput {
  return { requires: [], recommends: [], knowledge: null, report: null, state: "ok", ...over };
}

describe("requirementsModel · requires", () => {
  it("says nothing for a plugin that asks nothing of the agents", () => {
    const m = requirementsModel(input({ report: report([{ agent: "claude" }]) }));
    expect(m).toEqual({ phase: "none", notice: null, rows: [] });
  });

  it("an installed requirement: installed and its version, no action", () => {
    const m = requirementsModel(
      input({ requires: [myc("claude")], report: report([{ agent: "claude", plugins: [mycelium()] }]) }),
    );
    expect(m.phase).toBe("ready");
    expect(m.notice).toBeNull();
    expect(m.rows).toHaveLength(1);
    expect(m.rows[0]).toMatchObject({
      kind: "requires",
      status: "installed",
      state: "installed 0.7.2",
      tone: "good",
      action: null,
      offerInstall: false,
    });
  });

  it("matches the agent's listing by the id's part before @", () => {
    const m = requirementsModel(
      input({ requires: [myc("codex")], report: report([{ agent: "codex", plugins: [mycelium(true, "mycelium")] }]) }),
    );
    expect(m.rows[0].status).toBe("installed");
  });

  it("a missing requirement offers the agent's own install", () => {
    const m = requirementsModel(input({ requires: [myc("claude")], report: report([{ agent: "claude" }]) }));
    expect(m.rows[0]).toMatchObject({
      status: "missing",
      state: "not installed",
      tone: "warn",
      action: "install",
      offerInstall: true,
    });
  });

  it("a disabled requirement says so", () => {
    const m = requirementsModel(
      input({ requires: [myc("claude")], report: report([{ agent: "claude", plugins: [mycelium(false)] }]) }),
    );
    expect(m.rows[0]).toMatchObject({
      status: "disabled",
      state: "installed, disabled",
      tone: "warn",
      action: null,
      offerInstall: false,
    });
  });

  it("lists only the agents installed on this host", () => {
    const m = requirementsModel(
      input({
        requires: [myc("claude"), myc("codex")],
        report: report([{ agent: "claude", plugins: [mycelium()] }, { agent: "codex", available: false }]),
      }),
    );
    expect(m.notice).toBeNull();
    expect(m.rows.map((r) => r.agent)).toEqual(["claude"]);
  });

  it("says once when neither required agent is installed here", () => {
    const m = requirementsModel(
      input({
        requires: [myc("claude"), myc("codex")],
        report: report([{ agent: "claude", available: false }]),
      }),
    );
    expect(m.rows).toEqual([]);
    expect(m.notice).toBe("It needs claude or codex, and neither is installed on this host.");
  });

  it("names the one agent a lone requirement lists", () => {
    const m = requirementsModel(input({ requires: [myc("codex")], report: report([{ agent: "claude" }]) }));
    expect(m.notice).toBe("It needs codex, which isn't installed on this host.");
  });

  it("names three agents with none of them installed", () => {
    const m = requirementsModel(
      input({
        requires: [myc("claude"), { agent: "codex", id: "notes", marketplace: "x" }, myc("gemini")],
        report: report([]),
      }),
    );
    expect(m.notice).toBe("It needs claude, codex or gemini, and none of them is installed on this host.");
  });

  it("while loading, one line for the whole block", () => {
    for (const state of ["loading", "idle"] as const) {
      const m = requirementsModel(input({ requires: [myc("claude"), myc("codex")], recommends: [myc("claude")], state }));
      expect(m).toEqual({ phase: "checking", notice: CHECKING, rows: [] });
    }
  });

  it("a refetch keeps showing the last report", () => {
    const m = requirementsModel(
      input({ requires: [myc("claude")], state: "loading", report: report([{ agent: "claude", plugins: [mycelium()] }]) }),
    );
    expect(m.phase).toBe("ready");
    expect(m.rows[0].status).toBe("installed");
  });

  it("when the daemon can't ask the agents, requirements say so honestly", () => {
    const m = requirementsModel(
      input({ requires: [myc("claude"), myc("codex")], recommends: [myc("claude")], state: "unavailable" }),
    );
    expect(m.phase).toBe("unavailable");
    expect(m.notice).toBeNull();
    expect(m.rows.map((r) => [r.kind, r.agent, r.status, r.state, r.action])).toEqual([
      ["requires", "claude", "unknown", "can't check on this daemon", null],
      ["requires", "codex", "unknown", "can't check on this daemon", null],
    ]);
  });

  it("a failed first report keeps the rows, without actions (the caller shows the error once)", () => {
    const m = requirementsModel(input({ requires: [myc("claude")], state: "error" }));
    expect(m.phase).toBe("error");
    expect(m.rows).toHaveLength(1);
    expect(m.rows[0]).toMatchObject({ status: "unknown", state: "couldn't ask", action: null, offerInstall: false });
  });
});

describe("requirementsModel · recommends", () => {
  it("an installed recommendation: installed and its version", () => {
    const m = requirementsModel(
      input({ recommends: [myc("claude")], report: report([{ agent: "claude", plugins: [mycelium()] }]) }),
    );
    expect(m.notice).toBeNull();
    expect(m.rows[0]).toMatchObject({
      kind: "recommends",
      status: "installed",
      state: "installed 0.7.2",
      tone: "good",
      action: null,
      offerInstall: false,
    });
  });

  it("a disabled one says so", () => {
    const m = requirementsModel(
      input({ recommends: [myc("claude")], report: report([{ agent: "claude", plugins: [mycelium(false)] }]) }),
    );
    expect(m.rows[0].state).toBe("installed, disabled");
  });

  it("a missing recommendation offers the install, calmly", () => {
    const m = requirementsModel(
      input({ recommends: [myc("claude")], knowledge: "mycelium", report: report([{ agent: "claude" }]) }),
    );
    expect(m.rows[0]).toMatchObject({
      status: "missing",
      state: "not installed",
      tone: "neutral",
      action: "install",
      offerInstall: true,
    });
  });

  it("untrusted codex hooks are the row's one action", () => {
    const m = requirementsModel(
      input({
        recommends: [myc("codex")],
        report: report([
          {
            agent: "codex",
            plugins: [mycelium(true, "mycelium")],
            hooks: [
              { key: "a", event: "Stop", plugin_id: "mycelium", trust: "untrusted", hash: "h" },
              { key: "b", event: "SessionStart", plugin_id: "mycelium", trust: "modified", hash: "h" },
            ],
          },
        ]),
      }),
    );
    expect(m.rows[0]).toMatchObject({ state: "2 hooks not trusted", tone: "warn", action: "review" });
  });

  it("says nothing at all for an agent that isn't installed here", () => {
    const m = requirementsModel(
      input({
        recommends: [myc("claude"), myc("codex")],
        knowledge: "mycelium",
        report: report([{ agent: "claude", plugins: [mycelium()] }, { agent: "codex", available: false }]),
      }),
    );
    expect(m.notice).toBeNull();
    expect(m.rows.map((r) => r.agent)).toEqual(["claude"]);

    const none = requirementsModel(input({ recommends: [myc("claude"), myc("codex")], report: report([]) }));
    expect(none).toEqual({ phase: "ready", notice: null, rows: [] });
  });

  it("the daemon can't ask: a recommendation says nothing", () => {
    const m = requirementsModel(input({ recommends: [myc("claude")], state: "unavailable" }));
    expect(m.rows).toEqual([]);
    expect(m.notice).toBeNull();
  });

  it("requirements come first, recommendations after", () => {
    const m = requirementsModel(
      input({
        requires: [{ agent: "codex", id: "notes", marketplace: "x" }],
        recommends: [myc("claude")],
        report: report([{ agent: "claude" }, { agent: "codex" }]),
      }),
    );
    expect(m.rows.map((r) => `${r.kind}:${r.agent}`)).toEqual(["requires:codex", "recommends:claude"]);
  });
});

describe("hooks and the sheet's wording", () => {
  const hook = (key: string, plugin_id: string, trust: AgentHook["trust"]): AgentHook => ({
    key,
    event: "Stop",
    plugin_id,
    trust,
    hash: `h-${key}`,
  });

  it("collects codex hooks waiting for trust from both kinds, once each", () => {
    const m = requirementsModel(
      input({
        requires: [myc("codex")],
        recommends: [myc("codex")],
        report: report([
          {
            agent: "codex",
            plugins: [mycelium(true, "mycelium")],
            hooks: [hook("a", "mycelium", "untrusted"), hook("b", "mycelium", "trusted"), hook("c", "other", "untrusted")],
          },
        ]),
      }),
    );
    expect(m.rows.map((r) => r.untrustedHooks.map((h) => h.key))).toEqual([["a"], ["a"]]);
    expect(hooksAwaitingTrust(m).map((h) => h.key)).toEqual(["a"]);
  });

  it("words a requirement as needed and a recommendation as optional", () => {
    const rep = report([{ agent: "claude" }, { agent: "codex", plugins: [mycelium()] }]);
    const req = requirementsModel(input({ requires: [myc("claude")], report: rep })).rows[0];
    expect(sheetText(req, "mycelium")).toBe("claude needs the mycelium@mycelium plugin");
    const rec = requirementsModel(input({ recommends: [myc("claude"), myc("codex")], knowledge: "mycelium", report: rep }));
    expect(rec.rows.map((r) => sheetText(r, "mycelium"))).toEqual([
      "optional: the mycelium@mycelium plugin lets claude record knowledge as it works",
      "codex has mycelium 0.7.2 ✓",
    ]);
    expect(sheetText(rec.rows[0], null)).toBe("optional: the mycelium@mycelium plugin for claude");
    const unk = requirementsModel(input({ requires: [myc("claude")], state: "unavailable" })).rows[0];
    expect(sheetText(unk, null)).toBe("claude needs the mycelium@mycelium plugin — can't check on this daemon");
  });
});

describe("agentSideBlocks", () => {
  const mk = (agent: string): PluginRequirement => ({
    agent,
    id: "mycelium@mycelium",
    marketplace: "arjunrajlaboratory/mycelium",
  });
  const p = (over: Partial<BlockInput> = {}): BlockInput => ({
    name: "Mycelium",
    requires: [],
    recommends: [mk("claude"), mk("codex")],
    requires_summary: null,
    recommends_summary: "Mycelium's own agent plugin gives claude and codex the skills that record findings.",
    knowledge: "mycelium",
    ...over,
  });

  it("a box with the author's sentence, the plugin's page and one row per agent here", () => {
    const input_ = p();
    const m = requirementsModel(
      input({
        recommends: input_.recommends,
        report: report([{ agent: "claude", plugins: [mycelium()] }, { agent: "codex" }]),
      }),
    );
    const blocks = agentSideBlocks(m, input_);
    expect(blocks).toHaveLength(1);
    expect(blocks[0]).toMatchObject({
      kind: "recommends",
      title: "Agent-side plugin",
      summary: "Mycelium's own agent plugin gives claude and codex the skills that record findings.",
      link: { label: "mycelium on GitHub", url: "https://github.com/arjunrajlaboratory/mycelium" },
      notice: null,
    });
    expect(blocks[0].rows.map((r) => [r.agent, r.state, r.action])).toEqual([
      ["claude", "installed 0.7.2", null],
      ["codex", "not installed", "install"],
    ]);
  });

  it("no box when no agent that could use it is installed here", () => {
    const m = requirementsModel(input({ recommends: p().recommends, report: report([]) }));
    expect(agentSideBlocks(m, p())).toEqual([]);
  });

  it("while asking, one line naming the agents", () => {
    const m = requirementsModel(input({ recommends: p().recommends, state: "loading" }));
    const blocks = agentSideBlocks(m, p());
    expect(blocks.map((b) => [b.notice, b.rows.length])).toEqual([["asking claude and codex…", 0]]);
  });

  it("a requirement no agent here can meet keeps its box, with the notice", () => {
    const input_ = p({ requires: [myc("codex")], recommends: [], requires_summary: null });
    const m = requirementsModel(input({ requires: input_.requires, report: report([{ agent: "claude" }]) }));
    const blocks = agentSideBlocks(m, input_);
    expect(blocks).toHaveLength(1);
    expect(blocks[0]).toMatchObject({
      title: "Agent-side plugin · needed",
      notice: "It needs codex, which isn't installed on this host.",
      rows: [],
    });
    expect(blocks[0].summary).toContain("Mycelium works only when");
  });

  it("the plain sentence when the author wrote none", () => {
    const m = requirementsModel(input({ recommends: [myc("claude")], report: report([{ agent: "claude" }]) }));
    const blocks = agentSideBlocks(m, p({ recommends: [myc("claude")], recommends_summary: null }));
    expect(blocks[0].summary).toBe(
      "With their own plugin, your agents can record what they learn as they work; Mycelium works without it.",
    );
  });

  it("links a marketplace only when it names a page", () => {
    expect(marketplaceUrl("arjunrajlaboratory/mycelium")).toBe("https://github.com/arjunrajlaboratory/mycelium");
    expect(marketplaceUrl("https://example.org/market")).toBe("https://example.org/market");
    expect(marketplaceUrl("mycelium")).toBeNull();
    expect(marketplaceUrl("../x")).toBeNull();
    expect(marketplaceUrl("javascript:alert(1)")).toBeNull();
  });
});
