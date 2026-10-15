import { describe, expect, it } from "vitest";
import { groupAgents, normalizeAgentCatalog, primaryAgentId, primaryStart, quickAction } from "./launcher";
import { agentProductName } from "../shared/agentNames";

const ready = { installed: true, chat_capable: true };
const catalog = normalizeAgentCatalog([
  { id: "claude", name: "Claude Code", ...ready },
  { id: "codex", name: "Codex", ...ready },
  { id: "agy", name: "Antigravity", installed: false, managed_install: true },
  { id: "grok", name: "Grok Build", ...ready },
  { id: "cursor", name: "Cursor CLI", installed: false, managed_install: true },
]);

describe("the rail's one start", () => {
  it("is the saved default, whichever agent that is", () => {
    expect(primaryAgentId("claude", catalog)).toBe("claude");
    expect(primaryAgentId("grok", catalog)).toBe("grok");
    expect(primaryAgentId("cursor", catalog)).toBe("cursor");
  });
  it("lets a retired default yield to the first installed agent once the catalog is known", () => {
    expect(primaryAgentId("gemini", catalog)).toBe("claude");
    const codexOnly = catalog.map((a) => ({ ...a, installed: a.id === "codex" }));
    expect(primaryAgentId("gemini", codexOnly)).toBe("codex");
    const none = catalog.map((a) => ({ ...a, installed: false }));
    expect(primaryAgentId("gemini", none)).toBe("claude");
    expect(primaryAgentId("gemini", null)).toBe("gemini");
    expect(primaryAgentId("gemini", [])).toBe("gemini");
  });
  it("names the row from the catalog, else the product name", () => {
    expect(primaryStart(null, "cursor", "chat", agentProductName).name).toBe("Cursor CLI");
    const row = primaryStart(catalog, "codex", "chat");
    expect([row.id, row.name, row.action]).toEqual(["codex", "Codex", "start"]);
    expect(primaryStart(catalog, "agy", "chat").action).toBe("install");
  });
});

describe("quick action", () => {
  const [claude] = catalog;
  it("starts a ready agent, and anything before the catalog loads", () => {
    expect(quickAction(claude, "chat")).toBe("start");
    expect(quickAction(null, "chat")).toBe("start");
  });
  it("never opens a doomed pane for a missing agent", () => {
    expect(quickAction({ ...claude, installed: false, managedInstall: true }, "chat")).toBe("install");
    expect(quickAction({ ...claude, installed: false, managedInstall: false }, "terminal")).toBe("setup");
  });
  it("asks for chat setup only when the start would be a chat", () => {
    const agy = { ...claude, chatSetupRequired: true, managedInstall: true };
    expect(quickAction(agy, "chat")).toBe("setup-chat");
    expect(quickAction(agy, "terminal")).toBe("start");
    expect(quickAction({ ...agy, managedInstall: false }, "chat")).toBe("setup");
  });
  it("still starts an outdated build (the spawn answers honestly)", () => {
    expect(quickAction({ ...claude, outdated: true }, "chat")).toBe("start");
  });
});

describe("launcher groups", () => {
  it("puts the installed agents first, the default leading, the rest folded", () => {
    const { installed, others } = groupAgents(catalog, "claude");
    expect(installed.map((a) => a.id)).toEqual(["claude", "codex", "grok"]);
    expect(others.map((a) => a.id)).toEqual(["agy", "cursor"]);
  });
  it("leads with whichever installed agent is the default", () => {
    const { installed } = groupAgents(catalog, "grok");
    expect(installed.map((a) => a.id)).toEqual(["grok", "claude", "codex"]);
  });
  it("shows every agent up front when none is installed", () => {
    const none = catalog.map((a) => ({ ...a, installed: false }));
    const { installed, others } = groupAgents(none, "codex");
    expect(installed.map((a) => a.id)).toEqual(["codex", "claude", "agy", "grok", "cursor"]);
    expect(others).toEqual([]);
  });
});
