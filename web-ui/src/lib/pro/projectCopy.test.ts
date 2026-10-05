import { describe, it, expect } from "vitest";
import { projectCopyError } from "./projectCopy";
describe("opening a synced project here", () => {
  it("never renders arbitrary daemon diagnostics and keeps upgrade refusal explicit", () => {
    expect(projectCopyError(new Error("private credential value"))).not.toContain("credential");
    expect(projectCopyError("project_copy_update_required")).toContain("Update Chimaera");
    expect(projectCopyError("unknown")).toContain("couldn't open here");
  });
});
