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
  it("the Install tooltip names the pinned release on GitHub", () => {
    expect(pinnedVersion(available())).toBe("0.1.0");
    expect(installTitle(available())).toBe(
      "downloads plugin.wasm and plugin.toml from github.com/martinappberg/chimaera-agent-notes/releases/v0.1.0 into ~/.chimaera/plugins on this host, verifies both checksums, runs sandboxed inside chimaera, and does nothing until switched on",
    );
    expect(installTitle(available({ repo: null }))).toContain("from its v0.1.0 release into");
  });

  it("the install outcome shortens the verified sha256 and keeps it whole in the title", () => {
    const o = installedOutcome(
      { id: "agent-notes", version: "0.1.0", sha256: { "plugin.wasm": SHA }, plugin: { name: "Agent notes" } },
      "agent-notes",
    );
    expect(o.text).toBe("installed Agent notes 0.1.0 · plugin.wasm sha256 0123456789abcdef… verified");
    expect(o.title).toBe(`plugin.wasm sha256 ${SHA}`);
  });

  it("falls back to the given name, and says nothing about a checksum it wasn't given", () => {
    expect(installedOutcome({ id: "x", version: "1.0.0", plugin: null }, "x")).toEqual({
      text: "installed x 1.0.0",
      title: undefined,
    });
    expect(updatedOutcome({ id: "x", version: "1.1.0", sha256: { "plugin.wasm": SHA } }).text).toBe(
      "updated to 1.1.0 · plugin.wasm sha256 0123456789abcdef… verified",
    );
  });
});
