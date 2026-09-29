import { describe, expect, it } from "vitest";
import type { WorkspacePlugin } from "./store";
import { EMPTY_PLATFORM } from "./platform";
import {
  activityWords,
  approxSize,
  canReinstall,
  checkedWords,
  footprint,
  footprints,
  hereLine,
  installedOutcome,
  installLine,
  installTitle,
  pinnedVersion,
  stateWords,
  tileLetters,
  holdWords,
  sourceWords,
  trustAskFor,
  trustWords,
  updatedOutcome,
} from "./installCopy";

const SHA = "0123456789abcdef".repeat(4);

function available(over: Partial<WorkspacePlugin> = {}): WorkspacePlugin {
  return {
    platform: EMPTY_PLATFORM,
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
    tier: "sandboxed",
    caps: "c".repeat(64),
    can: [],
    standing: "verified",
    hold: null,
    skipped_version: null,
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
    expect(footprints(myc({ detect: [".living/INDEX.md", ".living/findings", "MYCELIUM.md"] }))).toEqual([
      ".living/",
      "MYCELIUM.md",
    ]);
    expect(footprints(myc({ detect: [] }))).toEqual([]);
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
    expect(tileLetters("Mycelium")).toBe("my");
    expect(tileLetters("Agent notes")).toBe("an");
    expect(tileLetters("Test fixture")).toBe("tf");
    expect(tileLetters("LaTeX")).toBe("la");
    expect(tileLetters("x-ray")).toBe("xr");
    expect(tileLetters("Écrit")).toBe("éc");
    expect(tileLetters("")).toBe("");
  });
});

describe("the line under a card opened before install", () => {
  // No-break spaces: "≈ 305 KB" never wraps apart.
  const nb = (s: string) => s.replace(/ /g, "\u00a0");

  it("rounds a download to what a person reads", () => {
    expect(approxSize(134_947)).toBe(nb("≈ 135 KB"));
    expect(approxSize(304_940)).toBe(nb("≈ 305 KB"));
    expect(approxSize(120)).toBe(nb("≈ 1 KB"));
    expect(approxSize(1_234_567)).toBe(nb("≈ 1.2 MB"));
    expect(approxSize(2_000_000)).toBe(nb("≈ 2 MB"));
    expect(approxSize(16_700_000)).toBe(nb("≈ 17 MB"));
  });

  it("names the repository and the size, and says it stays off", () => {
    expect(installLine("martinappberg/chimaera-plugin-mycelium", 304_940)).toBe(
      `Installing downloads it from github.com/martinappberg/chimaera-plugin-mycelium (${nb("≈ 305 KB")}) into this host's ~/.chimaera/plugins. It does nothing until you switch it on in a workspace.`,
    );
    expect(installLine("acme/demo", null)).toBe(
      "Installing downloads it from github.com/acme/demo into this host's ~/.chimaera/plugins. It does nothing until you switch it on in a workspace.",
    );
  });
});

describe("trust words", () => {
  const installed = (over: Partial<WorkspacePlugin> = {}) =>
    available({ source: "installed", installed: true, first_party: false, repo: "acme/x", name: "X", id: "x", version: "0.2.0", ...over });

  it("says where a copy came from, a local build by its directory", () => {
    expect(sourceWords(installed())).toBe("github.com/acme/x");
    expect(sourceWords(installed({ local_path: "/home/me/x" }))).toBe("a local build in /home/me/x");
  });

  it("asks for an installed build from its own card, a privileged one by name", () => {
    const can = [{ text: "Reads files in this workspace", privileged: false }];
    const ask = trustAskFor(installed({ can, caps: "d".repeat(64) }));
    expect(ask).toMatchObject({ id: "x", version: "0.2.0", caps: "d".repeat(64), can, grown: null, confirm: null });
    expect(trustAskFor(installed({ tier: "privileged" })).confirm).toBe("X");
  });

  it("the hold callout says why and what the user can do", () => {
    expect(holdWords(installed())).toBeNull();
    expect(holdWords(installed({ hold: { kind: "untrusted" } }))).toEqual({
      text: "X waits for your trust: nothing it adds works until you trust what it can do.",
      action: "trust",
    });
    expect(holdWords(installed({ hold: { kind: "blocked", level: "soft", reason: "it crashes" } }))?.action).toBe("allow");
    const hard = holdWords(installed({ hold: { kind: "blocked", level: "hard", reason: "it steals keys" } }));
    expect(hard).toEqual({ text: "Chimaera blocked X 0.2.0: it steals keys. Update or remove it.", action: null });
    expect(holdWords(installed({ hold: { kind: "policy", reason: "this host only allows verified plugins" } }))?.text).toBe(
      "Off on this host: this host only allows verified plugins.",
    );
  });

  it("an update that asks for more is an Allow, anything else a Trust", () => {
    const ask = trustAskFor(installed());
    expect(trustWords(ask, "install")).toMatchObject({ title: "Trust X?", confirm: "Trust and install" });
    expect(trustWords(ask, "install").lead).toContain("from github.com/acme/x");
    const grown = { ...ask, grown: [{ text: "Gives agents 1 tool: version", privileged: false }], from_version: "0.1.0" };
    const w = trustWords(grown, "update");
    expect(w.title).toBe("Allow X to do more?");
    expect(w.confirm).toBe("Allow update");
    expect(w.lead).toContain("0.1.0 keeps running until you decide");
  });

  it("the activity log reads as sentences", () => {
    expect(activityWords({ kind: "install", ts: 1, version: "0.1.0", source: "acme/x" })).toBe("Installed 0.1.0 from acme/x");
    expect(activityWords({ kind: "update", ts: 1, version: "0.2.0", from: "0.1.0" })).toBe("Updated to 0.2.0 (was 0.1.0)");
    expect(activityWords({ kind: "trust", ts: 1, version: "0.2.0", how: "prompt" })).toBe("You trusted what 0.2.0 can do");
    expect(activityWords({ kind: "trust", ts: 1, version: "0.2.1", how: "subset" })).toBe("Trusted 0.2.1: it asked for nothing new");
    expect(activityWords({ kind: "untrust", ts: 1 })).toBe("You withdrew your trust");
    expect(activityWords({ kind: "job", ts: 1, program: "latexmk", args: ["-pdf", "main.tex"], exit: 0, duration_ms: 2400 })).toBe(
      "Ran latexmk -pdf main.tex (exit 0, 2.4 s)",
    );
    expect(activityWords({ kind: "job", ts: 1, program: "sleep", args: ["30"], exit: null, timed_out: true })).toBe(
      "Ran sleep 30 (ran out of time)",
    );
    expect(activityWords({ kind: "tool-install", ts: 1, tool: "tinytex", version: "2026.09", url: "https://github.com/x/y.tar.xz" })).toBe(
      "Downloaded tinytex 2026.09 from https://github.com/x/y.tar.xz",
    );
    expect(activityWords({ kind: "tool-remove", ts: 1, tool: "tinytex" })).toBe("Removed its tinytex");
    expect(activityWords({ kind: "future-kind", ts: 1 })).toBe("future-kind");
  });
});
