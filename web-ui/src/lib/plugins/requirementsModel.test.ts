import { describe, expect, it } from "vitest";
import type { AgentHook, AgentPlugin, AgentPlugins, PluginRequirement } from "./store";
import { CHECKING, hooksAwaitingTrust, requirementsModel, sheetText, type RequirementsInput } from "./requirementsModel";

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

  it("an installed requirement: the sentence plus the ✓ pill with its version", () => {
    const m = requirementsModel(
      input({ requires: [myc("claude")], report: report([{ agent: "claude", plugins: [mycelium()] }]) }),
    );
    expect(m.phase).toBe("ready");
    expect(m.notice).toBeNull();
    expect(m.rows).toHaveLength(1);
    expect(m.rows[0]).toMatchObject({
      kind: "requires",
      status: "installed",
      text: "Requires the claude plugin mycelium@mycelium",
      pill: { text: "claude · plugin 0.7.2 ✓", tone: "good" },
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
      pill: { text: "claude · plugin not installed", tone: "warn" },
      offerInstall: true,
    });
  });

  it("a disabled requirement says so", () => {
    const m = requirementsModel(
      input({ requires: [myc("claude")], report: report([{ agent: "claude", plugins: [mycelium(false)] }]) }),
    );
    expect(m.rows[0]).toMatchObject({
      status: "disabled",
      pill: { text: "claude · plugin 0.7.2 · disabled", tone: "warn" },
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
    expect(m.notice).toBe("Requires claude or codex with plugin mycelium@mycelium; neither is installed on this host");
  });

  it("names the one agent a lone requirement lists", () => {
    const m = requirementsModel(input({ requires: [myc("codex")], report: report([{ agent: "claude" }]) }));
    expect(m.notice).toBe("Requires codex with plugin mycelium@mycelium; codex isn't installed on this host");
  });

  it("names each agent's plugin when the ids differ", () => {
    const m = requirementsModel(
      input({
        requires: [myc("claude"), { agent: "codex", id: "notes", marketplace: "x" }],
        report: report([]),
      }),
    );
    expect(m.notice).toBe(
      "Requires claude with plugin mycelium@mycelium or codex with plugin notes; neither is installed on this host",
    );
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

  it("when the daemon can't ask the agents, requirements keep the honest pill", () => {
    const m = requirementsModel(
      input({ requires: [myc("claude"), myc("codex")], recommends: [myc("claude")], state: "unavailable" }),
    );
    expect(m.phase).toBe("unavailable");
    expect(m.notice).toBeNull();
    expect(m.rows.map((r) => [r.kind, r.status, r.text, r.pill?.text])).toEqual([
      ["requires", "unknown", "Requires the claude plugin mycelium@mycelium", "claude · can't check on this daemon"],
      ["requires", "unknown", "Requires the codex plugin mycelium@mycelium", "codex · can't check on this daemon"],
    ]);
  });

  it("a failed first report keeps the sentences, without pills (the caller shows the error once)", () => {
    const m = requirementsModel(input({ requires: [myc("claude")], state: "error" }));
    expect(m.phase).toBe("error");
    expect(m.rows).toHaveLength(1);
    expect(m.rows[0]).toMatchObject({ status: "unknown", pill: null, offerInstall: false });
  });
});

describe("requirementsModel · recommends", () => {
  it("an installed recommendation: the name before @, ✓, its version", () => {
    const m = requirementsModel(
      input({ recommends: [myc("claude")], report: report([{ agent: "claude", plugins: [mycelium()] }]) }),
    );
    expect(m.notice).toBeNull();
    expect(m.rows[0]).toMatchObject({
      kind: "recommends",
      status: "installed",
      text: "For claude: mycelium ✓ 0.7.2",
      pill: null,
      offerInstall: false,
    });
  });

  it("a disabled one says so", () => {
    const m = requirementsModel(
      input({ recommends: [myc("claude")], report: report([{ agent: "claude", plugins: [mycelium(false)] }]) }),
    );
    expect(m.rows[0].text).toBe("For claude: mycelium 0.7.2 is installed but disabled");
  });

  it("offers the install, saying why when the plugin provides knowledge", () => {
    const m = requirementsModel(
      input({ recommends: [myc("claude")], knowledge: "mycelium", report: report([{ agent: "claude" }]) }),
    );
    expect(m.rows[0]).toMatchObject({
      status: "missing",
      text: "For claude: install the mycelium@mycelium plugin so it can record knowledge",
      offerInstall: true,
    });
  });

  it("offers the install without the knowledge clause otherwise", () => {
    const m = requirementsModel(input({ recommends: [myc("codex")], report: report([{ agent: "codex" }]) }));
    expect(m.rows[0].text).toBe("For codex: install the mycelium@mycelium plugin");
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
