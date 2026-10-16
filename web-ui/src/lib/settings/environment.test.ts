import { describe, expect, it, vi } from "vitest";

const api = vi.hoisted(() => vi.fn());
vi.mock("../net/api", () => ({ api, ApiError: class extends Error {} }));

import { baselinesAfterSave, patchEnvironment, planEnvironmentSave } from "./environment";

const saved = { host: "ml bcftools", workspace: "micromamba activate env" };

describe("planEnvironmentSave", () => {
  it("leaves an unchanged scope out so another window's save survives", () => {
    const plan = planEnvironmentSave({ ...saved, workspace: "export FOO=bar" }, saved, "ws1");
    expect(plan?.patch).toEqual({ workspaces: { ws1: { text: "export FOO=bar" } } });
    expect(plan?.patch).not.toHaveProperty("host");
  });

  it("sends an emptied scope as null, deleting it", () => {
    const plan = planEnvironmentSave({ ...saved, host: "  \n" }, saved, "ws1");
    expect(plan?.patch).toEqual({ host: null });
  });

  it("sends both scopes when both changed", () => {
    const plan = planEnvironmentSave({ host: "a", workspace: "" }, saved, "ws1");
    expect(plan?.patch).toEqual({ host: { text: "a" }, workspaces: { ws1: null } });
  });

  it("plans no request when nothing changed", () => {
    expect(planEnvironmentSave(saved, saved, "ws1")).toBeNull();
  });

  it("ignores the workspace editor in a window without a workspace", () => {
    expect(planEnvironmentSave({ ...saved, workspace: "x" }, saved, null)).toBeNull();
  });

  it("keeps edits typed while the save was pending unsaved", () => {
    const plan = planEnvironmentSave({ ...saved, host: "ml samtools" }, saved, "ws1");
    expect(plan).not.toBeNull();
    // The editor moved on before the PATCH answered.
    const editors = { ...saved, host: "ml samtools\nml htslib" };
    const after = baselinesAfterSave(saved, plan!);
    expect(after).toEqual({ ...saved, host: "ml samtools" });
    expect(planEnvironmentSave(editors, after, "ws1")?.patch).toEqual({
      host: { text: "ml samtools\nml htslib" },
    });
  });
});

describe("patchEnvironment", () => {
  it("PATCHes the scoped body as-is, null deletions included", async () => {
    api.mockResolvedValue(new Response(null, { status: 204 }));
    await patchEnvironment({ workspaces: { ws1: null } });
    expect(api).toHaveBeenCalledWith(
      "/environment",
      expect.objectContaining({ method: "PATCH", body: '{"workspaces":{"ws1":null}}' }),
    );
  });
});
