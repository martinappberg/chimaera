import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ASLEEP_NOTE, ApiError, isHomeHub, leaveHomeHub, ownerElsewhere, plainError, projectStateNote, reclaimHomeHub } from "./api";
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

describe("a project running elsewhere", () => {
  afterEach(() => { vi.unstubAllGlobals(); sessionStorage.clear(); });

  it("keeps an unknown owner neutral on native, host and project views", () => {
    for (const address of ["http://localhost/", "https://fixture.invalid/app/worker-w1/", "https://fixture.invalid/app/device-d1/", "https://fixture.invalid/workspace/w-one/"]) {
      vi.stubGlobal("location", new URL(address));
      expect(ownerElsewhere()).toBeNull();
      expect(new ApiError(409, "workspace_owned_elsewhere").message).toBe("This project is running somewhere else right now.");
      expect(plainError("read_only")).toBe("This project is running somewhere else right now.");
      expect(plainError("on_other_machine")).toBe("This file is on the other machine and can’t be opened here.");
    }
    // Verified placement still names the actual owner.
    expect(plainError("workspace_owned_elsewhere", "cloud")).toBe("This project is running in the cloud right now.");
    expect(plainError("read_only", "computer")).toBe("This project is running on your computer right now.");
    expect(plainError("on_other_machine", "cloud")).toBe("This file is in the cloud and can’t be opened here.");
    for (const where of ["cloud", "computer", "other", null] as const) {
      expect(plainError("workspace_owned_elsewhere", where)).not.toMatch(/device/);
    }
  });

  it("calls a sleeping cloud machine asleep", () => {
    expect(plainError("worker_asleep")).toBe("The cloud machine is asleep.");
  });
});

describe("a project's state, told apart from a failure", () => {
  it("reads a sleeping or unreachable owner as a quiet note, never an error", () => {
    expect(projectStateNote(new ApiError(409, "worker_asleep"))).toBe(ASLEEP_NOTE);
    expect(ASLEEP_NOTE).toBe("Asleep in the cloud. Send a message to wake it.");
    expect(projectStateNote(new ApiError(503, "project_unavailable"))).toBe("This project isn’t reachable right now.");
    for (const code of ["remote_unavailable", "workspace_scope_changed", "workspace_unavailable"]) {
      expect(projectStateNote(new ApiError(503, code))).toBe("Your project is reconnecting.");
    }
  });

  it("leaves a real failure to its error styling", () => {
    expect(projectStateNote(new ApiError(400, "invalid path"))).toBeNull();
    expect(projectStateNote(new ApiError(403, "outside_project"))).toBeNull();
    expect(projectStateNote(new ApiError(409, "workspace_owned_elsewhere"))).toBeNull();
    expect(projectStateNote(new Error("worker_asleep"))).toBeNull();
    expect(projectStateNote(null)).toBeNull();
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
