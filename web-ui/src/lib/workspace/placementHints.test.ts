import { describe, expect, it } from "vitest";

import { parseOwnershipHints, readsOwnership } from "./placementHints";

const row = (workspace_id: string, ownership: unknown) => ({ workspace_id, name: workspace_id, ownership });

describe("a Home row's place hint", () => {
  it("names work elsewhere without guessing the owner type", () => {
    const { configured, hints } = parseOwnershipHints({
      configured: true,
      workspaces: [
        row("w-cloud", { state: "remote", epoch: 3, holder: "worker-1" }),
        row("w-home", { state: "hydrating", epoch: 4 }),
        row("w-leaving", { state: "transferring", epoch: 2 }),
      ],
    });
    expect(configured).toBe(true);
    expect(hints.get("w-cloud")).toBe("Running elsewhere");
    expect(hints.get("w-home")).toBe("Coming back here…");
    expect(hints.get("w-leaving")).toBe("Moving…");
  });

  it("says nothing for a project that is here or not owned elsewhere", () => {
    const { hints } = parseOwnershipHints({
      configured: true,
      workspaces: [
        row("w-here", { state: "local", epoch: 1 }),
        row("w-verify", { state: "awaiting_verification", epoch: 1 }),
        row("w-setup", { state: "setting_up", epoch: 1 }),
        row("w-private", { state: "privacy_disabled", epoch: 1 }),
        row("w-none", null),
      ],
    });
    expect(hints.size).toBe(0);
  });

  it("says nothing at all unless Pro is configured on this daemon", () => {
    const workspaces = [row("w-cloud", { state: "remote", epoch: 3, holder: "worker-1" })];
    expect(parseOwnershipHints({ configured: false, workspaces })).toEqual({ configured: false, hints: new Map() });
    expect(parseOwnershipHints({ workspaces }).hints.size).toBe(0);
  });

  it("reads anything malformed as nothing to say", () => {
    for (const body of [null, "x", 3, [], { configured: true }, { configured: true, workspaces: [null, 1, {}, { workspace_id: 7, ownership: { state: "remote" } }] }]) {
      expect(parseOwnershipHints(body).hints.size).toBe(0);
    }
  });
});

describe("when Home asks the daemon about Pro", () => {
  const own = { remoteHome: false, gateway: false, visible: true };
  it("never without an active plan: a free or offered window requests no /pro/status", () => {
    expect(readsOwnership({ ...own, tier: "free" })).toBe(false);
    expect(readsOwnership({ ...own, tier: "offered" })).toBe(false);
    expect(readsOwnership({ ...own, tier: "active" })).toBe(true);
  });
  it("never on a remote host's Home, in a browser view or while hidden", () => {
    expect(readsOwnership({ ...own, tier: "active", remoteHome: true })).toBe(false);
    expect(readsOwnership({ ...own, tier: "active", gateway: true })).toBe(false);
    expect(readsOwnership({ ...own, tier: "active", visible: false })).toBe(false);
  });
});
