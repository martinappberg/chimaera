import { describe, expect, it } from "vitest";
import { groupAgents, normalizeAgentCatalog, quickAction, quickAgentIds, quickStarts } from "./launcher";
import { agentProductName } from "../shared/agentNames";

const ready = { installed: true, chat_capable: true };
const catalog = normalizeAgentCatalog([
  { id: "claude", name: "Claude Code", ...ready },
  { id: "codex", name: "Codex", ...ready },
  { id: "agy", name: "Antigravity", installed: false, managed_install: true },
  { id: "grok", name: "Grok Build", ...ready },
  { id: "cursor", name: "Cursor CLI", installed: false, managed_install: true },
]);

describe("quick starts", () => {
  it("offers Claude Code and Codex, the default first", () => {
    expect(quickAgentIds("claude", catalog)).toEqual(["claude", "codex"]);
    expect(quickAgentIds("codex", catalog)).toEqual(["codex", "claude"]);
  });
  it("keeps another default as the primary one-click start", () => {
    expect(quickAgentIds("grok", catalog)).toEqual(["grok", "claude", "codex"]);
    expect(quickAgentIds("cursor", catalog)).toEqual(["cursor", "claude", "codex"]);
  });
  it("lets a retired default yield to Claude Code once the catalog is known", () => {
    expect(quickAgentIds("gemini", catalog)).toEqual(["claude", "codex"]);
    expect(quickAgentIds("gemini", null)).toEqual(["gemini", "claude", "codex"]);
    expect(quickAgentIds("gemini", [])).toEqual(["gemini", "claude", "codex"]);
  });
  it("drops a quick agent this host's catalog doesn't list", () => {
    const noCodex = catalog.filter((a) => a.id !== "codex");
    expect(quickAgentIds("claude", noCodex)).toEqual(["claude"]);
  });
  it("names rows from the catalog, else the product name", () => {
    expect(quickStarts(null, "cursor", "chat", agentProductName).map((q) => q.name)).toEqual(["Cursor CLI", "Claude Code", "Codex"]);
    const rows = quickStarts(catalog, "codex", "chat");
    expect(rows.map((q) => [q.id, q.primary])).toEqual([["codex", true], ["claude", false]]);
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
    const { quick, others } = groupAgents(catalog, "claude");
    expect(quick.map((a) => a.id)).toEqual(["claude", "codex", "grok"]);
    expect(others.map((a) => a.id)).toEqual(["agy", "cursor"]);
  });
  it("leads with whichever installed agent is the default", () => {
    const { quick } = groupAgents(catalog, "grok");
    expect(quick.map((a) => a.id)).toEqual(["grok", "claude", "codex"]);
  });
  it("shows every agent up front when none is installed", () => {
    const none = catalog.map((a) => ({ ...a, installed: false }));
    const { quick, others } = groupAgents(none, "codex");
    expect(quick.map((a) => a.id)).toEqual(["codex", "claude", "agy", "grok", "cursor"]);
    expect(others).toEqual([]);
  });
});
