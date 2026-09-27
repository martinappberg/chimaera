import { describe, expect, it } from "vitest";
import type { WorkspacePlugin } from "./store";
import {
  canReinstall,
  checkedWords,
  footprint,
  hereLine,
  installedOutcome,
  installTitle,
  pinnedVersion,
  stateWords,
  tileLetters,
  updatedOutcome,
} from "./installCopy";

const SHA = "0123456789abcdef".repeat(4);

function available(over: Partial<WorkspacePlugin> = {}): WorkspacePlugin {
  return {
    id: "agent-notes",
    name: "Agent notes",
    summary: "Notes your agents keep for you.",
    description: null,
    homepage: null,
    adds: { ui: [], agents: [] },
    provides: { knowledge: null, mcp_tools: [], views: [] },
    setup: null,
    detect: [],
    on: false,
    detected: false,
    active: false,
    requires: [],
    recommends: [],
    requires_summary: null,
    recommends_summary: null,
    version: "0.1.0",
    api: "",
    source: "available",
    installed: false,
    first_party: true,
    verified: false,
    sha256_wasm: null,
    repo: "martinappberg/chimaera-agent-notes",
    pinned_version: "0.1.0",
    local_path: null,
    path: null,
    previous: null,
    update: null,
    fault: null,
    ...over,
  };
}

describe("installCopy", () => {
  it("the Install tooltip names the repository and says it does nothing until switched on", () => {
    expect(pinnedVersion(available())).toBe("0.1.0");
    expect(installTitle(available())).toBe(
      "Downloads it from github.com/martinappberg/chimaera-agent-notes into this host's ~/.chimaera/plugins. It does nothing until you switch it on in a workspace.",
    );
    expect(installTitle(available({ repo: null }))).toContain("from its release into");
  });

  it("the install outcome names the plugin and its version, never a checksum", () => {
    const o = installedOutcome(
      { id: "agent-notes", version: "0.1.0", sha256: { "plugin.wasm": SHA }, plugin: { name: "Agent notes" } },
      "agent-notes",
    );
    expect(o).toEqual({ text: "installed Agent notes 0.1.0" });
  });

  it("falls back to the given name, and an update says only the version", () => {
    expect(installedOutcome({ id: "x", version: "1.0.0", plugin: null }, "x")).toEqual({ text: "installed x 1.0.0" });
    expect(updatedOutcome({ id: "x", version: "1.1.0", sha256: { "plugin.wasm": SHA } })).toEqual({
      text: "updated to 1.1.0",
    });
  });
});

describe("the card's words", () => {
  const myc = (over: Partial<WorkspacePlugin> = {}) =>
    available({
      id: "mycelium",
      name: "Mycelium",
      source: "installed",
      installed: true,
      verified: true,
      detect: [".living/INDEX.md", "MYCELIUM.md"],
      setup: { prompt: "Set up Mycelium in this repository." },
      ...over,
    });

  it("the switch's state, in words", () => {
    expect(stateWords(myc({ on: true, detected: true, active: true }))).toBe("active here");
    expect(stateWords(myc({ on: true }))).toBe("on · not set up here yet");
    expect(stateWords(myc())).toBe("off");
  });

  it("the Here line says only what means something", () => {
    expect(footprint(myc())).toBe(".living/");
    expect(footprint(myc({ detect: ["MYCELIUM.md"] }))).toBe("MYCELIUM.md");
    expect(hereLine(myc({ on: true, detected: true, active: true }), { findings: 1, decisions: 0 })).toEqual({
      text: "using .living/ in this workspace · 1 finding · 0 decisions",
      kind: "using",
    });
    expect(hereLine(myc({ detected: true }), null)?.text).toBe(
      "found .living/ in this workspace — switch it on to use it",
    );
    expect(hereLine(myc({ on: true }), null)).toEqual({ text: "not set up in this workspace yet", kind: "setup" });
    expect(hereLine(myc(), null)).toBeNull();
    expect(hereLine(available({ on: true, active: true }), null)).toBeNull();
  });

  it("the checked line ages in words", () => {
    const now = 10_000_000;
    expect(checkedWords(now - 5_000, now)).toBe("checked just now");
    expect(checkedWords(now - 60_000, now)).toBe("checked 1 minute ago");
    expect(checkedWords(now - 2 * 3_600_000, now)).toBe("checked 2 hours ago");
    expect(checkedWords(now - 3 * 86_400_000, now)).toBe("checked 3 days ago");
  });

  it("Reinstall is for a copy whose files stopped matching its release", () => {
    const broken = myc({ fault: "the downloaded files don't match what the release published — reinstall it", verified: false, repo: "a/b" });
    expect(canReinstall(broken)).toBe(true);
    expect(canReinstall({ ...broken, local_path: "/src/x" })).toBe(false);
    expect(canReinstall({ ...broken, repo: null })).toBe(false);
    expect(canReinstall(myc({ fault: "needs a newer chimaera", verified: true }))).toBe(false);
    expect(canReinstall(available())).toBe(false);
  });

  it("the tile's letters", () => {
    expect(tileLetters("mycelium")).toBe("my");
    expect(tileLetters("test-fixture")).toBe("te");
    expect(tileLetters("x-ray")).toBe("xr");
  });
});
