import { beforeEach, describe, expect, it } from "vitest";

import { ApiError, isHomeHub, leaveHomeHub, reclaimHomeHub } from "./api";
import { placementLabel } from "./placement";

describe("daemon connection codes", () => {
  it("never reach the screen as raw wire identifiers", () => {
    for (const code of [
      "project_unavailable",
      "workspace_owned_elsewhere",
      "remote_unavailable",
      "workspace_scope_changed",
      "read_only",
      "worker_asleep",
    ]) {
      const error = new ApiError(409, code);
      expect(error.code).toBe(code);
      expect(error.message).not.toContain("_");
      expect(error.message.length).toBeGreaterThan(0);
    }
    const other = new ApiError(400, "invalid path");
    expect(other.code).toBeNull();
    expect(other.message).toBe("invalid path");
  });

  it("say where a routed session runs, and nothing for a session here", () => {
    expect(placementLabel("here", true)).toBeNull();
    expect(placementLabel(undefined, undefined)).toBeNull();
    const cloud = placementLabel({ remote: "worker-w1" }, true);
    const computer = placementLabel({ remote: "device-d1" }, true);
    expect(cloud).not.toBeNull();
    expect(computer).not.toBeNull();
    expect(cloud).not.toBe(computer);
    expect(placementLabel({ remote: "worker-w1" }, false)).not.toBe(cloud);
  });
});

describe("native Home hub identity", () => {
  beforeEach(() => sessionStorage.clear());

  it("can be reclaimed after a workspace promotion clears it", () => {
    reclaimHomeHub();
    expect(isHomeHub()).toBe(true);

    leaveHomeHub();
    expect(isHomeHub()).toBe(false);

    reclaimHomeHub();
    expect(isHomeHub()).toBe(true);
  });
});
