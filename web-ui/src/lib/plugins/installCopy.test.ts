import { describe, expect, it } from "vitest";
import type { WorkspacePlugin } from "./store";
import { installedOutcome, installTitle, pinnedVersion, updatedOutcome } from "./installCopy";

const SHA = "0123456789abcdef".repeat(4);

function available(over: Partial<WorkspacePlugin> = {}): WorkspacePlugin {
  return {
    id: "agent-notes",
    name: "Agent notes",
    summary: "Notes your agents keep for you.",
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
