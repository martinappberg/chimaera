import { describe, it, expect } from "vitest";
import { projectCopyError } from "./projectCopy";
describe("opening a cloud project here", () => {
  it("never renders arbitrary daemon diagnostics and keeps upgrade refusal explicit", () => {
    expect(projectCopyError(new Error("private credential value"))).not.toContain("credential");
    expect(projectCopyError("project_copy_update_required")).toContain("Update Chimaera");
    expect(projectCopyError("unknown")).toContain("couldn't open here");
  });
  it("speaks of copies and the cloud, never of syncing", () => {
    for (const code of [
      "project_busy",
      "project_checkpoint_pending",
      "project_unavailable",
      "project_folder_missing",
      "unknown",
    ]) {
      expect(projectCopyError(code)).not.toMatch(/sync/i);
    }
  });
});
